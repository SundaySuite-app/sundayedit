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
//!   - Per audio-bearing item: `atrim`/`asetpts`, then `atempo` (speed),
//!     `volume` (clip gain + track fader, dB-added into ONE node), `afade`
//!     in/out, and finally `adelay` to its timeline position — combined via
//!     `amix=inputs=K:normalize=0`, with `alimiter` on the bus when the mix
//!     can actually clip (see `BUS_LIMITER`).
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

/// Format a dB level for a filter argument. Fixed precision keeps the graph
/// stable across f32 round-trips (`-6.0000001dB` in a snapshot helps nobody).
fn db(v: f32) -> String {
    format!("{:.4}", v)
}

/// Format a playback speed for `setpts` / `atempo`.
fn rate(v: f64) -> String {
    format!("{:.6}", v)
}

/// Is this speed close enough to 1.0 that no time-scaling node is needed?
/// The tolerance is well below what a UI slider can express and well above
/// f32→f64 round-trip noise, so a "1.0" clip never grows an `atempo` node.
fn is_unit_speed(speed: f64) -> bool {
    (speed - 1.0).abs() < 1e-6
}

/// Decompose a playback speed into a chain of `atempo` filters.
///
/// `atempo` accepts only 0.5..=2.0 per instance — a 4× clip needs
/// `atempo=2.0,atempo=2.0`, and a 0.25× clip needs `atempo=0.5,atempo=0.5`.
/// Emitting the raw factor is not "mostly right": ffmpeg REJECTS the option
/// and the whole export dies, so the chaining is what makes speed usable at
/// all outside a narrow band.
///
/// Returns an empty vec at unit speed, so a normal clip's audio chain is
/// byte-identical to what it was before speed was implemented.
fn atempo_chain(speed: f64) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if is_unit_speed(speed) || !(speed.is_finite() && speed > 0.0) {
        return out;
    }
    let mut remaining = speed;
    // `clamp_speed` bounds speed to [0.01, 100], i.e. at most 7 doublings or
    // 7 halvings. The guard is belt-and-braces against a hand-edited file that
    // somehow reaches the builder unnormalised — it must not spin forever.
    let mut guard = 0;
    while remaining > 2.0 && guard < 32 {
        out.push("atempo=2.0".to_string());
        remaining /= 2.0;
        guard += 1;
    }
    while remaining < 0.5 && guard < 32 {
        out.push("atempo=0.5".to_string());
        remaining *= 2.0;
        guard += 1;
    }
    if !is_unit_speed(remaining) {
        out.push(format!("atempo={}", rate(remaining)));
    }
    out
}

/// Peak limiter for the MIXED audio bus.
///
/// **Headroom decision.** `amix=normalize=0` SUMS its inputs: a sermon bed at
/// −3 dBFS plus a music bed at −3 dBFS is +3 dBFS, and the encoder hard-clips
/// the overshoot. The two obvious cures are both dishonest:
///
///   * `normalize=1` divides every input by K, so adding a quiet b-roll clip
///     would duck the sermon by 6 dB — the gain the user dialled in would stop
///     meaning anything, and it would change with every clip added; and
///   * a fixed headroom trim (`volume=-6dB` on the bus) quietly makes the
///     whole render 6 dB quieter than the levels the user set and monitored.
///
/// So: KEEP `normalize=0` — a gain of −6 dB really is −6 dB, and a single
/// untouched clip stays bit-identical to what this engine rendered before R2 —
/// and catch the overshoot with a lookahead limiter on the bus instead, which
/// is transparent until something actually exceeds the ceiling.
///
/// Two details that are load-bearing rather than taste:
///   * `level=disabled` — `alimiter`'s `level` option DEFAULTS TO ENABLED and
///     auto-normalises the result back up to 0 dBFS. Left at its default it
///     would make a deliberately quiet mix LOUDER, i.e. undo the very gains
///     this feature exists to honour.
///   * `limit=0.891` ≈ −1 dBFS, not 1.0. The ceiling has to sit below full
///     scale so the AAC encoder's inter-sample peaks have somewhere to go.
///
/// Cost: `alimiter`'s lookahead delays the bus by `attack` (5 ms). That is an
/// order of magnitude below the audibility threshold for lip-sync, and it is
/// only paid when the limiter is inserted at all (see `needs_bus_limiter`).
const BUS_LIMITER: &str = "alimiter=level=disabled:limit=0.891:attack=5:release=50";

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

/// Human name for a timeline item kind, for error messages the user reads.
fn kind_label(kind: &TimelineItemKind) -> &'static str {
    match kind {
        TimelineItemKind::Av => "A/V",
        TimelineItemKind::Text => "Text",
        TimelineItemKind::Graphic => "Graphic",
    }
}

/// Items the compose graph CANNOT render: standalone `Text` / `Graphic`
/// overlays (`op_add_text_item` creates them with `source_media_id: None`, so
/// they are neither `is_visual` nor `has_audio` and no node in
/// `build_filter_complex` consumes them).
///
/// This is the single seam both consumers share:
///   - `is_simple_timeline` must return false for them, or the burn-in
///     shortcut swallows the overlay with zero signal (baseline import + one
///     text clip used to stay "simple" and export as if the text never existed);
///   - `build_filter_complex` must REFUSE them, so a project file can never
///     lose authored content quietly.
///
/// Sharing the predicate is what stops the two sides drifting apart again.
///
/// Only items that would actually be seen count: a disabled item, or one on a
/// hidden track, contributes nothing to the export *and* nothing to the
/// preview, so there is nothing to lose and nothing to complain about.
fn unsupported_overlay_items(project: &Project) -> Vec<&TimelineItem> {
    project
        .timeline_items
        .iter()
        .filter(|it| {
            it.enabled
                && track_visible(project, it)
                && matches!(it.kind, TimelineItemKind::Text | TimelineItemKind::Graphic)
        })
        .collect()
}

