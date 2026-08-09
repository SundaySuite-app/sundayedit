//! Multi-track COMPOSE engine — the ffmpeg `filter_complex` pipeline that
//! flattens the NLE timeline (media pool + tracks + timeline items) into a
//! single rendered video.
//!
//! The command BUILDER (`build_filter_complex`) is a pure function and is
//! unit-tested exhaustively (mirroring `burnin::build_ffmpeg_args`). Design:
//!   - One `-i` per DISTINCT used `MediaItem` (deduped by media id → input
//!     index), then a base canvas `-f lavfi -i color=black:...` at the last
//!     input index.
//!   - Per video item: `trim`/`setpts`, then the geometric `Transform`
//!     (scale/crop/rotate + opacity), then composited via `overlay` (with an
//!     `enable='between(...)'` time-window) onto the running composite,
//!     chaining LOW track index → HIGH (top). An item carrying a
//!     `transition_in` crossfades via `xfade` instead of a hard overlay.
//!   - Per audio-bearing item: `atrim`/`asetpts`/`adelay`, combined via
//!     `amix=inputs=K:normalize=0`.
//!   - The caption layer is applied LAST: `ass=<escaped sidecar path>`
//!     (produced by `export::write_ass`, written to a unique temp path like
//!     `run_burnin` does), reusing `escape_filter_path`.
//!   - Encoder selection reuses `burnin::encoder_name` / `default_encoder`.
//!
//! The actual spawn (`run_compose`) streams `-progress pipe:1` and honours an
//! `AtomicBool` cancel — copying the `highlight_reel::reel_render_all`
//! spawn_blocking + `window.emit` + managed-control skeleton.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::Emitter;
use ts_rs::TS;

use crate::error::{AppError, AppResult};
use crate::model::{Project, TimelineItem, TimelineItemKind, Transform};
use crate::services::burnin::{
    default_encoder, encoder_name, escape_filter_path, Encoder, VideoCodec,
};
use crate::services::video::{ffmpeg_path, MediaKind};

/// Output settings for a compose render. Mirrors the knobs `BurnInOptions`
/// exposes, but the compose engine always targets fixed output dimensions +
/// frame rate (the timeline is flattened onto that canvas).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/ComposeSettings.ts")]
pub struct ComposeSettings {
    pub width: i32,
    pub height: i32,
    pub fps: f32,
    pub codec: VideoCodec,
    pub encoder: Encoder,
    /// Constant-bitrate hint, in kbps. `None` = encoder default.
    pub bitrate_kbps: Option<i32>,
}

impl Default for ComposeSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30.0,
            codec: VideoCodec::H264,
            encoder: default_encoder(),
            bitrate_kbps: None,
        }
    }
}

/// Format a millisecond time as `ffmpeg` seconds (`{:.3}`).
fn secs(ms: i64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

/// Map a stored transition kind — the ClipInspector picker vocabulary
/// ("fade" / "crossfade" / "dip") — to a name ffmpeg's `xfade` filter actually
/// accepts. "crossfade" and "dip" are friendly UI names, NOT members of the
/// xfade enum; emitted verbatim they abort the whole render with
/// `Unable to parse option value "crossfade" as transition`. Mapping at this
/// seam (rather than renaming the UI options) also heals projects that saved
/// the friendly names. Unrecognized kinds pass through untouched so genuine
/// xfade names (e.g. "wipeleft") in project files keep working.
fn xfade_transition_name(kind: &str) -> &str {
    match kind {
        "crossfade" => "dissolve",
        "dip" => "fadeblack",
        other => other,
    }
}

/// Distinct media ids referenced by timeline items that actually CONTRIBUTE
/// to the render (item enabled + owning track visible/audible), in first-seen
/// order → `(id, input_index)` map. Pure.
fn used_media(project: &Project) -> Vec<String> {
    let any_solo = any_track_solo(project);
    let mut seen: Vec<String> = Vec::new();
    for it in &project.timeline_items {
        if !item_contributes(project, it, any_solo) {
            continue;
        }
        if let Some(mid) = &it.source_media_id {
            if !seen.iter().any(|s| s == mid) && project.media.iter().any(|m| &m.id == mid) {
                seen.push(mid.clone());
            }
        }
    }
    seen
}

/// The stacking index of the track an item sits on (0 = bottom). Missing track
/// sorts last. Used to composite LOW → HIGH.
fn track_index(project: &Project, item: &TimelineItem) -> i32 {
    project
        .tracks
        .iter()
        .find(|t| t.id == item.track_id)
        .map(|t| t.index)
        .unwrap_or(i32::MAX)
}

/// Is this item a *visual* clip (references a video-kind media)?
fn is_visual(project: &Project, item: &TimelineItem) -> bool {
    item.source_media_id
        .as_ref()
        .and_then(|mid| project.media.iter().find(|m| &m.id == mid))
        .map(|m| matches!(m.kind, MediaKind::Video))
        .unwrap_or(false)
}

/// Does this item carry an audio stream (its source media `has_audio`)?
fn has_audio(project: &Project, item: &TimelineItem) -> bool {
    item.source_media_id
        .as_ref()
        .and_then(|mid| project.media.iter().find(|m| &m.id == mid))
        .map(|m| m.has_audio)
        .unwrap_or(false)
}

/// The `Track` an item sits on, if it resolves.
fn track_of<'a>(project: &'a Project, item: &TimelineItem) -> Option<&'a crate::model::Track> {
    project.tracks.iter().find(|t| t.id == item.track_id)
}

/// Is any track soloed? While a solo is active, only soloed tracks are
/// audible (DAW convention — mirrors the Timeline track-header S button).
fn any_track_solo(project: &Project) -> bool {
    project.tracks.iter().any(|t| t.solo)
}

/// Preview parity: an item is VISIBLE only when its owning track is enabled
/// (`previewMap.activeVideoItem` skips `enabled === false` tracks). Items with
/// an unresolvable track keep current behaviour (treated as visible).
fn track_visible(project: &Project, item: &TimelineItem) -> bool {
    track_of(project, item).is_none_or(|t| t.enabled)
}

/// Preview parity: an item is AUDIBLE only when its owning track is enabled,
/// not muted, and — while any track is soloed — itself soloed. Items with an
/// unresolvable track keep current behaviour (treated as audible).
fn track_audible(project: &Project, item: &TimelineItem, any_solo: bool) -> bool {
    track_of(project, item).is_none_or(|t| t.enabled && !t.muted && (!any_solo || t.solo))
}

/// Will this item contribute ANY stream to the render, given both its own
/// `enabled` flag and its track's `enabled`/`muted`/`solo` state? Used to
/// avoid feeding ffmpeg `-i` inputs that no filter node consumes.
fn item_contributes(project: &Project, item: &TimelineItem, any_solo: bool) -> bool {
    item.enabled
        && ((is_visual(project, item) && track_visible(project, item))
            || (has_audio(project, item) && track_audible(project, item, any_solo)))
}

/// The total timeline duration in ms — the max of every item's timeline end,
/// falling back to the project's scalar video duration.
pub fn timeline_duration_ms(project: &Project) -> i64 {
    project
        .timeline_items
        .iter()
        .map(|it| it.timeline_end_ms())
        .max()
        .filter(|&d| d > 0)
        .unwrap_or(project.video_duration_ms.max(0))
}

/// The per-item geometric filter chain from its `Transform` (fractions of the
/// output frame). Appends to `chain` in the order scale → crop → rotate →
/// opacity, so the output stays resolution-independent.
fn transform_filters(t: &Transform, chain: &mut Vec<String>) {
    if (t.scale - 1.0).abs() > f32::EPSILON && t.scale > 0.0 {
        chain.push(format!("scale=iw*{s}:ih*{s}", s = t.scale));
    }
    if let Some(c) = &t.crop {
        chain.push(format!(
            "crop=iw*{w}:ih*{h}:iw*{x}:ih*{y}",
            w = c.width,
            h = c.height,
            x = c.x,
            y = c.y
        ));
    }
    if t.rotation_deg.abs() > f32::EPSILON {
        chain.push(format!("rotate={deg}*PI/180", deg = t.rotation_deg));
    }
    if t.opacity < 1.0 {
        chain.push(format!(
            "format=rgba,colorchannelmixer=aa={a}",
            a = t.opacity
        ));
    }
}

/// A "simple" timeline is one the existing single-track burn-in can render
/// exactly: no visual/audio timeline items to composite beyond, at most, the
/// primary video placed as ONE pristine full-length clip — the shape
/// `Project::backfill_default_timeline` synthesizes on import/load. Such a
/// project delegates to `burnin::render` (hardware encoding + audio
/// passthrough, battle-tested).
pub fn is_simple_timeline(project: &Project) -> bool {
    let av_items: Vec<&TimelineItem> = project
        .timeline_items
        .iter()
        .filter(|it| is_visual(project, it) || has_audio(project, it))
        .collect();
    match av_items.as_slice() {
        [] => true,
        // The burn-in shortcut renders the primary video with full audio
        // passthrough, so it is only exact while the owning track's
        // enabled/muted/solo state is a no-op — otherwise the composite path
        // must apply the track flags (export/preview parity).
        [only] => {
            let any_solo = any_track_solo(project);
            is_pristine_primary_item(project, only)
                && track_visible(project, only)
                && track_audible(project, only, any_solo)
        }
        _ => false,
    }
}

/// Is `item` the backfilled baseline clip — the ENTIRE primary video placed at
/// timeline 0 with no trim/speed/transform/effects/transition? Rendering that
/// through burn-in is identical to compositing it, so it keeps the fast path.
fn is_pristine_primary_item(project: &Project, item: &TimelineItem) -> bool {
    let Some(media) = item
        .source_media_id
        .as_ref()
        .and_then(|mid| project.media.iter().find(|m| &m.id == mid))
    else {
        return false;
    };
    media.path == project.video_path
        && media.content_hash == project.video_content_hash
        && item.kind == TimelineItemKind::Av
        && item.enabled
        && item.in_ms == 0
        && item.out_ms == media.duration_ms
        && item.timeline_start_ms == 0
        && (item.speed - 1.0).abs() < f32::EPSILON
        && item.transform == Transform::default()
        && item.effects.iter().all(|e| !e.enabled)
        && item.transition_in.is_none()
}