/// Refuse to build a compose graph that would silently omit authored content.
/// Pure. Called at the top of `build_filter_complex` (the pure builder, so no
/// caller can route around it) and again at the top of `run_compose` /
/// `run_compose_proxy` so the render fails before any temp sidecar is written.
pub fn validate_composable(project: &Project) -> AppResult<()> {
    let unsupported = unsupported_overlay_items(project);
    let Some(first) = unsupported.first() else {
        return Ok(());
    };
    let mut kinds: Vec<&str> = unsupported.iter().map(|it| kind_label(&it.kind)).collect();
    kinds.sort_unstable();
    kinds.dedup();
    Err(AppError::Validation(format!(
        "the export cannot render a {kind} timeline item yet — {n} enabled \
         {kinds} overlay item(s) on visible tracks would be dropped from the \
         render (first: id {id:?}{text}). Disable or delete them, or hide \
         their track, so nothing is lost without you knowing.",
        kind = kind_label(&first.kind),
        n = unsupported.len(),
        kinds = kinds.join("/"),
        id = first.id,
        text = first
            .text
            .as_ref()
            .map(|t| format!(", text {:?}", t.text))
            .unwrap_or_default(),
    )))
}

/// A "simple" timeline is one the existing single-track burn-in can render
/// exactly: no visual/audio timeline items to composite beyond, at most, the
/// primary video placed as ONE pristine full-length clip — the shape
/// `Project::backfill_default_timeline` synthesizes on import/load. Such a
/// project delegates to `burnin::render` (hardware encoding + audio
/// passthrough, battle-tested).
pub fn is_simple_timeline(project: &Project) -> bool {
    // A Text/Graphic overlay is neither visual nor audio, so it used to slip
    // through the `av_items` filter below and leave a text-bearing project
    // "simple" — the burn-in shortcut then rendered the video alone and the
    // overlay vanished. Anything the composite path cannot render must never
    // be swallowed by the shortcut either.
    if !unsupported_overlay_items(project).is_empty() {
        return false;
    }
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
/// timeline 0 with no trim/speed/transform/effects/transition, and untouched
/// audio? Rendering that through burn-in is identical to compositing it, so it
/// keeps the fast path.
///
/// The audio clause is not decoration: `burnin::build_ffmpeg_args` PASSES THE
/// SOURCE AUDIO THROUGH verbatim. A clip carrying a gain, a fade, or a track
/// fader that still took this shortcut would export at the original level with
/// no signal that anything was ignored — the exact "preview promises what the
/// export does not render" failure this codebase guards against.
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
        && item.has_default_audio()
        && track_of(project, item)
            .map(|t| t.effective_volume_db() == 0.0)
            .unwrap_or(true)
}