/// Build the FULL ffmpeg argument vector for a compose render. Pure — no IO.
/// `ass_file` is the (already-written) caption sidecar; `None` skips the
/// caption layer. This is the unit-tested heart of the compose path.
pub fn build_filter_complex(
    project: &Project,
    settings: &ComposeSettings,
    ass_file: Option<&str>,
    output: &str,
) -> Vec<String> {
    // H.264/`yuv420p` requires EVEN output dimensions. Odd caller-supplied
    // geometry (projects imported from odd-dimension screen/web captures probe
    // odd, and the frontend derives its default settings from those numbers)
    // otherwise splits the graph: the lavfi canvas silently rounds itself down
    // while the xfade branch scales to the raw odd size — the mismatch aborts
    // the render ("Failed to inject frame into filter network"), and the plain
    // path emits one pixel short of the requested frame. Sanitize at the seam
    // (same `even_up` the proxy path already applies) so every caller composes
    // at a valid geometry.
    let settings = &ComposeSettings {
        width: even_up(settings.width),
        height: even_up(settings.height),
        ..settings.clone()
    };

    let media_ids = used_media(project);
    let input_index = |mid: &str| media_ids.iter().position(|m| m == mid).unwrap();
    let canvas_idx = media_ids.len();

    let total_ms = timeline_duration_ms(project);
    let any_solo = any_track_solo(project);

    // ── Video items, composited LOW track → HIGH ────────────────────────────
    // Track parity with the live preview: a clip renders only when BOTH its
    // own `enabled` flag and its owning track's `enabled` flag are set
    // (previewMap skips disabled tracks; export must agree).
    let mut video_items: Vec<&TimelineItem> = project
        .timeline_items
        .iter()
        .filter(|it| it.enabled && track_visible(project, it) && is_visual(project, it))
        .collect();
    video_items.sort_by(|a, b| {
        track_index(project, a)
            .cmp(&track_index(project, b))
            .then(a.timeline_start_ms.cmp(&b.timeline_start_ms))
    });

    let mut nodes: Vec<String> = Vec::new();

    // Process each visual item into a `[pv{n}]` stream.
    for (n, it) in video_items.iter().enumerate() {
        let src = input_index(it.source_media_id.as_ref().unwrap());
        // Shift the clip's PTS to its TIMELINE position (mirroring `adelay` on
        // the audio side). Without the shift, `overlay` pairs frames by raw
        // timestamp: a clip starting at t>0 shows the wrong source region and
        // freezes on its last frame once the 0-based stream runs out — the
        // `enable=between(...)` window hides the misalignment but not the
        // freeze. (An item consumed by `xfade` re-zeroes PTS in its normalise
        // chain, so the shift is harmless there.)
        let setpts = if it.timeline_start_ms > 0 {
            format!("setpts=PTS-STARTPTS+{}/TB", secs(it.timeline_start_ms))
        } else {
            "setpts=PTS-STARTPTS".to_string()
        };
        let mut chain: Vec<String> = vec![
            format!("trim=start={}:end={}", secs(it.in_ms), secs(it.out_ms)),
            setpts,
        ];
        transform_filters(&it.transform, &mut chain);
        nodes.push(format!("[{src}:v]{}[pv{n}]", chain.join(",")));
    }

    // Fold the processed streams onto the base canvas, low → high.
    let mut prev = format!("[{canvas_idx}:v]");
    for (n, it) in video_items.iter().enumerate() {
        let out = format!("[cx{n}]");
        // A transition only makes sense against a preceding sibling on the SAME
        // track (the boundary it crossfades over).
        let same_track_prev = n > 0 && video_items[n - 1].track_id == it.track_id;
        if let (Some(tr), true) = (&it.transition_in, same_track_prev) {
            let prev_end = video_items[n - 1].timeline_end_ms();
            let offset = (prev_end - tr.duration_ms).max(0);
            // `xfade` rejects the blend unless BOTH branches share size, pixel
            // format, SAR, frame rate and timebase — otherwise ffmpeg aborts with
            // "Failed to inject frame into filter network: Invalid argument".
            // Normalise the running composite and the incoming clip before it.
            let norm = format!(
                "fps={fps},format=yuv420p,setsar=1,settb=AVTB,setpts=PTS-STARTPTS",
                fps = settings.fps,
            );
            nodes.push(format!("{prev}{norm}[xa{n}]"));
            nodes.push(format!(
                "[pv{n}]scale={w}:{h},{norm}[xb{n}]",
                w = settings.width,
                h = settings.height,
            ));
            nodes.push(format!(
                "[xa{n}][xb{n}]xfade=transition={kind}:duration={dur}:offset={off}{out}",
                kind = xfade_transition_name(&tr.kind),
                dur = secs(tr.duration_ms),
                off = secs(offset),
            ));
        } else {
            let x = (settings.width as f32 * it.transform.x).round() as i64;
            let y = (settings.height as f32 * it.transform.y).round() as i64;
            nodes.push(format!(
                "{prev}[pv{n}]overlay={x}:{y}:enable='between(t,{a},{b})'{out}",
                a = secs(it.timeline_start_ms),
                b = secs(it.timeline_end_ms()),
            ));
        }
        prev = out;
    }

    // ── Audio items → amix ──────────────────────────────────────────────────
    // Track parity with the preview mute/solo buttons: a clip's audio reaches
    // the mix only when its track is enabled, unmuted and — while any track is
    // soloed — itself soloed. The enumeration index `n` stays tied to the
    // item's position in the full audio-bearing list (skips do not renumber),
    // so `[pa{n}]` labels are stable regardless of flag state.
    let audio_items: Vec<&TimelineItem> = project
        .timeline_items
        .iter()
        .filter(|it| it.enabled && has_audio(project, it))
        .collect();
    let mut audio_labels: Vec<String> = Vec::new();
    for (n, it) in audio_items.iter().enumerate() {
        if !track_audible(project, it, any_solo) {
            continue;
        }
        let src = input_index(it.source_media_id.as_ref().unwrap());
        let delay = it.timeline_start_ms.max(0);
        nodes.push(format!(
            "[{src}:a]atrim=start={s}:end={e},asetpts=PTS-STARTPTS,adelay={d}|{d}[pa{n}]",
            s = secs(it.in_ms),
            e = secs(it.out_ms),
            d = delay,
        ));
        audio_labels.push(format!("[pa{n}]"));
    }
    let audio_out = if audio_labels.len() >= 2 {
        nodes.push(format!(
            "{}amix=inputs={}:normalize=0[aout]",
            audio_labels.join(""),
            audio_labels.len()
        ));
        Some("[aout]".to_string())
    } else if audio_labels.len() == 1 {
        nodes.push(format!("{}anull[aout]", audio_labels[0]));
        Some("[aout]".to_string())
    } else {
        None
    };

    // ── Caption layer LAST: ass overlay on the video composite ──────────────
    let video_out = if let Some(ass) = ass_file {
        // Placed last in the graph so `ass=` is the final filter node.
        nodes.push(format!("{prev}ass={}[vout]", escape_filter_path(ass)));
        "[vout]".to_string()
    } else if video_items.is_empty() {
        // No visual items and no caption layer: `prev` is still the RAW canvas
        // input label. `-map` treats a bracketed name as a filtergraph OUTPUT
        // label ("Output with label '{n}:v' does not exist"), so map the input
        // pad plainly instead — e.g. an audio-only timeline over black.
        format!("{canvas_idx}:v")
    } else {
        prev.clone()
    };

    // ── Assemble the argument vector ────────────────────────────────────────
    let mut args: Vec<String> = Vec::new();
    args.push("-y".into());

    for mid in &media_ids {
        let path = project
            .media
            .iter()
            .find(|m| &m.id == mid)
            .map(|m| m.path.clone())
            .unwrap_or_default();
        args.push("-i".into());
        args.push(path);
    }

    // Base canvas at the last input index.
    args.push("-f".into());
    args.push("lavfi".into());
    args.push("-i".into());
    args.push(format!(
        "color=black:s={w}x{h}:r={fps}:d={d}",
        w = settings.width,
        h = settings.height,
        fps = settings.fps,
        d = secs(total_ms),
    ));

    // An EMPTY `-filter_complex ""` makes ffmpeg abort — reachable only when
    // the builder is called directly on a project with no visual/audio items
    // (e.g. caption-track-only without a sidecar; `run_compose` routes such
    // timelines to the burn-in path). Defense-in-depth: skip the flag.
    if !nodes.is_empty() {
        args.push("-filter_complex".into());
        args.push(nodes.join(";"));
    }

    args.push("-map".into());
    args.push(video_out);
    if let Some(a) = &audio_out {
        args.push("-map".into());
        args.push(a.clone());
    }

    args.push("-c:v".into());
    args.push(encoder_name(settings.codec, settings.encoder).into());
    if let Some(kbps) = settings.bitrate_kbps {
        args.push("-b:v".into());
        args.push(format!("{}k", kbps));
    }
    args.push("-r".into());
    args.push(format!("{}", settings.fps));
    args.push("-pix_fmt".into());
    args.push("yuv420p".into());

    if audio_out.is_some() {
        args.push("-c:a".into());
        args.push("aac".into());
    } else {
        args.push("-an".into());
    }

    args.push(output.into());
    args
}

/// Round `n` up to the nearest even integer (H.264/`yuv420p` requires even
/// dimensions). Never returns below 2.
fn even_up(n: i32) -> i32 {
    let n = n.max(2);
    if n % 2 == 0 {
        n
    } else {
        n + 1
    }
}

/// Derive a LOW-RES fast-render profile from a project — the "preview-render
/// proxy" fallback (ADR-009): height capped at 480 (keeping the primary
/// video's aspect ratio, even dims), fps capped at 30, always the CPU
/// (`libx264`) encoder at a low bitrate. Pure.
pub fn proxy_settings(project: &Project) -> ComposeSettings {
    let src_w = project.video_width.max(1);
    let src_h = project.video_height.max(1);

    // Cap height at 480 but never upscale past the source.
    let height = src_h.min(480);
    let width = (src_w as f64 * height as f64 / src_h as f64).round() as i32;

    let fps = if project.video_fps > 0.0 {
        project.video_fps.min(30.0)
    } else {
        30.0
    };

    ComposeSettings {
        width: even_up(width),
        height: even_up(height),
        fps,
        codec: VideoCodec::H264,
        encoder: Encoder::Cpu,
        bitrate_kbps: Some(1200),
    }
}

/// Build the ffmpeg argument vector for a fast PROXY render: the full compose
/// graph plus `-preset ultrafast` (valid because a proxy always uses the CPU
/// `libx264` encoder — see `proxy_settings`). Pure — no IO.
pub fn build_proxy_args(
    project: &Project,
    settings: &ComposeSettings,
    ass_file: Option<&str>,
    output: &str,
) -> Vec<String> {
    let mut args = build_filter_complex(project, settings, ass_file, output);
    // Insert `-preset ultrafast` just before the trailing output path so the
    // x264 encoder runs at its lowest-latency profile.
    let out = args
        .pop()
        .expect("build_filter_complex always ends with output");
    args.push("-preset".into());
    args.push("ultrafast".into());
    args.push(out);
    args
}

/// Streamed to the UI as the compose render advances. Mirrors the reel/download
/// progress shape: a completed fraction over the total timeline duration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/ComposeProgress.ts")]
pub struct ComposeProgress {
    /// Milliseconds of output rendered so far (from ffmpeg `out_time_ms`).
    #[ts(type = "number")]
    pub out_ms: i64,
    /// Total timeline duration in ms.
    #[ts(type = "number")]
    pub total_ms: i64,
    /// 0..1, clamped; `None` when total is 0.
    pub fraction: Option<f32>,
    /// Latest encoded frame count, if ffmpeg reported it.
    #[ts(type = "number")]
    pub frame: i64,
    /// True on the final tick.
    pub done: bool,
}

/// Completion fraction, clamped to `[0, 1]`, `None` when total is 0. Pure.
pub fn compose_fraction(out_ms: i64, total_ms: i64) -> Option<f32> {
    if total_ms <= 0 {
        None
    } else {
        Some((out_ms as f32 / total_ms as f32).clamp(0.0, 1.0))
    }
}

/// Pick a temp `.ass` sidecar next to `output` that does not clobber an
/// existing file (same policy as `burnin::run_burnin`).
fn unique_sidecar_path(output: &Path) -> PathBuf {
    let candidate = output.with_extension("compose.ass");
    if !candidate.exists() {
        return candidate;
    }
    let stem = output
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "compose".to_string());
    let dir = output.parent().unwrap_or_else(|| Path::new("."));
    for n in 0..10_000 {
        let p = dir.join(format!("{stem}.compose.{n}.ass"));
        if !p.exists() {
            return p;
        }
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!("{stem}.compose.{ts}.ass"))
}

/// Parse `key=value` progress lines from ffmpeg's `-progress pipe:1` output.
/// Returns `(out_ms, frame, done)` extracted from a batch of lines. Pure.
fn parse_progress_line(line: &str, out_ms: &mut i64, frame: &mut i64, done: &mut bool) {
    let line = line.trim();
    if let Some(v) = line.strip_prefix("out_time_ms=") {
        // ffmpeg reports microseconds in `out_time_ms` (historical misnomer).
        if let Ok(us) = v.trim().parse::<i64>() {
            *out_ms = us / 1000;
        }
    } else if let Some(v) = line.strip_prefix("out_time_us=") {
        if let Ok(us) = v.trim().parse::<i64>() {
            *out_ms = us / 1000;
        }
    } else if let Some(v) = line.strip_prefix("frame=") {
        if let Ok(f) = v.trim().parse::<i64>() {
            *frame = f;
        }
    } else if let Some(v) = line.strip_prefix("progress=") {
        if v.trim() == "end" {
            *done = true;
        }
    }
}

/// The SIMPLE-PATH render: burn-in argument builder (hardware encoding +
/// audio passthrough), but spawned with `-progress pipe:1` so it streams
/// `compose-render-progress` and polls `cancel` — exactly like the composite
/// path. `burnin::render` blocks on `Command::status()` with no progress and
/// no cancel, which made the DEFAULT export of every fresh import show a
/// 0%-forever bar and a Cancel button that did nothing.
fn run_simple_compose(
    window: &tauri::Window,
    project: &Project,
    output: &Path,
    opts: &crate::services::burnin::BurnInOptions,
    cancel: Arc<AtomicBool>,
) -> AppResult<()> {
    if !Path::new(&project.video_path).exists() {
        return Err(AppError::VideoMissing(project.video_path.clone()));
    }
    let total_ms = timeline_duration_ms(project);

    // Write the caption sidecar (reused verbatim from export::write_ass).
    let ass = crate::services::export::write_ass(project);
    let ass_path = unique_sidecar_path(output);
    std::fs::write(&ass_path, ass)?;

    let args = crate::services::burnin::build_ffmpeg_args(
        &project.video_path,
        &ass_path.to_string_lossy(),
        &output.to_string_lossy(),
        opts,
    );

    // Ensure output dir exists (best-effort).
    if let Some(parent) = output.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut child = Command::new(ffmpeg_path())
        .args(&args)
        .args(["-progress", "pipe:1", "-nostats"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            let _ = std::fs::remove_file(&ass_path);
            AppError::Internal(format!(
                "failed to launch ffmpeg for compose: {e}. Is ffmpeg installed / bundled?"
            ))
        })?;

    // Initial 0% tick.
    let _ = window.emit(
        "compose-render-progress",
        &ComposeProgress {
            out_ms: 0,
            total_ms,
            fraction: compose_fraction(0, total_ms),
            frame: 0,
            done: false,
        },
    );

    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        let mut out_ms = 0i64;
        let mut frame = 0i64;
        for line in reader.lines().map_while(Result::ok) {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                break;
            }
            let mut done = false;
            parse_progress_line(&line, &mut out_ms, &mut frame, &mut done);
            // A `progress=` line closes each stats block — emit once per block.
            if line.trim_start().starts_with("progress=") {
                let _ = window.emit(
                    "compose-render-progress",
                    &ComposeProgress {
                        out_ms,
                        total_ms,
                        fraction: compose_fraction(out_ms, total_ms),
                        frame,
                        done,
                    },
                );
            }
        }
    }

    let status = child.wait().map_err(|e| {
        let _ = std::fs::remove_file(&ass_path);
        AppError::Internal(format!("ffmpeg compose wait failed: {e}"))
    })?;

    let _ = std::fs::remove_file(&ass_path);

    if cancel.load(Ordering::Relaxed) {
        return Err(AppError::Internal("compose render cancelled".into()));
    }

    // Final tick.
    let _ = window.emit(
        "compose-render-progress",
        &ComposeProgress {
            out_ms: total_ms,
            total_ms,
            fraction: compose_fraction(total_ms, total_ms),
            frame: 0,
            done: true,
        },
    );

    if !status.success() {
        return Err(AppError::Internal(
            "ffmpeg compose failed. If your machine lacks the chosen hardware \
             encoder, retry with the CPU encoder."
                .to_string(),
        ));
    }
    Ok(())
}

/// Render the whole timeline to `output`. Takes the SIMPLE-PATH shortcut
/// (the burn-in argument builder via `run_simple_compose`, which keeps
/// progress + cancel) when the timeline holds no extra visual/audio items;
/// otherwise spawns the `filter_complex` pipeline with `-progress pipe:1`,
/// streams `compose-render-progress`, and honours `cancel`.
pub fn run_compose(
    window: &tauri::Window,
    project: &Project,
    output: &Path,
    settings: &ComposeSettings,
    cancel: Arc<AtomicBool>,
) -> AppResult<()> {
    // Simple path: only the primary video + caption track(s) — the
    // single-track burn-in ARGUMENT BUILDER renders this exactly, with
    // hardware encoding + audio passthrough. Cheaper and battle-tested. The
    // spawn still goes through the streaming skeleton below so the `window`
    // gets progress events and the `cancel` flag is honoured — `burnin::render`
    // itself can do neither, and this is the DEFAULT path of every fresh
    // import (see tests/compose_simple_path_contract.rs).
    if is_simple_timeline(project) {
        let opts = crate::services::burnin::BurnInOptions {
            codec: settings.codec,
            encoder: settings.encoder,
            // Same even-dimension guard the composite path applies inside
            // `build_filter_complex` — libx264/yuv420p rejects odd frames.
            out_width: Some(even_up(settings.width)),
            out_height: Some(even_up(settings.height)),
            bitrate_kbps: settings.bitrate_kbps,
            clip_start_ms: None,
            clip_end_ms: None,
        };
        return run_simple_compose(window, project, output, &opts, cancel);
    }

    let total_ms = timeline_duration_ms(project);

    // Write the caption sidecar (reused verbatim from export::write_ass).
    let ass = crate::services::export::write_ass(project);
    let ass_path = unique_sidecar_path(output);
    std::fs::write(&ass_path, ass)?;

    let ass_str = ass_path.to_string_lossy().into_owned();
    let ass_ref = if project.captions.is_empty() {
        None
    } else {
        Some(ass_str.as_str())
    };
    let args = build_filter_complex(project, settings, ass_ref, &output.to_string_lossy());

    // Ensure output dir exists (best-effort).
    if let Some(parent) = output.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut child = Command::new(ffmpeg_path())
        .args(&args)
        .args(["-progress", "pipe:1", "-nostats"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            let _ = std::fs::remove_file(&ass_path);
            AppError::Internal(format!(
                "failed to launch ffmpeg for compose: {e}. Is ffmpeg installed / bundled?"
            ))
        })?;

    // Initial 0% tick.
    let _ = window.emit(
        "compose-render-progress",
        &ComposeProgress {
            out_ms: 0,
            total_ms,
            fraction: compose_fraction(0, total_ms),
            frame: 0,
            done: false,
        },
    );

    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        let mut out_ms = 0i64;
        let mut frame = 0i64;
        for line in reader.lines().map_while(Result::ok) {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                break;
            }
            let mut done = false;
            parse_progress_line(&line, &mut out_ms, &mut frame, &mut done);
            // A `progress=` line closes each stats block — emit once per block.
            if line.trim_start().starts_with("progress=") {
                let _ = window.emit(
                    "compose-render-progress",
                    &ComposeProgress {
                        out_ms,
                        total_ms,
                        fraction: compose_fraction(out_ms, total_ms),
                        frame,
                        done,
                    },
                );
            }
        }
    }

    let status = child.wait().map_err(|e| {
        let _ = std::fs::remove_file(&ass_path);
        AppError::Internal(format!("ffmpeg compose wait failed: {e}"))
    })?;

    let _ = std::fs::remove_file(&ass_path);

    if cancel.load(Ordering::Relaxed) {
        return Err(AppError::Internal("compose render cancelled".into()));
    }

    // Final tick.
    let _ = window.emit(
        "compose-render-progress",
        &ComposeProgress {
            out_ms: total_ms,
            total_ms,
            fraction: compose_fraction(total_ms, total_ms),
            frame: 0,
            done: true,
        },
    );

    if !status.success() {
        return Err(AppError::Internal(
            "ffmpeg compose failed. If your machine lacks the chosen hardware \
             encoder, retry with the CPU encoder."
                .to_string(),
        ));
    }
    Ok(())
}