/// Build the FULL ffmpeg argument vector for a compose render. Pure — no IO.
/// `ass_file` is the (already-written) caption sidecar; `None` skips the
/// caption layer. This is the unit-tested heart of the compose path.
///
/// Fallible on purpose: a project holding an item kind the graph cannot render
/// (a `Text`/`Graphic` overlay) is REFUSED rather than rendered without it. The
/// builder is the narrowest waist every compose/proxy render passes through, so
/// putting the check here means no caller can produce a lying argv.
pub fn build_filter_complex(
    project: &Project,
    settings: &ComposeSettings,
    ass_file: Option<&str>,
    output: &str,
) -> AppResult<Vec<String>> {
    validate_composable(project)?;

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
        //
        // SPEED (R2): `timeline_end_ms`, the preview clock, the lane layout
        // and the filmstrip mapping have all honoured `TimelineItem.speed`
        // since it was introduced; this graph did not, so a sped-up clip would
        // have exported at 1× — running past the lane it occupies and
        // desyncing everything after it. `(PTS-STARTPTS)/speed` compresses the
        // clip's own time by exactly the factor `timeline_end_ms` divides by
        // (`effective_speed`, deliberately the same expression), and the
        // timeline shift is added AFTER the division so it is not scaled too.
        let speed = it.effective_speed();
        let base = if is_unit_speed(speed) {
            "PTS-STARTPTS".to_string()
        } else {
            format!("(PTS-STARTPTS)/{}", rate(speed))
        };
        let setpts = if it.timeline_start_ms > 0 {
            format!("setpts={base}+{}/TB", secs(it.timeline_start_ms))
        } else {
            format!("setpts={base}")
        };
        let mut chain: Vec<String> = vec![
            format!("trim=start={}:end={}", secs(it.in_ms), secs(it.out_ms)),
            setpts,
        ];
        // COLOUR first, GEOMETRY second (E6). Grading the source before it is
        // scaled/rotated/faded keeps `eq`/`hue` working on the clip's own
        // pixels: after `transform_filters` the stream may already be RGBA with
        // a premultiplied-looking alpha from the opacity mixer, where a luma
        // filter no longer means what the slider said. It is also the cheaper
        // order when the transform scales down. The Pixi preview applies the
        // same order (texture → colour matrix → sprite transform).
        crate::services::effects::effect_filters(&it.effects, &mut chain);
        transform_filters(&it.transform, &mut chain);
        nodes.push(format!("[{src}:v]{}[pv{n}]", chain.join(",")));
    }

    // Fold the processed streams onto the base canvas, low → high.
    let mut prev = format!("[{canvas_idx}:v]");
    for (n, it) in video_items.iter().enumerate() {
        let out = format!("[cx{n}]");
        // Placement is the SAME arithmetic in both branches — `Transform.x`/`.y`
        // are fractions of the output frame (mirrored by
        // `compositor/scene.ts::describeScene`, which positions the preview
        // layer at `round(width * t.x)` / `round(height * t.y)`).
        let x = (settings.width as f32 * it.transform.x).round() as i64;
        let y = (settings.height as f32 * it.transform.y).round() as i64;
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

            // Regression (seam-xfade-drops-transform): this branch used to make
            // the incoming clip full-frame with `scale={w}:{h}`, which BOTH
            // overwrote the transform's own `scale=iw*s:ih*s` and skipped the
            // `overlay` that carries x/y — so a clip with a transition exported
            // stretched full-frame while the preview showed it inset. xfade
            // does need a full-frame stream, but the honest way to get one is
            // to composite the (already transformed) clip onto its OWN
            // full-frame canvas at exactly the offsets the plain branch uses.
            // The clip's PTS is re-zeroed first: `[pv{n}]` sits at its timeline
            // position, and overlaying that onto a 0-based canvas would prepend
            // `timeline_start_ms` of black inside the incoming stream.
            //
            // Canvas length: the LONGER of the source span (`[pv{n}]` is a
            // plain `trim`, so its real length is `out_ms - in_ms`) and the
            // on-timeline span, never shorter than the clip — `shortest=1`
            // then ends the composite exactly with the clip.
            let clip_dur_ms = (it.out_ms - it.in_ms)
                .max(it.timeline_end_ms() - it.timeline_start_ms)
                .max(1);
            nodes.push(format!(
                "color=black:s={w}x{h}:r={fps}:d={d}[xc{n}]",
                w = settings.width,
                h = settings.height,
                fps = settings.fps,
                d = secs(clip_dur_ms),
            ));
            nodes.push(format!("[pv{n}]setpts=PTS-STARTPTS[xp{n}]"));
            nodes.push(format!(
                "[xc{n}][xp{n}]overlay={x}:{y}:shortest=1,{norm}[xb{n}]"
            ));
            nodes.push(format!(
                "[xa{n}][xb{n}]xfade=transition={kind}:duration={dur}:offset={off}{out}",
                kind = xfade_transition_name(&tr.kind),
                dur = secs(tr.duration_ms),
                off = secs(offset),
            ));
        } else {
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
    // Does anything in the mix push level UP? Only a positive total gain can
    // make the bus exceed what the sources already were; fades and cuts only
    // ever attenuate.
    let mut any_boost = false;
    for (n, it) in audio_items.iter().enumerate() {
        if !track_audible(project, it, any_solo) {
            continue;
        }
        let src = input_index(it.source_media_id.as_ref().unwrap());
        let delay = it.timeline_start_ms.max(0);

        let mut chain: Vec<String> = vec![
            format!("atrim=start={}:end={}", secs(it.in_ms), secs(it.out_ms)),
            "asetpts=PTS-STARTPTS".to_string(),
        ];

        // 1. SPEED, first — everything after it measures time on the clip's
        //    OWN, already-rescaled timeline, which is the timeline the lane
        //    (and `timeline_end_ms`) describes.
        chain.extend(atempo_chain(it.effective_speed()));

        // 2. LEVEL. The clip's gain and its track's fader are BOTH multipliers
        //    of the same signal, and dB add — so one `volume` node carrying
        //    the sum is exactly equivalent to two chained nodes, and cheaper.
        //    Emitted only when it does something, so an untouched clip's chain
        //    is unchanged from before R2.
        let track_db = track_of(project, it)
            .map(|t| t.effective_volume_db())
            .unwrap_or(0.0);
        let total_db = it.effective_gain_db() + track_db;
        if total_db != 0.0 {
            chain.push(format!("volume={}dB", db(total_db)));
            if total_db > 0.0 {
                any_boost = true;
            }
        }

        // 3. FADES, BEFORE `adelay`. This is the seam that would rot silently:
        //    `afade`'s `st=` is a timestamp on the stream it sees, and after
        //    `adelay` that stream has been pushed to `timeline_start_ms`. Put
        //    the fades after the delay and a fade-in written as `st=0` would
        //    ramp over the silence in FRONT of a clip that starts at 0:30 and
        //    the clip itself would begin at full level. Here the stream is
        //    still 0-based, so the clip's own start IS 0 and its own end IS
        //    `timeline_len_ms` (post-`atempo`). Pinned by
        //    `fade_out_is_positioned_against_the_clip_not_the_timeline`.
        let len_ms = it.timeline_len_ms();
        let fade_in = it.effective_fade_in_ms();
        let fade_out = it.effective_fade_out_ms();
        if fade_in > 0 {
            chain.push(format!("afade=t=in:st=0:d={}", secs(fade_in)));
        }
        if fade_out > 0 {
            chain.push(format!(
                "afade=t=out:st={}:d={}",
                secs(len_ms - fade_out),
                secs(fade_out),
            ));
        }

        // 4. PLACEMENT last.
        chain.push(format!("adelay={delay}|{delay}"));

        nodes.push(format!("[{src}:a]{}[pa{n}]", chain.join(",")));
        audio_labels.push(format!("[pa{n}]"));
    }

    // Headroom: see `BUS_LIMITER`. The limiter is inserted only where the bus
    // can genuinely exceed full scale — a summed mix, or a clip boosted above
    // unity. A single clip at or below unity gain therefore renders through
    // the same bare `anull` it always did, byte for byte.
    let needs_bus_limiter = audio_labels.len() >= 2 || any_boost;
    let audio_out = if audio_labels.is_empty() {
        None
    } else {
        let mix = if audio_labels.len() >= 2 {
            format!("amix=inputs={}:normalize=0", audio_labels.len())
        } else {
            "anull".to_string()
        };
        let bus = if needs_bus_limiter {
            format!("{mix},{BUS_LIMITER}")
        } else {
            mix
        };
        nodes.push(format!("{}{bus}[aout]", audio_labels.join("")));
        Some("[aout]".to_string())
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
    Ok(args)
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
) -> AppResult<Vec<String>> {
    let mut args = build_filter_complex(project, settings, ass_file, output)?;
    // Insert `-preset ultrafast` just before the trailing output path so the
    // x264 encoder runs at its lowest-latency profile.
    let out = args
        .pop()
        .expect("build_filter_complex always ends with output");
    args.push("-preset".into());
    args.push("ultrafast".into());
    args.push(out);
    Ok(args)
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
    // Refuse BEFORE any temp sidecar is written: a Text/Graphic overlay the
    // graph cannot render must abort the export, not vanish from it. (The pure
    // builder checks again — this is only about failing early and cleanly.)
    validate_composable(project)?;

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
    let args = build_filter_complex(project, settings, ass_ref, &output.to_string_lossy())
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&ass_path);
        })?;

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
    validate_composable(project)?;
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
    let args = build_proxy_args(project, &settings, ass_ref, &output.to_string_lossy())
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&ass_path);
        })?;

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

    /// Test shims. The real builders are FALLIBLE (they refuse a project with
    /// an item kind the graph cannot render — see `validate_composable`); every
    /// fixture below is composable, so the tests keep the infallible shape.
    /// A local item shadows the `use super::*` glob, so no call site changes.
    fn build_filter_complex(
        project: &Project,
        settings: &ComposeSettings,
        ass_file: Option<&str>,
        output: &str,
    ) -> Vec<String> {
        super::build_filter_complex(project, settings, ass_file, output)
            .expect("fixture must be composable")
    }

    fn build_proxy_args(
        project: &Project,
        settings: &ComposeSettings,
        ass_file: Option<&str>,
        output: &str,
    ) -> Vec<String> {
        super::build_proxy_args(project, settings, ass_file, output)
            .expect("fixture must be composable")
    }
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
            volume_db: 0.0,
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
            gain_db: 0.0,
            fade_in_ms: 0,
            fade_out_ms: 0,
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
        // Canvas and the xfade branch's own full-frame canvas agree on the
        // sanitized geometry…
        assert!(
            args.iter().any(|a| a.starts_with("color=black:s=642x482")),
            "canvas must use even dims: {args:?}"
        );
        let g = fc(&args);
        assert!(
            g.contains("color=black:s=642x482:"),
            "xfade branch canvas even: {g}"
        );
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

    // ── R2: gain / fades / headroom / speed ─────────────────────────────────

    /// One untouched clip must emit EXACTLY the chain it emitted before R2 —
    /// no `volume`, no `afade`, no `atempo`, no limiter. Anything else and the
    /// feature has silently changed every existing project's sound.
    #[test]
    fn an_untouched_single_clip_emits_the_pre_r2_audio_chain() {
        let p = project(
            vec![media("m1", "/a.mp4", true)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 0, 5000)],
            vec![],
        );
        let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));
        assert!(
            g.contains("[0:a]atrim=start=0.000:end=5.000,asetpts=PTS-STARTPTS,adelay=0|0[pa0]"),
            "got {g}"
        );
        assert!(g.contains("[pa0]anull[aout]"), "got {g}");
        for absent in ["volume=", "afade", "atempo", "alimiter"] {
            assert!(!g.contains(absent), "unexpected `{absent}` in {g}");
        }
    }

    /// The clip's gain and its track's fader are one multiplication of the
    /// same signal — dB add, so ONE node carries both.
    #[test]
    fn item_gain_and_track_volume_are_summed_into_one_volume_node() {
        let mut tr = track("a1", TrackKind::Audio, 0);
        tr.volume_db = -4.0;
        let mut it = item("i0", "a1", "m1", 0, 0, 5000);
        it.gain_db = -2.0;
        let p = project(
            vec![audio_media("m1", "/a.mp3")],
            vec![tr],
            vec![it],
            vec![],
        );
        let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));
        assert!(g.contains("volume=-6.0000dB"), "got {g}");
        assert_eq!(g.matches("volume=").count(), 1, "one node, not two: {g}");
    }

    /// Equal and opposite gain and fader cancel — and a chain that emitted
    /// `volume=0dB` anyway would be a pointless node in every project.
    #[test]
    fn a_cancelling_gain_and_fader_emit_no_volume_node() {
        let mut tr = track("a1", TrackKind::Audio, 0);
        tr.volume_db = 3.0;
        let mut it = item("i0", "a1", "m1", 0, 0, 5000);
        it.gain_db = -3.0;
        let p = project(
            vec![audio_media("m1", "/a.mp3")],
            vec![tr],
            vec![it],
            vec![],
        );
        let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));
        assert!(!g.contains("volume="), "got {g}");
    }

    /// THE fade seam. `afade`'s `st=` is a timestamp on the stream the filter
    /// sees. The fades are emitted BEFORE `adelay`, so that stream is 0-based
    /// and `st=` is measured from the CLIP's start — not from the timeline's.
    /// A clip at 0:10 that is 5 s long fades out at st=4.0, never at st=14.0
    /// (which would land past the end of the clip entirely) and never at
    /// st=9.0.
    #[test]
    fn fade_out_is_positioned_against_the_clip_not_the_timeline() {
        let mut it = item("i0", "a1", "m1", 10_000, 0, 5000);
        it.fade_in_ms = 1000;
        it.fade_out_ms = 1000;
        let p = project(
            vec![audio_media("m1", "/a.mp3")],
            vec![track("a1", TrackKind::Audio, 0)],
            vec![it],
            vec![],
        );
        let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));
        assert!(g.contains("afade=t=in:st=0:d=1.000"), "got {g}");
        assert!(
            g.contains("afade=t=out:st=4.000:d=1.000"),
            "fade-out must sit 1 s before the CLIP's end (4.000), got {g}"
        );
        assert!(
            !g.contains("st=14.000"),
            "fade landed on timeline time: {g}"
        );
        assert!(!g.contains("st=9.000"), "fade landed on timeline time: {g}");
        // …and the ordering that makes those numbers true.
        let fade_at = g.find("afade=t=in").unwrap();
        let delay_at = g.find("adelay=").unwrap();
        assert!(fade_at < delay_at, "fades must precede adelay: {g}");
    }

    /// A 2× clip occupies half the timeline, and `atempo` has already halved
    /// the stream by the time `afade` sees it — so the fade-out is placed
    /// against the SPED-UP length.
    #[test]
    fn fade_out_position_follows_speed() {
        let mut it = item("i0", "a1", "m1", 0, 0, 4000);
        it.speed = 2.0;
        it.fade_out_ms = 500;
        let p = project(
            vec![audio_media("m1", "/a.mp3")],
            vec![track("a1", TrackKind::Audio, 0)],
            vec![it],
            vec![],
        );
        let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));
        assert!(
            g.contains("afade=t=out:st=1.500:d=0.500"),
            "2 s of sped-up clip, fading out at 1.5 s; got {g}"
        );
        let tempo_at = g.find("atempo").unwrap();
        let fade_at = g.find("afade").unwrap();
        assert!(tempo_at < fade_at, "atempo must precede afade: {g}");
    }

    // ── headroom ────────────────────────────────────────────────────────────

    /// Two summed clips can exceed full scale, so the bus gets its limiter —
    /// but the mix stays `normalize=0`, because normalising would make every
    /// user-set gain depend on how many clips happen to overlap.
    #[test]
    fn a_summed_mix_keeps_normalize_0_and_gets_the_bus_limiter() {
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
        let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));
        assert!(g.contains("amix=inputs=2:normalize=0"), "got {g}");
        assert!(g.contains("alimiter="), "a summed bus must be limited: {g}");
        assert!(
            g.contains("level=disabled"),
            "alimiter's auto-level defaults to ON and would undo the user's gains: {g}"
        );
        assert!(g.contains("limit=0.891"), "ceiling below full scale: {g}");
        let mix_at = g.find("amix=").unwrap();
        let lim_at = g.find("alimiter=").unwrap();
        assert!(lim_at > mix_at, "the limiter belongs on the MIXED bus: {g}");
    }

    /// A boost above unity can clip on its own, with nothing to sum against.
    #[test]
    fn a_single_boosted_clip_gets_the_bus_limiter() {
        let mut it = item("i0", "a1", "m1", 0, 0, 5000);
        it.gain_db = 6.0;
        let p = project(
            vec![audio_media("m1", "/a.mp3")],
            vec![track("a1", TrackKind::Audio, 0)],
            vec![it],
            vec![],
        );
        let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));
        assert!(g.contains("volume=6.0000dB"), "got {g}");
        assert!(g.contains("alimiter="), "a boost can clip on its own: {g}");
    }

    /// Attenuation cannot clip, so it must not drag a limiter (and its 5 ms of
    /// lookahead) into the render.
    #[test]
    fn a_single_attenuated_clip_gets_no_limiter() {
        let mut it = item("i0", "a1", "m1", 0, 0, 5000);
        it.gain_db = -6.0;
        it.fade_in_ms = 500;
        let p = project(
            vec![audio_media("m1", "/a.mp3")],
            vec![track("a1", TrackKind::Audio, 0)],
            vec![it],
            vec![],
        );
        let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));
        assert!(g.contains("volume=-6.0000dB"), "got {g}");
        assert!(!g.contains("alimiter="), "nothing here can clip: {g}");
    }

    /// A muted track contributes nothing — so its gain must not drag a
    /// limiter onto a bus it is not even on.
    #[test]
    fn a_boost_on_a_muted_track_does_not_arm_the_limiter() {
        let mut tr = track("a1", TrackKind::Audio, 0);
        tr.muted = true;
        let mut it = item("i0", "a1", "m1", 0, 0, 5000);
        it.gain_db = 12.0;
        let mut tr2 = track("a2", TrackKind::Audio, 1);
        tr2.volume_db = -3.0;
        let p = project(
            vec![audio_media("m1", "/a.mp3"), audio_media("m2", "/b.mp3")],
            vec![tr, tr2],
            vec![it, item("i1", "a2", "m2", 0, 0, 5000)],
            vec![],
        );
        let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));
        assert!(!g.contains("alimiter="), "muted clip must not arm it: {g}");
        assert!(
            !g.contains("volume=12"),
            "muted clip must not be mixed: {g}"
        );
    }

    // ── speed ───────────────────────────────────────────────────────────────

    #[test]
    fn atempo_chain_stays_inside_ffmpegs_0_5_to_2_window() {
        assert!(atempo_chain(1.0).is_empty(), "unit speed emits nothing");
        assert_eq!(atempo_chain(2.0), vec!["atempo=2.000000"]);
        assert_eq!(atempo_chain(4.0), vec!["atempo=2.0", "atempo=2.000000"]);
        assert_eq!(atempo_chain(3.0), vec!["atempo=2.0", "atempo=1.500000"]);
        assert_eq!(atempo_chain(0.5), vec!["atempo=0.500000"]);
        assert_eq!(atempo_chain(0.25), vec!["atempo=0.5", "atempo=0.500000"]);

        // Every emitted factor must be one ffmpeg will accept, and the product
        // must be the requested speed — the two properties that matter.
        for s in [0.01f64, 0.1, 0.3, 0.75, 1.5, 2.5, 8.0, 17.3, 100.0] {
            let chain = atempo_chain(s);
            let mut product = 1.0f64;
            for node in &chain {
                let v: f64 = node.strip_prefix("atempo=").unwrap().parse().unwrap();
                assert!(
                    (0.5..=2.0).contains(&v),
                    "speed {s} emitted out-of-range `{node}`"
                );
                product *= v;
            }
            assert!(
                (product - s).abs() < s * 1e-4,
                "speed {s} chain {chain:?} multiplies to {product}"
            );
        }
    }

    /// Speed was modelled everywhere and IGNORED by the export: a 2× clip
    /// rendered at 1×, running past the lane it occupies. Both branches must
    /// now scale time.
    #[test]
    fn speed_scales_both_the_video_and_the_audio_branch() {
        let mut it = item("i0", "v1", "m1", 0, 0, 4000);
        it.speed = 2.0;
        let p = project(
            vec![media("m1", "/a.mp4", true)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![it],
            vec![],
        );
        let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));
        assert!(g.contains("setpts=(PTS-STARTPTS)/2.000000"), "got {g}");
        assert!(g.contains("atempo=2.000000"), "got {g}");
    }

    /// The timeline shift is added AFTER the division, so it is not scaled
    /// too — a 2× clip at 0:10 still starts at 0:10.
    #[test]
    fn speed_does_not_scale_the_timeline_offset() {
        let mut it = item("i0", "v1", "m1", 10_000, 0, 4000);
        it.speed = 2.0;
        let p = project(
            vec![media("m1", "/a.mp4", true)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![it],
            vec![],
        );
        let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));
        assert!(
            g.contains("setpts=(PTS-STARTPTS)/2.000000+10.000/TB"),
            "got {g}"
        );
        // The overlay window must agree with the halved length.
        assert!(g.contains("between(t,10.000,12.000)"), "got {g}");
    }

    #[test]
    fn unit_speed_emits_no_time_scaling_at_all() {
        let p = project(
            vec![media("m1", "/a.mp4", true)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![item("i0", "v1", "m1", 0, 0, 4000)],
            vec![],
        );
        let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));
        assert!(g.contains("setpts=PTS-STARTPTS"), "got {g}");
        assert!(!g.contains("/1.000000"), "got {g}");
        assert!(!g.contains("atempo"), "got {g}");
    }

    // ── the burn-in fast path must not swallow audio settings ───────────────

    /// `burnin::build_ffmpeg_args` passes source audio through VERBATIM. A
    /// project whose audio has been touched must therefore leave the fast
    /// path, or the export ignores every setting with no signal at all.
    #[test]
    fn touched_audio_takes_the_project_off_the_simple_burn_in_path() {
        let baseline = baseline_project();
        assert!(is_simple_timeline(&baseline), "baseline is simple");

        let mut gained = baseline.clone();
        gained.timeline_items[0].gain_db = -6.0;
        assert!(
            !is_simple_timeline(&gained),
            "a clip gain must not be swallowed"
        );

        let mut faded = baseline.clone();
        faded.timeline_items[0].fade_in_ms = 500;
        assert!(
            !is_simple_timeline(&faded),
            "a fade-in must not be swallowed"
        );

        let mut faded_out = baseline.clone();
        faded_out.timeline_items[0].fade_out_ms = 500;
        assert!(
            !is_simple_timeline(&faded_out),
            "a fade-out must not be swallowed"
        );

        let mut fader = baseline.clone();
        fader.tracks[0].volume_db = -3.0;
        assert!(
            !is_simple_timeline(&fader),
            "a track fader must not be swallowed"
        );
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

    // ── Curated effects (E6) ────────────────────────────────────────────────

    fn fx(kind: &str, params: serde_json::Value) -> crate::model::Effect {
        crate::model::Effect {
            id: format!("fx-{kind}"),
            kind: kind.into(),
            params,
            enabled: true,
        }
    }

    /// The per-item chain for a project holding exactly one clip.
    fn one_item_chain(it: TimelineItem) -> String {
        let p = project(
            vec![media("m1", "/a.mp4", false)],
            vec![track("v1", TrackKind::Video, 0)],
            vec![it],
            vec![],
        );
        let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));
        g.split(';')
            .find(|n| n.contains("[pv0]"))
            .expect("the item's own node")
            .to_string()
    }

    #[test]
    fn curated_effects_land_in_the_item_chain() {
        let mut it = item("i0", "v1", "m1", 0, 0, 5000);
        it.effects = vec![
            fx("brightness", serde_json::json!({ "amount": 0.2 })),
            fx("saturation", serde_json::json!({ "amount": 1.4 })),
        ];
        let node = one_item_chain(it);
        assert!(node.contains("eq=brightness=0.2"), "got {node}");
        assert!(node.contains("eq=saturation=1.4"), "got {node}");
    }

    #[test]
    fn effects_are_applied_before_the_geometric_transform() {
        // Colour first, geometry second — see the comment at the call site.
        let mut it = item("i0", "v1", "m1", 0, 0, 5000);
        it.effects = vec![fx("grayscale", serde_json::json!({}))];
        it.transform = Transform {
            scale: 0.5,
            ..Transform::default()
        };
        let node = one_item_chain(it);
        let fx_at = node.find("hue=s=0").expect("effect emitted");
        let tf_at = node.find("scale=iw*0.5").expect("transform emitted");
        assert!(fx_at < tf_at, "effects must precede the transform: {node}");
    }

    #[test]
    fn disabled_and_unknown_effects_leave_the_graph_untouched() {
        // The seam that matters: an effect kind we do not render must produce
        // BYTE-IDENTICAL argv to no effect at all — never an invented filter.
        let base = one_item_chain(item("i0", "v1", "m1", 0, 0, 5000));

        let mut off = item("i0", "v1", "m1", 0, 0, 5000);
        let mut disabled = fx("brightness", serde_json::json!({ "amount": 0.9 }));
        disabled.enabled = false;
        off.effects = vec![
            disabled,
            fx("bloom", serde_json::json!({ "radius": 4 })),
            fx("contrast", serde_json::json!({ "amount": 1.0 })), // neutral
        ];
        assert_eq!(one_item_chain(off), base);
    }

    #[test]
    fn an_enabled_effect_takes_the_project_off_the_simple_burn_in_path() {
        // `is_pristine_primary_item` refuses any enabled effect — deliberately
        // stricter than "emits a filter", so the general (correct) composite
        // path is what renders anything the burn-in shortcut wasn't built for.
        let mut p = baseline_project();
        assert!(is_simple_timeline(&p));
        p.timeline_items[0].effects = vec![fx("grayscale", serde_json::json!({}))];
        assert!(!is_simple_timeline(&p));
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

    // ── Text/Graphic overlays: refused loudly, never dropped ────────────────

    /// A standalone text overlay — the exact shape `op_add_text_item` builds:
    /// `kind: Text`, `source_media_id: None`.
    fn text_item(id: &str, track_id: &str, start: i64, dur: i64) -> TimelineItem {
        TimelineItem {
            id: id.into(),
            track_id: track_id.into(),
            kind: TimelineItemKind::Text,
            source_media_id: None,
            in_ms: 0,
            out_ms: dur,
            timeline_start_ms: start,
            speed: 1.0,
            gain_db: 0.0,
            fade_in_ms: 0,
            fade_out_ms: 0,
            transform: Transform::default(),
            effects: vec![],
            transition_in: None,
            text: Some(crate::model::TextSpec {
                text: "Velkommen".into(),
                style_id: None,
            }),
            enabled: true,
            locked: false,
        }
    }

    /// Regression (seam-text-item-swallowed-by-simple-path): a text overlay is
    /// neither `is_visual` nor `has_audio`, so it used to leave a baseline
    /// import "simple" — the burn-in shortcut then rendered the video alone and
    /// the text vanished with ZERO signal. It must disqualify the fast path.
    #[test]
    fn text_item_disqualifies_the_simple_path() {
        let mut p = baseline_project();
        assert!(is_simple_timeline(&p), "precondition: baseline is simple");
        p.tracks.push(track("o1", TrackKind::Overlay, 2));
        p.timeline_items.push(text_item("tx1", "o1", 1000, 2000));
        assert!(
            !is_simple_timeline(&p),
            "a text overlay must never be swallowed by the burn-in shortcut"
        );
    }

    /// …and the composite path it now routes to must REFUSE, not omit.
    #[test]
    fn composing_a_text_item_errors_and_names_the_kind() {
        let mut p = baseline_project();
        p.tracks.push(track("o1", TrackKind::Overlay, 2));
        p.timeline_items.push(text_item("tx1", "o1", 1000, 2000));

        let err = super::build_filter_complex(&p, &settings(), None, "out.mp4")
            .expect_err("a text overlay must not compose silently");
        assert_eq!(err.code(), "validation", "got {err}");
        let msg = err.to_string();
        assert!(msg.contains("Text"), "the error must name the kind: {msg}");
        assert!(msg.contains("tx1"), "the error must name the item: {msg}");

        // The proxy builder and both spawn entry points share the check.
        assert!(super::build_proxy_args(&p, &settings(), None, "out.mp4").is_err());
        assert!(validate_composable(&p).is_err());
    }

    #[test]
    fn graphic_items_are_refused_and_named_too() {
        let mut p = baseline_project();
        p.tracks.push(track("o1", TrackKind::Overlay, 2));
        let mut g = text_item("gx1", "o1", 0, 1000);
        g.kind = TimelineItemKind::Graphic;
        g.text = None;
        p.timeline_items.push(g);

        assert!(!is_simple_timeline(&p));
        let msg = validate_composable(&p)
            .expect_err("a graphic overlay must not compose silently")
            .to_string();
        assert!(
            msg.contains("Graphic"),
            "the error must name the kind: {msg}"
        );
    }

    /// Only content that WOULD be seen counts. A disabled overlay, or one on a
    /// hidden track, is absent from the preview too — there is nothing to lose,
    /// so it must neither block the fast path nor fail the render.
    #[test]
    fn invisible_text_items_neither_block_nor_break() {
        let mut disabled = baseline_project();
        disabled.tracks.push(track("o1", TrackKind::Overlay, 2));
        let mut off = text_item("tx1", "o1", 1000, 2000);
        off.enabled = false;
        disabled.timeline_items.push(off);
        assert!(
            is_simple_timeline(&disabled),
            "disabled overlay stays simple"
        );
        assert!(validate_composable(&disabled).is_ok());

        let mut hidden = baseline_project();
        let mut t = track("o1", TrackKind::Overlay, 2);
        t.enabled = false;
        hidden.tracks.push(t);
        hidden
            .timeline_items
            .push(text_item("tx1", "o1", 1000, 2000));
        assert!(
            is_simple_timeline(&hidden),
            "overlay on a hidden track stays simple"
        );
        assert!(validate_composable(&hidden).is_ok());
    }

    /// Cross-layer: whatever the REAL `add_text_item` op produces must hit the
    /// refusal, not the shortcut. Pins the op ↔ compose seam rather than this
    /// module's own fixture.
    #[test]
    fn real_add_text_item_output_is_refused_not_dropped() {
        let mut p = baseline_project();
        p.tracks.push(track("o1", TrackKind::Overlay, 2));
        let p = crate::services::timeline_ops::add_text_item(
            &p,
            "tx-real".into(),
            "o1",
            1_000,
            2_000,
            "Velkommen".into(),
        )
        .expect("adding a text overlay is a legal edit");

        assert!(
            !is_simple_timeline(&p),
            "the op's own output must leave the burn-in fast path"
        );
        let msg = super::build_filter_complex(&p, &settings(), None, "out.mp4")
            .expect_err("the op's own output must be refused, not rendered without the text")
            .to_string();
        assert!(msg.contains("Text"), "got {msg}");
    }

    // ── Transitions must not eat the transform ──────────────────────────────

    /// Regression (seam-xfade-drops-transform): the xfade branch used to emit
    /// `[pv{n}]scale={W}:{H}` — overwriting the transform's own
    /// `scale=iw*s:ih*s` and skipping the `overlay` that carries x/y, so a
    /// scaled + offset clip exported full-frame. Pixel proof lives in
    /// tests/compose_transition_transform.rs; this pins the graph shape.
    #[test]
    fn transition_branch_keeps_the_transform_scale_and_offset() {
        let mut second = item("i1", "v1", "m1", 4000, 0, 4000);
        second.transform = Transform {
            x: 0.55,
            y: 0.10,
            scale: 0.4,
            ..Transform::default()
        };
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
        let g = fc(&build_filter_complex(&p, &settings(), None, "out.mp4"));

        // The transform's own scale survives…
        assert!(
            g.contains("scale=iw*0.4:ih*0.4"),
            "transform scale lost: {g}"
        );
        // …it is NOT overwritten by a fit-to-frame scale…
        assert!(
            !g.contains("scale=1920:1080"),
            "the fit-to-frame scale is back — it eats the transform: {g}"
        );
        // …and the placement reaches an overlay: 1920*0.55 = 1056, 1080*0.10 = 108.
        assert!(
            g.contains("overlay=1056:108"),
            "transitioned clip must still be placed at its x/y: {g}"
        );
        // The incoming stream is made full-frame by its OWN canvas, so xfade
        // still sees two same-size inputs.
        assert!(g.contains("color=black:s=1920x1080:"), "got {g}");
        assert!(g.contains("xfade=transition=fade:"), "got {g}");
    }

    /// DRIFT GUARD: the two branches place an identically-transformed clip with
    /// the SAME overlay offsets. If one branch's geometry is edited alone, the
    /// strings stop matching.
    #[test]
    fn both_branches_emit_the_same_overlay_offsets() {
        let tf = Transform {
            x: 0.55,
            y: 0.10,
            scale: 0.4,
            ..Transform::default()
        };
        let make = |with_transition: bool| {
            let mut second = item("i1", "v1", "m1", 4000, 0, 4000);
            second.transform = tf.clone();
            if with_transition {
                second.transition_in = Some(Transition {
                    kind: "fade".into(),
                    duration_ms: 1000,
                });
            }
            let p = project(
                vec![media("m1", "/a.mp4", false)],
                vec![track("v1", TrackKind::Video, 0)],
                vec![item("i0", "v1", "m1", 0, 0, 4000), second],
                vec![],
            );
            fc(&build_filter_complex(&p, &settings(), None, "out.mp4"))
        };
        let with_tr = make(true);
        let without_tr = make(false);
        let offsets = |g: &str| -> Vec<String> {
            g.split("overlay=")
                .skip(1)
                .map(|tail| {
                    tail.split([',', ':', ';'])
                        .take(2)
                        .collect::<Vec<_>>()
                        .join(":")
                })
                .collect()
        };
        assert_eq!(
            offsets(&with_tr).last(),
            offsets(&without_tr).last(),
            "the transition branch must place the clip exactly where the plain \
             branch does\nwith:    {with_tr}\nwithout: {without_tr}"
        );
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