/// Render a FAST LOW-RES proxy of the whole timeline to `output` — the
/// preview-render fallback used while a real-time WebCodecs compositor is
/// unavailable (ADR-009). Derives its settings via `proxy_settings`, always
/// runs the `filter_complex` proxy path (never the burn-in shortcut, so the
/// low-res composite is exact), streams `compose-proxy-progress`, and honours
/// `cancel`. Mirrors `run_compose`'s spawn + progress skeleton.
pub fn run_compose_proxy(
    window: &tauri::Window,
    project: &Project,
    output: &Path,
    cancel: Arc<AtomicBool>,
) -> AppResult<()> {
    let settings = proxy_settings(project);
    let total_ms = timeline_duration_ms(project);

    // Write the caption sidecar (reused verbatim from export::write_ass).
    let ass = crate::services::export::write_ass(project);
    let ass_path = unique_sidecar_path(output);
    std::fs::write(&ass_path, ass)?;

    let ass_str = ass_path.to_string_lossy().into_owned();
    let ass_ref = if project.captions.is_empty() {
        None
    } else {
        Some(ass_str.as_str())
    };
    let args = build_proxy_args(project, &settings, ass_ref, &output.to_string_lossy());

    // Ensure output dir exists (best-effort).
    if let Some(parent) = output.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut child = Command::new(ffmpeg_path())
        .args(&args)
        .args(["-progress", "pipe:1", "-nostats"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            let _ = std::fs::remove_file(&ass_path);
            AppError::Internal(format!(
                "failed to launch ffmpeg for proxy compose: {e}. Is ffmpeg installed / bundled?"
            ))
        })?;

    // Initial 0% tick.
    let _ = window.emit(
        "compose-proxy-progress",
        &ComposeProgress {
            out_ms: 0,
            total_ms,
            fraction: compose_fraction(0, total_ms),
            frame: 0,
            done: false,
        },
    );

    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        let mut out_ms = 0i64;
        let mut frame = 0i64;
        for line in reader.lines().map_while(Result::ok) {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                break;
            }
            let mut done = false;
            parse_progress_line(&line, &mut out_ms, &mut frame, &mut done);
            if line.trim_start().starts_with("progress=") {
                let _ = window.emit(
                    "compose-proxy-progress",
                    &ComposeProgress {
                        out_ms,
                        total_ms,
                        fraction: compose_fraction(out_ms, total_ms),
                        frame,
                        done,
                    },
                );
            }
        }
    }

    let status = child.wait().map_err(|e| {
        let _ = std::fs::remove_file(&ass_path);
        AppError::Internal(format!("ffmpeg proxy compose wait failed: {e}"))
    })?;

    let _ = std::fs::remove_file(&ass_path);

    if cancel.load(Ordering::Relaxed) {
        return Err(AppError::Internal("proxy compose render cancelled".into()));
    }

    // Final tick.
    let _ = window.emit(
        "compose-proxy-progress",
        &ComposeProgress {
            out_ms: total_ms,
            total_ms,
            fraction: compose_fraction(total_ms, total_ms),
            frame: 0,
            done: true,
        },
    );

    if !status.success() {
        return Err(AppError::Internal(
            "ffmpeg proxy compose failed.".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Caption, MediaItem, Style, TimelineItemKind, Track, TrackKind, Transition, Word,
    };

    fn settings() -> ComposeSettings {
        ComposeSettings {
            width: 1920,
            height: 1080,
            fps: 30.0,
            codec: VideoCodec::H264,
            encoder: Encoder::Cpu,
            bitrate_kbps: None,
        }
    }

    fn media(id: &str, path: &str, audio: bool) -> MediaItem {
        MediaItem {
            id: id.into(),
            path: path.into(),
            content_hash: "h".into(),
            kind: MediaKind::Video,
            duration_ms: 60_000,
            width: 1920,
            height: 1080,
            fps: 30.0,
            has_audio: audio,
            audio_wav_path: None,
            original_filename: format!("{id}.mp4"),
            added_at: 0,
        }
    }

    fn audio_media(id: &str, path: &str) -> MediaItem {
        MediaItem {
            kind: MediaKind::AudioOnly,
            has_audio: true,
            ..media(id, path, true)
        }
    }

    fn track(id: &str, kind: TrackKind, index: i32) -> Track {
        Track {
            id: id.into(),
            kind,
            name: id.into(),
            index,
            enabled: true,
            locked: false,
            muted: false,
            solo: false,
        }
    }

    fn item(
        id: &str,
        track_id: &str,
        media_id: &str,
        start: i64,
        in_ms: i64,
        out_ms: i64,
    ) -> TimelineItem {
        TimelineItem {
            id: id.into(),
            track_id: track_id.into(),
            kind: TimelineItemKind::Av,
            source_media_id: Some(media_id.into()),
            in_ms,
            out_ms,
            timeline_start_ms: start,
            speed: 1.0,
            transform: Transform::default(),
            effects: vec![],
            transition_in: None,
            text: None,
            enabled: true,
            locked: false,
        }
    }

    fn caption(id: &str, start: i64, end: i64) -> Caption {
        Caption {
            id: id.into(),
            start_ms: start,
            end_ms: end,
            words: vec![Word::new("ord", start, end, 90.0)],
            speaker_id: None,
            style_id: None,
            notes: None,
            ai_generated: true,
            last_edited_at: 0,
            track_id: None,
        }
    }

    fn project(
        media: Vec<MediaItem>,
        tracks: Vec<Track>,
        items: Vec<TimelineItem>,
        captions: Vec<Caption>,
    ) -> Project {
        Project {
            id: "p".into(),
            name: "t".into(),
            video_path: "/x.mp4".into(),
            video_content_hash: "h".into(),
            video_duration_ms: 60_000,
            video_width: 1920,
            video_height: 1080,
            video_fps: 30.0,
            audio_wav_path: None,
            language: "no".into(),
            default_style: Style::broadcast_news(),
            context_description: None,
            captions,
            speakers: vec![],
            glossary: vec![],
            clips: vec![],
            talk_summary: None,
            export_config: crate::model::ExportConfig::default(),
            project_meta: crate::model::ProjectMeta::default(),
            created_at: 0,
            updated_at: 0,
            media,
            tracks,
            timeline_items: items,
        }
    }

    fn fc(args: &[String]) -> String {
        let i = args.iter().position(|a| a == "-filter_complex").unwrap();
        args[i + 1].clone()
    }

    #[test]
    fn trim_and_setpts_per_video_item() {
        let p = project(
            vec![media("m1", "/a.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 1000, 5000)],
            vec![],
        );
        let args = build_filter_complex(&p, &settings(), None, "out.mp4");
        let g = fc(&args);
        assert!(g.contains("trim=start=1.000:end=5.000"), "got {g}");
        assert!(g.contains("setpts=PTS-STARTPTS"), "got {g}");
    }

    #[test]
    fn input_dedupe_one_i_per_distinct_media_plus_canvas() {
        // Two items reference the SAME media → one media `-i` + the canvas `-i`.
        let p = project(
            vec![media("m1", "/a.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![
                item("i0", "v1", "m1", 0, 0, 2000),
                item("i1", "v1", "m1", 2000, 2000, 4000),
            ],
            vec![],
        );
        let args = build_filter_complex(&p, &settings(), None, "out.mp4");
        let inputs = args.iter().filter(|a| *a == "-i").count();
        assert_eq!(inputs, 2, "one deduped media input + one canvas input");
        // The canvas is a lavfi color source.
        assert!(args
            .iter()
            .any(|a| a.starts_with("color=black:s=1920x1080")));
    }

    #[test]
    fn two_distinct_media_get_two_inputs() {
        let p = project(
            vec![media("m1", "/a.mp4", false), media("m2", "/b.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![
                item("i0", "v1", "m1", 0, 0, 2000),
                item("i1", "v1", "m2", 2000, 0, 2000),
            ],
            vec![],
        );
        let args = build_filter_complex(&p, &settings(), None, "out.mp4");
        let inputs = args.iter().filter(|a| *a == "-i").count();
        assert_eq!(inputs, 3, "two media inputs + canvas");
    }

    #[test]
    fn overlay_orders_low_track_to_high() {
        // t1 (index 0, bottom) then t2 (index 1, top). The high track must be
        // overlaid LAST (its `[pv]` label consumed after the low track's).
        let p = project(
            vec![media("m1", "/a.mp4", false), media("m2", "/b.mp4", false)],
            vec![
                track("t2", TrackKind::Overlay, 1),
                track("t1", TrackKind::Video, 0),
            ],
            vec![
                item("hi", "t2", "m2", 0, 0, 5000),
                item("lo", "t1", "m1", 0, 0, 5000),
            ],
            vec![],
        );
        let args = build_filter_complex(&p, &settings(), None, "out.mp4");
        let g = fc(&args);
        // Low track is pv0 (overlaid first, onto canvas), high track is pv1.
        let first_overlay = g.find("[pv0]overlay").expect("pv0 overlaid");
        let second_overlay = g.find("[pv1]overlay").expect("pv1 overlaid");
        assert!(
            first_overlay < second_overlay,
            "low track must composite before high track: {g}"
        );
        // The first overlay builds on the canvas input.
        assert!(g.contains(&format!("[{}:v][pv0]overlay", 2)), "got {g}");
    }

    #[test]
    fn transition_uses_xfade_with_offset() {
        // Two sequential clips on the same track; the second carries a
        // crossfade transition → xfade at offset = prev_end - duration.
        let mut second = item("i1", "v1", "m1", 4000, 0, 4000); // ends 8000
        second.transition_in = Some(Transition {
            kind: "fade".into(),
            duration_ms: 1000,
        });
        let p = project(
            vec![media("m1", "/a.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 0, 4000), second], // first ends 4000
            vec![],
        );
        let args = build_filter_complex(&p, &settings(), None, "out.mp4");
        let g = fc(&args);
        // offset = prev_end (4000) - duration (1000) = 3000ms = 3.000s
        assert!(
            g.contains("xfade=transition=fade:duration=1.000:offset=3.000"),
            "got {g}"
        );
    }

    /// Regression (seam-xfade-transition-vocabulary): the ClipInspector picker
    /// offers "crossfade" and "dip", which are NOT names ffmpeg's `xfade` enum
    /// accepts — emitted verbatim they abort the render. The builder must map
    /// them to real xfade names ("dissolve" / "fadeblack") and pass genuine
    /// xfade names through untouched. The picker↔ffmpeg seam itself is pinned
    /// end-to-end in tests/compose_xfade_vocabulary.rs.
    #[test]
    fn ui_transition_kinds_map_to_real_xfade_names() {
        for (ui_kind, expected) in [
            ("fade", "fade"),
            ("crossfade", "dissolve"),
            ("dip", "fadeblack"),
            ("wipeleft", "wipeleft"), // genuine xfade name → pass-through
        ] {
            let mut second = item("i1", "v1", "m1", 4000, 0, 4000);
            second.transition_in = Some(Transition {
                kind: ui_kind.into(),
                duration_ms: 1000,
            });
            let p = project(
                vec![media("m1", "/a.mp4", false)],
                vec![track("v1", TrackKind::Video, 0)],
                vec![item("i0", "v1", "m1", 0, 0, 4000), second],
                vec![],
            );
            let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));
            assert!(
                g.contains(&format!("xfade=transition={expected}:")),
                "picker kind {ui_kind:?} must emit xfade name {expected:?}: {g}"
            );
        }
    }

    /// Regression (seam-compose-settings-missing-even-up): odd caller-supplied
    /// dimensions must be sanitized to even at the builder seam — otherwise the
    /// lavfi canvas silently rounds down while the xfade branch scales to the
    /// raw odd size, and the mismatch aborts the render. `proxy_settings`
    /// already even_up'd; the export path must too.
    #[test]
    fn odd_settings_are_evened_up_across_the_whole_graph() {
        let mut second = item("i1", "v1", "m1", 4000, 0, 4000);
        second.transition_in = Some(Transition {
            kind: "fade".into(),
            duration_ms: 1000,
        });
        let p = project(
            vec![media("m1", "/a.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 0, 4000), second],
            vec![],
        );
        let mut s = settings();
        s.width = 641;
        s.height = 481;
        let args = build_filter_complex(&p, &s, None, "out.mp4");
        // Canvas and the xfade scale branch agree on the sanitized geometry…
        assert!(
            args.iter().any(|a| a.starts_with("color=black:s=642x482")),
            "canvas must use even dims: {args:?}"
        );
        let g = fc(&args);
        assert!(g.contains("scale=642:482"), "xfade branch even: {g}");
        // …and no raw odd geometry leaks anywhere in the argv.
        assert!(
            !args.iter().any(|a| a.contains("641") || a.contains("481")),
            "odd dimensions must not survive sanitization: {args:?}"
        );
    }

    #[test]
    fn audio_items_combine_via_amix() {
        let p = project(
            vec![media("m1", "/a.mp4", true), audio_media("m2", "/b.mp3")],
            vec![
                track("v1", TrackKind::Video, 0),
                track("a1", TrackKind::Audio, 1),
            ],
            vec![
                item("i0", "v1", "m1", 0, 0, 5000),
                item("i1", "a1", "m2", 1000, 0, 4000),
            ],
            vec![],
        );
        let args = build_filter_complex(&p, &settings(), None, "out.mp4");
        let g = fc(&args);
        assert!(g.contains("amix=inputs=2:normalize=0"), "got {g}");
        assert!(g.contains("atrim=start=0.000:end=5.000"), "got {g}");
        assert!(g.contains("adelay=1000|1000"), "got {g}");
        // Audio is mapped out.
        assert!(args.windows(2).any(|w| w[0] == "-map" && w[1] == "[aout]"));
        assert!(args.iter().any(|a| a == "-c:a"));
    }

    #[test]
    fn no_audio_yields_an_flag() {
        let p = project(
            vec![media("m1", "/a.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 0, 5000)],
            vec![],
        );
        let args = build_filter_complex(&p, &settings(), None, "out.mp4");
        assert!(args.iter().any(|a| a == "-an"));
        assert!(!args.iter().any(|a| a == "-c:a"));
    }

    #[test]
    fn caption_layer_is_last_filter() {
        let p = project(
            vec![media("m1", "/a.mp4", false), media("m2", "/b.mp4", false)],
            vec![
                track("v1", TrackKind::Video, 0),
                track("t2", TrackKind::Overlay, 1),
            ],
            vec![
                item("i0", "v1", "m1", 0, 0, 5000),
                item("i1", "t2", "m2", 0, 0, 5000),
            ],
            vec![caption("c0", 0, 3000)],
        );
        let args = build_filter_complex(&p, &settings(), Some("subs.ass"), "out.mp4");
        let g = fc(&args);
        let ass_pos = g.find("ass=subs.ass").expect("ass filter present");
        // No trim/overlay/xfade may appear after the ass node.
        let tail = &g[ass_pos..];
        assert!(!tail.contains("overlay"), "ass must be last: {g}");
        assert!(!tail.contains("trim="), "ass must be last: {g}");
        assert!(!tail.contains("xfade"), "ass must be last: {g}");
        // The composited video is mapped from the ass output.
        assert!(args.windows(2).any(|w| w[0] == "-map" && w[1] == "[vout]"));
    }

    #[test]
    fn no_ass_when_none() {
        let p = project(
            vec![media("m1", "/a.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 0, 5000)],
            vec![],
        );
        let args = build_filter_complex(&p, &settings(), None, "out.mp4");
        let g = fc(&args);
        assert!(
            !g.contains("ass="),
            "no caption layer without a sidecar: {g}"
        );
    }

    #[test]
    fn transform_scale_crop_rotate_opacity_emitted() {
        let mut it = item("i0", "v1", "m1", 0, 0, 5000);
        it.transform = Transform {
            x: 0.1,
            y: 0.2,
            scale: 0.5,
            rotation_deg: 90.0,
            opacity: 0.5,
            crop: Some(crate::model::CropRect {
                x: 0.0,
                y: 0.0,
                width: 0.5,
                height: 0.5,
            }),
        };
        let p = project(
            vec![media("m1", "/a.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![it],
            vec![],
        );
        let args = build_filter_complex(&p, &settings(), None, "out.mp4");
        let g = fc(&args);
        assert!(g.contains("scale=iw*0.5:ih*0.5"), "got {g}");
        assert!(g.contains("crop=iw*0.5:ih*0.5"), "got {g}");
        assert!(g.contains("rotate=90*PI/180"), "got {g}");
        assert!(g.contains("colorchannelmixer=aa=0.5"), "got {g}");
    }

    #[test]
    fn bitrate_flag_when_set() {
        let mut s = settings();
        s.bitrate_kbps = Some(9000);
        let p = project(
            vec![media("m1", "/a.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 0, 5000)],
            vec![],
        );
        let args = build_filter_complex(&p, &s, None, "out.mp4");
        let i = args.iter().position(|a| a == "-b:v").unwrap();
        assert_eq!(args[i + 1], "9000k");
    }

    #[test]
    fn output_is_last_arg_and_encoder_selected() {
        let p = project(
            vec![media("m1", "/a.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 0, 5000)],
            vec![],
        );
        let args = build_filter_complex(&p, &settings(), None, "final.mp4");
        assert_eq!(args.last().unwrap(), "final.mp4");
        let i = args.iter().position(|a| a == "-c:v").unwrap();
        assert_eq!(args[i + 1], "libx264");
    }

    #[test]
    fn simple_timeline_detected_when_no_av_items() {
        // Captions only, no media-backed timeline items → simple path.
        let p = project(vec![], vec![], vec![], vec![caption("c0", 0, 3000)]);
        assert!(is_simple_timeline(&p));
    }

    #[test]
    fn non_simple_when_visual_item_present() {
        let p = project(
            vec![media("m1", "/a.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 0, 5000)],
            vec![],
        );
        assert!(!is_simple_timeline(&p));
    }

    /// The exact shape `Project::backfill_default_timeline` synthesizes on
    /// import/load: the primary video (path + hash match the scalars) placed
    /// as ONE pristine full-length clip, plus captions.
    fn baseline_project() -> Project {
        project(
            vec![media("m1", "/x.mp4", true)], // matches video_path "/x.mp4", hash "h"
            vec![
                track("v1", TrackKind::Video, 0),
                track("c1", TrackKind::Caption, 1),
            ],
            vec![item("i0", "v1", "m1", 0, 0, 60_000)], // full 60s source
            vec![caption("c0", 0, 3000)],
        )
    }

    #[test]
    fn backfilled_import_shape_takes_the_simple_path() {
        // A fresh import (or a migrated v<=3 file) must keep the battle-tested
        // burn-in fast path even though the video is now a placed clip.
        assert!(is_simple_timeline(&baseline_project()));
    }

    #[test]
    fn backfill_helper_output_takes_the_simple_path() {
        // Cross-layer invariant, proven against the REAL helper: whatever
        // `backfill_default_timeline` produces is simple.
        let mut p = project(vec![], vec![], vec![], vec![caption("c0", 0, 3000)]);
        p.backfill_default_timeline(true);
        assert!(is_simple_timeline(&p));
    }

    #[test]
    fn edited_baseline_clip_is_not_simple() {
        // Any real edit to the placed primary clip must leave the fast path.
        let mut p = baseline_project();
        p.timeline_items[0].out_ms = 30_000; // trimmed
        assert!(!is_simple_timeline(&p));

        let mut p = baseline_project();
        p.timeline_items[0].timeline_start_ms = 1_000; // moved
        assert!(!is_simple_timeline(&p));

        let mut p = baseline_project();
        p.timeline_items[0].speed = 2.0; // retimed
        assert!(!is_simple_timeline(&p));

        let mut p = baseline_project();
        p.timeline_items[0].transform.scale = 0.5; // transformed
        assert!(!is_simple_timeline(&p));

        let mut p = baseline_project();
        p.timeline_items[0].effects.push(crate::model::Effect {
            id: "e1".into(),
            kind: "brightness".into(),
            params: serde_json::json!({ "amount": 0.2 }),
            enabled: true,
        }); // effect applied
        assert!(!is_simple_timeline(&p));
    }

    #[test]
    fn extra_item_or_foreign_media_is_not_simple() {
        // A second placed clip → composite path.
        let mut p = baseline_project();
        p.media.push(media("m2", "/broll.mp4", false));
        p.timeline_items[0].out_ms = 30_000;
        p.timeline_items
            .push(item("i1", "v1", "m2", 30_000, 0, 5000));
        assert!(!is_simple_timeline(&p));

        // A single full-length clip of NON-primary media → composite path.
        let p = project(
            vec![media("m2", "/broll.mp4", true)], // path != video_path
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m2", 0, 0, 60_000)],
            vec![],
        );
        assert!(!is_simple_timeline(&p));
    }

    #[test]
    fn compose_fraction_progresses_and_clamps() {
        assert_eq!(compose_fraction(0, 4000), Some(0.0));
        assert_eq!(compose_fraction(2000, 4000), Some(0.5));
        assert_eq!(compose_fraction(8000, 4000), Some(1.0)); // clamped
        assert_eq!(compose_fraction(0, 0), None);
    }

    #[test]
    fn parse_progress_reads_time_frame_and_end() {
        let mut out_ms = 0i64;
        let mut frame = 0i64;
        let mut done = false;
        parse_progress_line("frame=120", &mut out_ms, &mut frame, &mut done);
        parse_progress_line("out_time_ms=2500000", &mut out_ms, &mut frame, &mut done);
        parse_progress_line("progress=continue", &mut out_ms, &mut frame, &mut done);
        assert_eq!(frame, 120);
        assert_eq!(out_ms, 2500); // 2_500_000 µs → 2500 ms
        assert!(!done);
        parse_progress_line("progress=end", &mut out_ms, &mut frame, &mut done);
        assert!(done);
    }

    #[test]
    fn proxy_settings_caps_height_and_uses_cpu() {
        let p = project(vec![], vec![], vec![], vec![]); // 1920x1080 @30 defaults
        let s = proxy_settings(&p);
        assert!(s.height <= 480, "height capped at 480, got {}", s.height);
        assert_eq!(s.height, 480);
        assert_eq!(s.width, 854, "1920x1080 → 854x480 (even, aspect kept)");
        assert_eq!(s.encoder, Encoder::Cpu);
        assert_eq!(s.codec, VideoCodec::H264);
        assert!(s.bitrate_kbps.is_some());
        assert!(s.width % 2 == 0 && s.height % 2 == 0, "even dims");
    }

    #[test]
    fn proxy_settings_never_upscales_or_exceeds_30fps() {
        let mut p = project(vec![], vec![], vec![], vec![]);
        p.video_width = 320;
        p.video_height = 240;
        p.video_fps = 60.0;
        let s = proxy_settings(&p);
        assert_eq!(s.height, 240, "no upscaling past the source");
        assert_eq!(s.width, 320);
        assert!((s.fps - 30.0).abs() < f32::EPSILON, "fps capped at 30");
    }

    #[test]
    fn gap_start_item_shifts_pts_to_timeline_position() {
        // A clip starting at t=2s must carry `setpts=PTS-STARTPTS+2.000/TB` so
        // `overlay` pairs it with the canvas at the right timeline instant —
        // without the shift it freezes on its last frame (frames verified live
        // in `compose_edge_leading_gap`).
        let p = project(
            vec![media("m1", "/a.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 2000, 0, 3000)],
            vec![],
        );
        let args = build_filter_complex(&p, &settings(), None, "out.mp4");
        let g = fc(&args);
        assert!(g.contains("setpts=PTS-STARTPTS+2.000/TB"), "got {g}");
        assert!(
            g.contains("enable='between(t,2.000,5.000)'"),
            "overlay window matches the shifted clip: {g}"
        );
        // Items at t=0 keep the plain reset (graph stays byte-stable).
        let p0 = project(
            vec![media("m1", "/a.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 0, 3000)],
            vec![],
        );
        let g0 = fc(&build_filter_complex(&p0, &settings(), None, "out.mp4"));
        assert!(g0.contains("setpts=PTS-STARTPTS[pv0]"), "got {g0}");
    }

    #[test]
    fn leading_transition_on_first_item_degrades_to_hard_cut() {
        // `transition_in` on the FIRST item of a track has no preceding
        // boundary to crossfade over → plain overlay, no xfade in the graph.
        let mut first = item("i0", "v1", "m1", 0, 0, 4000);
        first.transition_in = Some(Transition {
            kind: "fade".into(),
            duration_ms: 1000,
        });
        let p = project(
            vec![media("m1", "/a.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![first],
            vec![],
        );
        let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));
        assert!(
            !g.contains("xfade"),
            "leading transition must not xfade: {g}"
        );
        assert!(g.contains("[pv0]overlay"), "falls back to overlay: {g}");
    }

    #[test]
    fn audio_only_timeline_maps_canvas_input_plainly() {
        // No visual items → the video map is the RAW canvas input pad, which
        // must be UNBRACKETED ("-map [1:v]" is a filtergraph-output lookup and
        // fails with "Output with label '1:v' does not exist").
        let p = project(
            vec![audio_media("m1", "/a.mp3")],
            vec![track("a1", TrackKind::Audio, 0)],
            vec![item("i0", "a1", "m1", 0, 0, 3000)],
            vec![],
        );
        let args = build_filter_complex(&p, &settings(), None, "out.mp4");
        assert!(
            args.windows(2).any(|w| w[0] == "-map" && w[1] == "1:v"),
            "canvas mapped as a plain input pad: {args:?}"
        );
        assert!(args.windows(2).any(|w| w[0] == "-map" && w[1] == "[aout]"));
        assert!(
            !args.iter().any(|a| a == "[1:v]"),
            "no bracketed raw-input map: {args:?}"
        );
    }

    #[test]
    fn degenerate_graph_omits_empty_filter_complex() {
        // Caption-track-only project, called directly WITHOUT a sidecar (the
        // run_compose simple-path guard normally routes this to burn-in): no
        // nodes → the empty `-filter_complex ""` flag (which ffmpeg rejects)
        // must be omitted and the bare canvas mapped plainly.
        let p = project(
            vec![],
            vec![track("cap", TrackKind::Caption, 0)],
            vec![],
            vec![caption("c0", 0, 3000)],
        );
        assert!(is_simple_timeline(&p), "run_compose takes the burn-in path");
        let args = build_filter_complex(&p, &settings(), None, "out.mp4");
        assert!(
            !args.iter().any(|a| a == "-filter_complex"),
            "no empty filter_complex: {args:?}"
        );
        assert!(args.windows(2).any(|w| w[0] == "-map" && w[1] == "0:v"));
        // WITH a sidecar the ass node exists, so the graph is non-degenerate.
        let args = build_filter_complex(&p, &settings(), Some("subs.ass"), "out.mp4");
        assert!(fc(&args).contains("[0:v]ass=subs.ass[vout]"));
    }

    #[test]
    fn build_proxy_args_injects_ultrafast_before_output() {
        let p = project(
            vec![media("m1", "/a.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 0, 5000)],
            vec![],
        );
        let s = proxy_settings(&p);
        let args = build_proxy_args(&p, &s, None, "proxy.mp4");
        assert_eq!(args.last().unwrap(), "proxy.mp4", "output stays last");
        let pos = args.iter().position(|a| a == "-preset").unwrap();
        assert_eq!(args[pos + 1], "ultrafast");
        assert!(pos < args.len() - 2, "preset precedes the output arg");
    }

    /// Live end-to-end COMPOSE — flattens a 2-item / 2-track project (a base
    /// video plus a scaled picture-in-picture overlay drawn from the same
    /// source media) to a real MP4 via the `build_filter_complex` argument
    /// vector, then ffprobes the output to confirm the composited dimensions,
    /// duration, and streams. `#[ignore]`d because it needs a real sample video
    /// plus a working ffmpeg/ffprobe on the machine — `cargo test` compiles but
    /// skips it, so the CI build stays ffmpeg-free.
    ///
    /// ```sh
    /// SUNDAYEDIT_TEST_VIDEO=/path/to/sample.mp4 \
    ///   cargo test compose_two_track_project_to_mp4 -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a real sample video (SUNDAYEDIT_TEST_VIDEO) + ffmpeg/ffprobe on PATH"]
    fn compose_two_track_project_to_mp4() {
        use std::process::Command;

        let sample = std::env::var("SUNDAYEDIT_TEST_VIDEO")
            .expect("set SUNDAYEDIT_TEST_VIDEO to a real video path");
        let out = std::env::temp_dir().join("sundayedit_compose_two_track.mp4");
        let _ = std::fs::remove_file(&out);

        // Two tracks: a base video (index 0) + an overlay PiP (index 1), each a
        // 3-second clip. Both items reference the SAME source media, so the
        // graph dedupes to a single `-i` input + the black canvas.
        let mut pip = item("pip", "t2", "m1", 0, 0, 3000);
        pip.transform = Transform {
            scale: 0.4,
            x: 0.55,
            y: 0.05,
            ..Transform::default()
        };
        let p = project(
            vec![media("m1", &sample, true)],
            vec![
                track("t1", TrackKind::Video, 0),
                track("t2", TrackKind::Overlay, 1),
            ],
            vec![item("base", "t1", "m1", 0, 0, 3000), pip],
            vec![],
        );

        let mut s = settings();
        s.width = 1280;
        s.height = 720;

        let out_str = out.to_string_lossy().into_owned();
        let args = build_filter_complex(&p, &s, None, &out_str);
        let status = Command::new("ffmpeg")
            .args(&args)
            .status()
            .expect("spawn ffmpeg");
        assert!(status.success(), "ffmpeg compose exited non-zero");
        assert!(out.exists(), "compose did not write {out_str}");

        // ffprobe the output → dimensions / duration / streams.
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(&out)
            .output()
            .expect("spawn ffprobe");
        let json = String::from_utf8_lossy(&probe.stdout);
        let meta = crate::services::video::parse_ffprobe_json(&json).expect("ffprobe json parses");
        assert_eq!(meta.width, 1280, "composed onto the 1280-wide canvas");
        assert_eq!(meta.height, 720, "composed onto the 720-high canvas");
        assert!(
            meta.duration_ms >= 2500,
            "≈3s timeline, got {} ms",
            meta.duration_ms
        );
        assert!(
            meta.video_codec.is_some(),
            "output must carry a video stream"
        );

        let _ = std::fs::remove_file(&out);
    }

    // ── Shared #[ignore] integration helpers ──────────────────────────────────

    /// Spawn bare `ffmpeg` with `args`, assert success + that `out` was written,
    /// then `ffprobe` it into `VideoMetadata`. Used by the live compose tests.
    fn run_ffmpeg_and_probe(args: &[String], out: &Path) -> crate::services::video::VideoMetadata {
        use std::process::Command;
        let status = Command::new("ffmpeg")
            .args(args)
            .status()
            .expect("spawn ffmpeg");
        assert!(status.success(), "ffmpeg compose exited non-zero");
        assert!(out.exists(), "compose did not write {}", out.display());
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(out)
            .output()
            .expect("spawn ffprobe");
        let json = String::from_utf8_lossy(&probe.stdout);
        crate::services::video::parse_ffprobe_json(&json).expect("ffprobe json parses")
    }

    /// ffprobe a source file for its native pixel dimensions. `xfade` needs both
    /// branches to be the SAME size, so its test renders onto the source dims.
    fn probe_dims(path: &str) -> (i32, i32) {
        use std::process::Command;
        let probe = Command::new("ffprobe")
            .args(["-v", "error", "-print_format", "json", "-show_streams"])
            .arg(path)
            .output()
            .expect("spawn ffprobe");
        let json = String::from_utf8_lossy(&probe.stdout);
        let meta = crate::services::video::parse_ffprobe_json(&json).expect("ffprobe json parses");
        (meta.width, meta.height)
    }

    /// Two DISTINCT sources back-to-back on ONE video track → the output runs
    /// the SUM of both clip lengths and carries a single video stream.
    #[test]
    #[ignore = "needs SUNDAYEDIT_TEST_VIDEO + SUNDAYEDIT_TEST_VIDEO2 + ffmpeg/ffprobe on PATH"]
    fn compose_concat_two_distinct_sources() {
        let a = std::env::var("SUNDAYEDIT_TEST_VIDEO").expect("set SUNDAYEDIT_TEST_VIDEO");
        let b = std::env::var("SUNDAYEDIT_TEST_VIDEO2").expect("set SUNDAYEDIT_TEST_VIDEO2");
        let out = std::env::temp_dir().join("sundayedit_compose_concat.mp4");
        let _ = std::fs::remove_file(&out);

        let p = project(
            vec![media("m1", &a, false), media("m2", &b, false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![
                item("i0", "v1", "m1", 0, 0, 3000),
                item("i1", "v1", "m2", 3000, 0, 3000),
            ],
            vec![],
        );
        let mut s = settings();
        s.width = 1280;
        s.height = 720;

        let out_str = out.to_string_lossy().into_owned();
        let args = build_filter_complex(&p, &s, None, &out_str);
        let meta = run_ffmpeg_and_probe(&args, &out);
        assert!(
            meta.duration_ms >= 5000,
            "≈6s concat, got {} ms",
            meta.duration_ms
        );
        assert!(
            meta.video_codec.is_some(),
            "output must have a video stream"
        );
        let _ = std::fs::remove_file(&out);
    }

    /// Two clips on one track, the second carrying a 500 ms `fade` transition →
    /// the output exists, has video, and runs ≈ (sum − overlap).
    #[test]
    #[ignore = "needs SUNDAYEDIT_TEST_VIDEO + ffmpeg/ffprobe on PATH"]
    fn compose_xfade_transition() {
        let sample = std::env::var("SUNDAYEDIT_TEST_VIDEO").expect("set SUNDAYEDIT_TEST_VIDEO");
        let out = std::env::temp_dir().join("sundayedit_compose_xfade.mp4");
        let _ = std::fs::remove_file(&out);

        // xfade blends two same-size streams → render onto the source's dims.
        let (w, h) = probe_dims(&sample);

        let mut second = item("i1", "v1", "m1", 2500, 0, 3000); // ends 5500
        second.transition_in = Some(Transition {
            kind: "fade".into(),
            duration_ms: 500,
        });
        let p = project(
            vec![media("m1", &sample, false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 0, 3000), second],
            vec![],
        );
        let mut s = settings();
        s.width = w;
        s.height = h;

        let out_str = out.to_string_lossy().into_owned();
        let args = build_filter_complex(&p, &s, None, &out_str);
        let meta = run_ffmpeg_and_probe(&args, &out);
        // sum 6000 − 500 overlap ≈ 5500 ms.
        assert!(
            meta.duration_ms >= 4500,
            "xfade duration ≈5.5s, got {} ms",
            meta.duration_ms
        );
        assert!(
            meta.video_codec.is_some(),
            "output must have a video stream"
        );
        let _ = std::fs::remove_file(&out);
    }

    /// A base clip plus a scaled PiP overlay on a SECOND video track → the
    /// composite lands on the requested canvas dimensions.
    #[test]
    #[ignore = "needs SUNDAYEDIT_TEST_VIDEO + ffmpeg/ffprobe on PATH"]
    fn compose_pip_two_video_tracks() {
        let sample = std::env::var("SUNDAYEDIT_TEST_VIDEO").expect("set SUNDAYEDIT_TEST_VIDEO");
        let out = std::env::temp_dir().join("sundayedit_compose_pip.mp4");
        let _ = std::fs::remove_file(&out);

        let mut pip = item("pip", "t2", "m1", 0, 0, 3000);
        pip.transform = Transform {
            scale: 0.4,
            x: 0.55,
            y: 0.05,
            ..Transform::default()
        };
        let p = project(
            vec![media("m1", &sample, true)],
            vec![
                track("t1", TrackKind::Video, 0),
                track("t2", TrackKind::Video, 1),
            ],
            vec![item("base", "t1", "m1", 0, 0, 3000), pip],
            vec![],
        );
        let mut s = settings();
        s.width = 1280;
        s.height = 720;

        let out_str = out.to_string_lossy().into_owned();
        let args = build_filter_complex(&p, &s, None, &out_str);
        let meta = run_ffmpeg_and_probe(&args, &out);
        assert_eq!(meta.width, 1280, "composited onto the 1280-wide canvas");
        assert_eq!(meta.height, 720, "composited onto the 720-high canvas");
        let _ = std::fs::remove_file(&out);
    }

    /// Two audio-bearing clips (a video+audio clip on a video track + an
    /// audio-bearing clip on an audio track) → the output carries an AUDIO
    /// stream (the two sources amix together).
    #[test]
    #[ignore = "needs SUNDAYEDIT_TEST_VIDEO + SUNDAYEDIT_TEST_VIDEO2 (both with audio) + ffmpeg/ffprobe on PATH"]
    fn compose_audio_amix_two_sources() {
        let a = std::env::var("SUNDAYEDIT_TEST_VIDEO").expect("set SUNDAYEDIT_TEST_VIDEO");
        let b = std::env::var("SUNDAYEDIT_TEST_VIDEO2").expect("set SUNDAYEDIT_TEST_VIDEO2");
        let out = std::env::temp_dir().join("sundayedit_compose_amix.mp4");
        let _ = std::fs::remove_file(&out);

        let p = project(
            vec![media("m1", &a, true), media("m2", &b, true)],
            vec![
                track("v1", TrackKind::Video, 0),
                track("a1", TrackKind::Audio, 1),
            ],
            vec![
                item("i0", "v1", "m1", 0, 0, 3000),
                item("i1", "a1", "m2", 0, 0, 3000),
            ],
            vec![],
        );
        let mut s = settings();
        s.width = 1280;
        s.height = 720;

        let out_str = out.to_string_lossy().into_owned();
        let args = build_filter_complex(&p, &s, None, &out_str);
        let meta = run_ffmpeg_and_probe(&args, &out);
        assert!(
            meta.audio_codec.is_some(),
            "amix must produce an audio stream"
        );
        let _ = std::fs::remove_file(&out);
    }

    /// A project WITH a caption track + captions, rendered with the ass sidecar
    /// layer → succeeds with a video stream at canvas dims (captions are burned
    /// into pixels, so there is no separate subtitle stream to probe).
    #[test]
    #[ignore = "needs SUNDAYEDIT_TEST_VIDEO + ffmpeg/ffprobe on PATH"]
    fn compose_with_captions_burned_in() {
        let sample = std::env::var("SUNDAYEDIT_TEST_VIDEO").expect("set SUNDAYEDIT_TEST_VIDEO");
        let out = std::env::temp_dir().join("sundayedit_compose_captions.mp4");
        let _ = std::fs::remove_file(&out);

        let p = project(
            vec![media("m1", &sample, false)],
            vec![
                track("v1", TrackKind::Video, 0),
                track("cap", TrackKind::Caption, 1),
            ],
            vec![item("i0", "v1", "m1", 0, 0, 3000)],
            vec![caption("c0", 0, 2500)],
        );
        let mut s = settings();
        s.width = 1280;
        s.height = 720;

        // Write the caption sidecar exactly like `run_compose` does.
        let ass = crate::services::export::write_ass(&p);
        let ass_path = std::env::temp_dir().join("sundayedit_compose_captions.ass");
        std::fs::write(&ass_path, ass).unwrap();

        let out_str = out.to_string_lossy().into_owned();
        let args = build_filter_complex(&p, &s, Some(&ass_path.to_string_lossy()), &out_str);
        let meta = run_ffmpeg_and_probe(&args, &out);
        assert_eq!(meta.width, 1280);
        assert_eq!(meta.height, 720);
        assert!(
            meta.video_codec.is_some(),
            "output must have a video stream"
        );

        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(&ass_path);
    }

    /// The proxy arg path renders a low-res composite: a 1080p project caps at
    /// 480 tall, and `-preset ultrafast` is present.
    #[test]
    #[ignore = "needs SUNDAYEDIT_TEST_VIDEO + ffmpeg/ffprobe on PATH"]
    fn proxy_render_is_low_res() {
        let sample = std::env::var("SUNDAYEDIT_TEST_VIDEO").expect("set SUNDAYEDIT_TEST_VIDEO");
        let out = std::env::temp_dir().join("sundayedit_compose_proxy.mp4");
        let _ = std::fs::remove_file(&out);

        let mut p = project(
            vec![media("m1", &sample, false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 0, 3000)],
            vec![],
        );
        // A 1080p project → proxy caps at 480 tall.
        p.video_width = 1920;
        p.video_height = 1080;
        p.video_fps = 30.0;

        let s = proxy_settings(&p);
        assert!(
            s.height <= 480,
            "proxy settings cap height at 480, got {}",
            s.height
        );

        let out_str = out.to_string_lossy().into_owned();
        let args = build_proxy_args(&p, &s, None, &out_str);
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-preset" && w[1] == "ultrafast"),
            "proxy args carry -preset ultrafast"
        );
        let meta = run_ffmpeg_and_probe(&args, &out);
        assert!(
            meta.height <= 480,
            "proxy output height ≤ 480, got {}",
            meta.height
        );
        let _ = std::fs::remove_file(&out);
    }

    // ── #[ignore] edge-case regressions (Suspect D probe, 2026-08-08) ────────

    /// Grab ONE decoded frame at `t` seconds as raw 8-bit grayscale bytes.
    fn gray_frame_at(path: &Path, t: f64) -> Vec<u8> {
        use std::process::Command;
        let out = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{t}")])
            .arg("-i")
            .arg(path)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
            .output()
            .expect("spawn ffmpeg frame grab");
        assert!(out.status.success(), "frame grab at {t}s failed");
        assert!(!out.stdout.is_empty(), "no frame decoded at {t}s");
        out.stdout
    }

    fn mean_luma(frame: &[u8]) -> f64 {
        frame.iter().map(|&b| b as f64).sum::<f64>() / frame.len() as f64
    }

    fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len(), "frames must share dimensions");
        a.iter()
            .zip(b)
            .map(|(&x, &y)| (x as f64 - y as f64).abs())
            .sum::<f64>()
            / a.len() as f64
    }

    /// Case 1 — the FIRST clip starts at t=2s (gap at the timeline head). The
    /// canvas must cover [0,2s) with black, the clip must actually PLAY from
    /// 2s (regression: without the `setpts=+start/TB` shift, `overlay` paired
    /// raw 0-based PTS and froze the clip on its last frame), and the total
    /// duration must span the timeline.
    #[test]
    #[ignore = "needs SUNDAYEDIT_TEST_VIDEO + ffmpeg/ffprobe on PATH"]
    fn compose_edge_leading_gap() {
        let sample = std::env::var("SUNDAYEDIT_TEST_VIDEO").expect("set SUNDAYEDIT_TEST_VIDEO");
        let out = std::env::temp_dir().join("sundayedit_compose_edge_gap.mp4");
        let _ = std::fs::remove_file(&out);

        // 3s clip placed at [2s, 5s) — nothing on the timeline before it.
        let p = project(
            vec![media("m1", &sample, false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 2000, 0, 3000)],
            vec![],
        );
        let mut s = settings();
        s.width = 1280;
        s.height = 720;

        let out_str = out.to_string_lossy().into_owned();
        let args = build_filter_complex(&p, &s, None, &out_str);
        let meta = run_ffmpeg_and_probe(&args, &out);
        assert!(
            (4500..=5500).contains(&meta.duration_ms),
            "timeline spans 5s, got {} ms",
            meta.duration_ms
        );

        // [0,2s) is black canvas…
        let lead_in = gray_frame_at(&out, 1.0);
        assert!(
            mean_luma(&lead_in) < 16.0,
            "gap must render black, mean luma {}",
            mean_luma(&lead_in)
        );
        // …the clip is visible once it starts…
        let content = gray_frame_at(&out, 2.5);
        assert!(
            mean_luma(&content) > 16.0,
            "clip must be visible at 2.5s, mean luma {}",
            mean_luma(&content)
        );
        // …and it PLAYS rather than freezing (distinct frames late in the clip).
        let a = gray_frame_at(&out, 3.5);
        let b = gray_frame_at(&out, 4.5);
        assert!(
            mean_abs_diff(&a, &b) > 0.05,
            "clip froze after the gap (identical frames at 3.5s/4.5s)"
        );
        let _ = std::fs::remove_file(&out);
    }

    /// Case 2 — `transition_in` on the FIRST item of a track (nothing before
    /// it) must degrade to a hard cut: the graph renders cleanly with no
    /// xfade node.
    #[test]
    #[ignore = "needs SUNDAYEDIT_TEST_VIDEO + ffmpeg/ffprobe on PATH"]
    fn compose_edge_leading_transition_hard_cut() {
        let sample = std::env::var("SUNDAYEDIT_TEST_VIDEO").expect("set SUNDAYEDIT_TEST_VIDEO");
        let out = std::env::temp_dir().join("sundayedit_compose_edge_leadtrans.mp4");
        let _ = std::fs::remove_file(&out);

        let mut first = item("i0", "v1", "m1", 0, 0, 3000);
        first.transition_in = Some(Transition {
            kind: "fade".into(),
            duration_ms: 1000,
        });
        let p = project(
            vec![media("m1", &sample, false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![first],
            vec![],
        );
        let mut s = settings();
        s.width = 1280;
        s.height = 720;

        let out_str = out.to_string_lossy().into_owned();
        let args = build_filter_complex(&p, &s, None, &out_str);
        assert!(!fc(&args).contains("xfade"), "no dangling xfade");
        let meta = run_ffmpeg_and_probe(&args, &out);
        assert!(
            meta.duration_ms >= 2500,
            "≈3s clip, got {} ms",
            meta.duration_ms
        );
        assert!(meta.video_codec.is_some());
        let _ = std::fs::remove_file(&out);
    }

    /// Case 3 — an audio item at t=0 emits `adelay=0|0`; real ffmpeg accepts a
    /// zero delay and the output carries the audio stream.
    #[test]
    #[ignore = "needs SUNDAYEDIT_TEST_VIDEO (with audio) + ffmpeg/ffprobe on PATH"]
    fn compose_edge_adelay_zero() {
        let sample = std::env::var("SUNDAYEDIT_TEST_VIDEO").expect("set SUNDAYEDIT_TEST_VIDEO");
        let out = std::env::temp_dir().join("sundayedit_compose_edge_adelay0.mp4");
        let _ = std::fs::remove_file(&out);

        let p = project(
            vec![media("m1", &sample, true)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 0, 3000)],
            vec![],
        );
        let mut s = settings();
        s.width = 1280;
        s.height = 720;

        let out_str = out.to_string_lossy().into_owned();
        let args = build_filter_complex(&p, &s, None, &out_str);
        assert!(fc(&args).contains("adelay=0|0"), "t=0 item delays by zero");
        let meta = run_ffmpeg_and_probe(&args, &out);
        assert!(
            meta.audio_codec.is_some(),
            "adelay=0 must still yield an audio stream"
        );
        let _ = std::fs::remove_file(&out);
    }

    /// Case 4 — an `opacity<1` clip (whose chain ends `format=rgba,
    /// colorchannelmixer`) feeding the xfade normalise chain (`format=yuv420p`)
    /// must be accepted by real ffmpeg.
    #[test]
    #[ignore = "needs SUNDAYEDIT_TEST_VIDEO + ffmpeg/ffprobe on PATH"]
    fn compose_edge_opacity_into_xfade() {
        let sample = std::env::var("SUNDAYEDIT_TEST_VIDEO").expect("set SUNDAYEDIT_TEST_VIDEO");
        let out = std::env::temp_dir().join("sundayedit_compose_edge_rgba_xfade.mp4");
        let _ = std::fs::remove_file(&out);

        let (w, h) = probe_dims(&sample);

        // The INCOMING xfade branch carries opacity → its rgba stream enters
        // the normalise chain.
        let mut second = item("i1", "v1", "m1", 2500, 0, 3000);
        second.transition_in = Some(Transition {
            kind: "fade".into(),
            duration_ms: 500,
        });
        second.transform.opacity = 0.5;
        let p = project(
            vec![media("m1", &sample, false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 0, 3000), second],
            vec![],
        );
        let mut s = settings();
        s.width = w;
        s.height = h;

        let out_str = out.to_string_lossy().into_owned();
        let args = build_filter_complex(&p, &s, None, &out_str);
        let g = fc(&args);
        assert!(g.contains("colorchannelmixer=aa=0.5"), "got {g}");
        assert!(g.contains("xfade"), "got {g}");
        let meta = run_ffmpeg_and_probe(&args, &out);
        assert!(meta.video_codec.is_some(), "rgba→yuv420p xfade renders");
        assert!(
            meta.duration_ms >= 4500,
            "≈5.5s xfade timeline, got {} ms",
            meta.duration_ms
        );
        let _ = std::fs::remove_file(&out);
    }

    /// Case 5 — a SINGLE audio-only item (no visual items at all): the video
    /// map is the raw canvas pad (regression: `-map "[1:v]"` was rejected with
    /// "Output with label '1:v' does not exist") and the output carries BOTH
    /// the black-canvas video and the anull'd audio.
    #[test]
    #[ignore = "needs SUNDAYEDIT_TEST_VIDEO (with audio) + ffmpeg/ffprobe on PATH"]
    fn compose_edge_audio_only_item() {
        let sample = std::env::var("SUNDAYEDIT_TEST_VIDEO").expect("set SUNDAYEDIT_TEST_VIDEO");
        let out = std::env::temp_dir().join("sundayedit_compose_edge_audio_only.mp4");
        let _ = std::fs::remove_file(&out);

        let p = project(
            vec![audio_media("m1", &sample)],
            vec![track("a1", TrackKind::Audio, 0)],
            vec![item("i0", "a1", "m1", 0, 0, 3000)],
            vec![],
        );
        let mut s = settings();
        s.width = 1280;
        s.height = 720;

        let out_str = out.to_string_lossy().into_owned();
        let args = build_filter_complex(&p, &s, None, &out_str);
        let meta = run_ffmpeg_and_probe(&args, &out);
        assert!(
            meta.audio_codec.is_some(),
            "audio-only item must be audible"
        );
        assert!(meta.video_codec.is_some(), "black canvas video present");
        assert!(
            (2500..=3500).contains(&meta.duration_ms),
            "3s audio timeline, got {} ms",
            meta.duration_ms
        );
        let _ = std::fs::remove_file(&out);
    }

    /// Case 6 — a caption-track-only project called DIRECTLY on the builder
    /// with an ass sidecar: the degenerate graph (bare canvas + ass, no [pv]
    /// nodes) must render. (`run_compose` itself routes this through burn-in —
    /// asserted in `degenerate_graph_omits_empty_filter_complex`.)
    #[test]
    #[ignore = "needs ffmpeg/ffprobe on PATH"]
    fn compose_edge_caption_only_graph() {
        let out = std::env::temp_dir().join("sundayedit_compose_edge_caponly.mp4");
        let _ = std::fs::remove_file(&out);

        let p = project(
            vec![],
            vec![track("cap", TrackKind::Caption, 0)],
            vec![],
            vec![caption("c0", 0, 2500)],
        );
        assert!(is_simple_timeline(&p), "run_compose would take burn-in");
        let mut s = settings();
        s.width = 1280;
        s.height = 720;

        let ass = crate::services::export::write_ass(&p);
        let ass_path = std::env::temp_dir().join("sundayedit_compose_edge_caponly.ass");
        std::fs::write(&ass_path, ass).unwrap();

        let out_str = out.to_string_lossy().into_owned();
        let args = build_filter_complex(&p, &s, Some(&ass_path.to_string_lossy()), &out_str);
        let g = fc(&args);
        assert!(
            !g.contains("[pv"),
            "no visual nodes in a caption-only graph"
        );
        let meta = run_ffmpeg_and_probe(&args, &out);
        assert!(meta.video_codec.is_some(), "canvas+ass renders");

        // And WITHOUT the sidecar: the degenerate no-node arg vector (bare
        // lavfi canvas, no -filter_complex) must also be a valid command.
        let _ = std::fs::remove_file(&out);
        let args = build_filter_complex(&p, &s, None, &out_str);
        assert!(!args.iter().any(|a| a == "-filter_complex"));
        let meta = run_ffmpeg_and_probe(&args, &out);
        assert!(meta.video_codec.is_some(), "bare canvas renders");

        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(&ass_path);
    }

    /// Case 7 — two items OVERLAPPING on the same track WITHOUT a transition
    /// (the validator forbids this; defense-in-depth): the builder emits two
    /// overlays with overlapping enable-windows, the LATER item simply wins
    /// during the overlap, and real ffmpeg renders the full span.
    #[test]
    #[ignore = "needs SUNDAYEDIT_TEST_VIDEO + SUNDAYEDIT_TEST_VIDEO2 + ffmpeg/ffprobe on PATH"]
    fn compose_edge_overlap_without_transition() {
        let a = std::env::var("SUNDAYEDIT_TEST_VIDEO").expect("set SUNDAYEDIT_TEST_VIDEO");
        let b = std::env::var("SUNDAYEDIT_TEST_VIDEO2").expect("set SUNDAYEDIT_TEST_VIDEO2");
        let out = std::env::temp_dir().join("sundayedit_compose_edge_overlap.mp4");
        let _ = std::fs::remove_file(&out);

        // [0,3s) and [2s,6s) overlap on [2s,3s) — no transition declared.
        let p = project(
            vec![media("m1", &a, false), media("m2", &b, false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![
                item("i0", "v1", "m1", 0, 0, 3000),
                item("i1", "v1", "m2", 2000, 0, 4000),
            ],
            vec![],
        );
        let mut s = settings();
        s.width = 1280;
        s.height = 720;

        let out_str = out.to_string_lossy().into_owned();
        let args = build_filter_complex(&p, &s, None, &out_str);
        let meta = run_ffmpeg_and_probe(&args, &out);
        assert!(
            (5500..=6500).contains(&meta.duration_ms),
            "timeline spans 6s, got {} ms",
            meta.duration_ms
        );
        assert!(meta.video_codec.is_some());
        let _ = std::fs::remove_file(&out);
    }
}
