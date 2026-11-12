//! Sermon Highlight Reel Studio — batch-produce N social clips from one
//! captioned talk. SundayEdit's "one sermon → a week of social posts" feature.
//!
//! The flow, mirroring the existing AI seams:
//!   1. The operator approves a STORYBOARD of clips. Each clip comes either
//!      from the AI clip planner (`services::llm::clips`, behind the keyless
//!      LLM seam) OR — with no API key — from `heuristic_clips`, a pure
//!      pause-based segmentation of the captions. Either way the operator
//!      reviews/edits before anything renders.
//!   2. `build_render_plan` FANS OUT the storyboard: one vertical,
//!      caption-burned render item per (clip × platform). Pure → fully tested.
//!   3. The command layer drives the batch render queue with progress/cancel,
//!      reusing the existing per-clip `burnin::render_clip` + the platform
//!      `ExportPreset` catalog.
//!
//! Everything that decides WHAT gets rendered (clip selection heuristic,
//! fan-out, output paths, filename sanitising, progress maths) lives here as
//! pure functions with unit tests. The actual ffmpeg spawn stays in `burnin`.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use ts_rs::TS;

use crate::model::{Clip, Project};
use crate::services::export_presets::{ExportPreset, catalog};
use crate::services::llm::clips::ClipPlan;

// ── Heuristic clip detection (keyless fallback) ─────────────────────────────────

/// Tuning for the keyless, pause-based clip heuristic. A "gap" between two
/// captions longer than `gap_threshold_ms` is treated as a topic boundary.
#[derive(Debug, Clone, Copy)]
pub struct HeuristicParams {
    /// A silence longer than this between captions starts a new segment.
    pub gap_threshold_ms: i64,
    /// Discard segments shorter than this — too short to be a clip.
    pub min_clip_ms: i64,
    /// Cap a segment at this length so a long monologue still yields a clip.
    pub max_clip_ms: i64,
    /// Never propose more than this many clips.
    pub max_clips: usize,
}

impl Default for HeuristicParams {
    fn default() -> Self {
        Self {
            // ~700 ms of silence is a natural sentence/paragraph break in
            // speech. Long enough to ignore between-word micro-pauses.
            gap_threshold_ms: 700,
            min_clip_ms: 12_000, // a usable clip is at least ~12 s
            max_clip_ms: 90_000, // platform-friendly upper bound
            max_clips: 8,
        }
    }
}

/// A first-line title from a caption's text: the leading words, trimmed to a
/// sensible on-screen length, no trailing punctuation. Pure.
pub fn title_from_text(text: &str, max_chars: usize) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() <= max_chars {
        return cleaned.trim_end_matches(['.', ',', ';', ':']).to_string();
    }
    // Cut on a word boundary at or before max_chars.
    let mut out = String::new();
    for word in cleaned.split(' ') {
        let candidate = if out.is_empty() {
            word.to_string()
        } else {
            format!("{out} {word}")
        };
        if candidate.chars().count() > max_chars {
            break;
        }
        out = candidate;
    }
    if out.is_empty() {
        // A single very long word — hard-truncate.
        out = cleaned.chars().take(max_chars).collect();
    }
    out.trim_end_matches(['.', ',', ';', ':']).to_string()
}

/// Propose clips from caption-gap segmentation, with NO model. Captions are
/// walked in time order; a gap longer than `params.gap_threshold_ms` (or a
/// segment exceeding `max_clip_ms`) closes the current segment. Segments
/// shorter than `min_clip_ms` are dropped. Ranges are clamped to the video.
/// Deterministic — same project always yields the same storyboard.
pub fn heuristic_clips(project: &Project, params: HeuristicParams) -> Vec<Clip> {
    // Only captions with real words, in time order.
    let mut caps: Vec<&crate::model::Caption> = project
        .captions
        .iter()
        .filter(|c| !c.words.is_empty())
        .collect();
    caps.sort_by_key(|c| c.start_ms);

    let duration = project.video_duration_ms.max(0);

    // Group into segments.
    let mut segments: Vec<Vec<&crate::model::Caption>> = Vec::new();
    let mut current: Vec<&crate::model::Caption> = Vec::new();
    let mut seg_start: i64 = 0;
    for c in caps {
        if let Some(prev) = current.last() {
            let gap = c.start_ms - prev.end_ms;
            let span = c.end_ms - seg_start;
            if gap >= params.gap_threshold_ms || span > params.max_clip_ms {
                segments.push(std::mem::take(&mut current));
            }
        }
        if current.is_empty() {
            seg_start = c.start_ms;
        }
        current.push(c);
    }
    if !current.is_empty() {
        segments.push(current);
    }

    let mut clips: Vec<Clip> = Vec::new();
    for seg in segments {
        if seg.is_empty() {
            continue;
        }
        let start = seg.iter().map(|c| c.start_ms).min().unwrap_or(0).max(0);
        let mut end = seg.iter().map(|c| c.end_ms).max().unwrap_or(0);
        if duration > 0 {
            end = end.min(duration);
        }
        if end <= start {
            continue;
        }
        if end - start < params.min_clip_ms {
            continue;
        }
        // Trim over-long segments to max_clip_ms (keep the opening — the hook).
        if end - start > params.max_clip_ms {
            end = start + params.max_clip_ms;
        }
        let caption_ids: Vec<String> = seg
            .iter()
            .filter(|c| c.start_ms < end) // keep only captions inside the trimmed window
            .map(|c| c.id.clone())
            .collect();
        if caption_ids.is_empty() {
            continue;
        }
        let first_text = seg.first().map(|c| c.text()).unwrap_or_default();
        let idx = clips.len();
        clips.push(Clip {
            id: format!("clip:{idx}"),
            title: title_from_text(&first_text, 60),
            hook: String::new(),
            caption_ids,
            start_ms: start,
            end_ms: end,
        });
        if clips.len() >= params.max_clips {
            break;
        }
    }
    clips
}

/// Build a keyless storyboard: a heuristic clip plan with an empty summary,
/// in the exact `ClipPlan` shape the operator reviews for the AI path. This is
/// the graceful no-key degradation — never an error, never a blocked flow.
pub fn heuristic_plan(project: &Project, params: HeuristicParams) -> ClipPlan {
    ClipPlan {
        talk_summary: String::new(),
        clips: heuristic_clips(project, params),
    }
}

// ── Fan-out: storyboard → render plan ───────────────────────────────────────────

/// Which platforms a storyboard renders to. Resolved from the export-preset
/// catalog by id; unknown ids are dropped. Defaults to the vertical social set.
pub fn resolve_presets(preset_ids: &[String]) -> Vec<ExportPreset> {
    let all = catalog();
    if preset_ids.is_empty() {
        return default_vertical_presets();
    }
    let wanted: HashSet<&str> = preset_ids.iter().map(|s| s.as_str()).collect();
    all.into_iter()
        .filter(|p| wanted.contains(p.id.as_str()))
        .collect()
}

/// The default platform set for a highlight reel: the vertical (9:16) social
/// formats. A sermon highlight reel is portrait-first.
pub fn default_vertical_presets() -> Vec<ExportPreset> {
    use crate::services::export_presets::Aspect;
    catalog()
        .into_iter()
        .filter(|p| matches!(p.aspect, Aspect::Portrait))
        .collect()
}

/// One concrete render job: a specific clip burned in at a specific platform
/// preset, written to `output_path`. The atomic unit of the batch queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/RenderItem.ts")]
pub struct RenderItem {
    /// Stable id: `<clip_id>__<preset_id>`. Used to report per-item progress.
    pub id: String,
    pub clip: Clip,
    pub preset: ExportPreset,
    /// Absolute output path (under the chosen output directory).
    pub output_path: String,
}

/// The full fanned-out plan the operator confirms before "Render all".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/RenderPlan.ts")]
pub struct RenderPlan {
    pub items: Vec<RenderItem>,
    /// Total = clips × platforms.
    #[ts(type = "number")]
    pub total: u32,
}

/// Sanitise a clip title into a filesystem-safe slug for the output filename.
/// Lowercased, spaces → `-`, only `[a-z0-9-]`, collapsed/trimmed dashes, and
/// length-capped. Empty input yields `"clip"`. Pure.
pub fn slugify(title: &str, max_len: usize) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in title.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            prev_dash = false;
        } else if matches!(
            ch,
            ' ' | '-' | '_' | '.' | ',' | '/' | '\\' | ':' | ';' | '\t' | '\n'
        ) && !prev_dash
            && !out.is_empty()
        {
            out.push('-');
            prev_dash = true;
        }
        // Drop everything else (incl. non-ASCII like æøå) — keeps paths portable.
        if out.chars().count() >= max_len {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "clip".to_string()
    } else {
        trimmed
    }
}

/// The output filename for a render item, unique within the batch:
/// `<NN>-<slug>__<preset>.mp4` where NN is the 1-based clip index zero-padded.
/// Pure. The numeric prefix guarantees uniqueness even if two clips slugify
/// to the same string.
pub fn output_filename(clip_index: usize, clip: &Clip, preset: &ExportPreset) -> String {
    let slug = slugify(&clip.title, 40);
    let preset_slug = preset
        .id
        .strip_prefix("export:")
        .unwrap_or(&preset.id)
        .to_string();
    format!("{:02}-{}__{}.mp4", clip_index + 1, slug, preset_slug)
}

/// Join an output directory and a filename with the right separator, without
/// pulling in `Path` (kept pure/testable). Trailing separators on `dir` are
/// handled.
fn join_path(dir: &str, file: &str) -> String {
    let sep = if dir.contains('\\') && !dir.contains('/') {
        '\\'
    } else {
        '/'
    };
    let trimmed = dir.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        file.to_string()
    } else {
        format!("{trimmed}{sep}{file}")
    }
}

/// FAN OUT a reviewed storyboard into concrete render items: every clip × every
/// resolved platform preset. Output paths land under `output_dir`. Pure — this
/// is the tested heart of the batch planner. `preset_ids` empty → default
/// vertical social set.
pub fn build_render_plan(clips: &[Clip], preset_ids: &[String], output_dir: &str) -> RenderPlan {
    let presets = resolve_presets(preset_ids);
    let mut items: Vec<RenderItem> = Vec::new();
    for (ci, clip) in clips.iter().enumerate() {
        for preset in &presets {
            let file = output_filename(ci, clip, preset);
            items.push(RenderItem {
                id: format!("{}__{}", clip.id, preset.id),
                clip: clip.clone(),
                preset: preset.clone(),
                output_path: join_path(output_dir, &file),
            });
        }
    }
    let total = items.len() as u32;
    RenderPlan { items, total }
}

// ── Batch render progress ───────────────────────────────────────────────────────

/// Streamed to the UI as the batch render queue advances. Mirrors the
/// `DownloadProgress` shape (completed/total + fraction) plus the current item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/ReelRenderProgress.ts")]
pub struct ReelRenderProgress {
    /// Items finished (succeeded or failed) so far.
    pub completed: u32,
    /// Total items in the batch.
    pub total: u32,
    /// 0..1, or null when total is 0.
    pub fraction: Option<f32>,
    /// The item id currently rendering (or just finished), for the UI to
    /// highlight a row. `None` once the batch is done.
    pub current_item_id: Option<String>,
    /// Items that failed to render (e.g. ffmpeg missing). Reported so a partial
    /// success is still surfaced — one bad clip doesn't sink the batch.
    pub failed: u32,
}

/// Completion fraction for the batch, `None` when total is 0. Pure.
pub fn render_fraction(completed: u32, total: u32) -> Option<f32> {
    if total == 0 {
        None
    } else {
        Some(completed as f32 / total as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Caption, Style, Word};

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

    fn caption_text(id: &str, start: i64, end: i64, text: &str) -> Caption {
        let words: Vec<Word> = text
            .split_whitespace()
            .map(|w| Word::new(w, start, end, 90.0))
            .collect();
        Caption {
            id: id.into(),
            start_ms: start,
            end_ms: end,
            words,
            speaker_id: None,
            style_id: None,
            notes: None,
            ai_generated: true,
            last_edited_at: 0,
            track_id: None,
        }
    }

    fn project_with(captions: Vec<Caption>, duration_ms: i64) -> Project {
        Project {
            id: "p".into(),
            name: "t".into(),
            video_path: "/x".into(),
            video_content_hash: "h".into(),
            video_duration_ms: duration_ms,
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
            media: vec![],
            tracks: vec![],
            timeline_items: vec![],
        }
    }

    fn clip(id: &str, title: &str, start: i64, end: i64) -> Clip {
        Clip {
            id: id.into(),
            title: title.into(),
            hook: String::new(),
            caption_ids: vec!["c0".into()],
            start_ms: start,
            end_ms: end,
        }
    }

    // ── title_from_text ──────────────────────────────────────────────────────

    #[test]
    fn title_short_text_passes_through_trimmed() {
        assert_eq!(
            title_from_text("  Guds nåde er nok.  ", 60),
            "Guds nåde er nok"
        );
    }

    #[test]
    fn title_truncates_on_word_boundary() {
        let t = title_from_text(
            "Dette er en ganske lang setning som overgår grensen helt klart",
            20,
        );
        assert!(t.chars().count() <= 20, "got {t:?}");
        // must not cut mid-word
        assert!(!t.ends_with("setn"));
        assert!(t.starts_with("Dette er"));
    }

    #[test]
    fn title_hard_truncates_single_long_word() {
        let t = title_from_text("Supercalifragilisticexpialidocious", 10);
        assert_eq!(t.chars().count(), 10);
    }

    // ── heuristic_clips ────────────────────────────────────────────────────────

    #[test]
    fn heuristic_splits_on_long_gaps() {
        // Two segments separated by a 2s gap; each ~15s so both survive min.
        let caps = vec![
            caption("c0", 0, 5000),
            caption("c1", 5000, 15000),
            // 2s gap here (15000 → 17000)
            caption("c2", 17000, 22000),
            caption("c3", 22000, 32000),
        ];
        let clips = heuristic_clips(&project_with(caps, 32000), HeuristicParams::default());
        assert_eq!(clips.len(), 2, "a >700ms gap should split into 2 clips");
        assert_eq!(clips[0].start_ms, 0);
        assert_eq!(clips[0].end_ms, 15000);
        assert_eq!(clips[1].start_ms, 17000);
        assert_eq!(clips[1].end_ms, 32000);
    }

    #[test]
    fn heuristic_drops_too_short_segments() {
        // One real clip + a tiny 1s trailing blip after a gap.
        let caps = vec![
            caption("c0", 0, 14000),
            // gap
            caption("c1", 20000, 21000), // 1s — below min_clip_ms (12s)
        ];
        let clips = heuristic_clips(&project_with(caps, 21000), HeuristicParams::default());
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].end_ms, 14000);
    }

    #[test]
    fn heuristic_caps_long_monologue_at_max() {
        // One uninterrupted 5-min block, no gaps → trimmed to max_clip_ms.
        let caps: Vec<Caption> = (0..30)
            .map(|i| caption(&format!("c{i}"), i * 10_000, i * 10_000 + 10_000))
            .collect();
        let clips = heuristic_clips(&project_with(caps, 300_000), HeuristicParams::default());
        assert!(!clips.is_empty());
        // every clip is no longer than max_clip_ms
        for c in &clips {
            assert!(c.end_ms - c.start_ms <= 90_000, "clip {c:?} exceeds max");
        }
    }

    #[test]
    fn heuristic_respects_max_clips() {
        let params = HeuristicParams {
            max_clips: 2,
            min_clip_ms: 1000,
            ..HeuristicParams::default()
        };
        // Many short, gap-separated segments.
        let mut caps = Vec::new();
        for i in 0..10 {
            let base = i as i64 * 10_000;
            caps.push(caption(&format!("a{i}"), base, base + 2000));
        }
        let clips = heuristic_clips(&project_with(caps, 110_000), params);
        assert!(clips.len() <= 2);
    }

    #[test]
    fn heuristic_clamps_end_to_duration() {
        let caps = vec![caption("c0", 0, 30_000)];
        let clips = heuristic_clips(&project_with(caps, 20_000), HeuristicParams::default());
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].end_ms, 20_000);
    }

    #[test]
    fn heuristic_title_comes_from_first_caption_text() {
        let caps = vec![
            caption_text("c0", 0, 7000, "Dette er hovedpoenget i talen i dag"),
            caption_text("c1", 7000, 14000, "og mer tekst her etterpå"),
        ];
        let clips = heuristic_clips(&project_with(caps, 14000), HeuristicParams::default());
        assert_eq!(clips.len(), 1);
        assert!(clips[0].title.starts_with("Dette er hovedpoenget"));
    }

    #[test]
    fn heuristic_empty_project_yields_no_clips() {
        let clips = heuristic_clips(&project_with(vec![], 0), HeuristicParams::default());
        assert!(clips.is_empty());
    }

    #[test]
    fn heuristic_plan_has_empty_summary() {
        let caps = vec![caption("c0", 0, 14000)];
        let plan = heuristic_plan(&project_with(caps, 14000), HeuristicParams::default());
        assert_eq!(plan.talk_summary, "");
        assert_eq!(plan.clips.len(), 1);
    }

    // ── slugify ──────────────────────────────────────────────────────────────

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Guds nåde er nok!", 40), "guds-nde-er-nok");
    }

    #[test]
    fn slugify_collapses_and_trims_dashes() {
        assert_eq!(slugify("  --Hello,   World--  ", 40), "hello-world");
    }

    #[test]
    fn slugify_empty_falls_back_to_clip() {
        assert_eq!(slugify("", 40), "clip");
        assert_eq!(slugify("æøå", 40), "clip"); // all non-ASCII dropped
    }

    #[test]
    fn slugify_respects_max_len() {
        let s = slugify("aaaaaaaaaa bbbbbbbbbb cccccccccc", 10);
        assert!(s.chars().count() <= 10, "got {s:?}");
    }

    // ── output_filename ────────────────────────────────────────────────────────

    #[test]
    fn output_filename_has_index_slug_and_preset() {
        let presets = default_vertical_presets();
        let p = presets.iter().find(|p| p.id == "export:reels").unwrap();
        let f = output_filename(0, &clip("clip:0", "Guds nåde", 0, 5000), p);
        assert_eq!(f, "01-guds-nde__reels.mp4");
    }

    #[test]
    fn output_filename_index_is_one_based_padded() {
        let presets = default_vertical_presets();
        let p = &presets[0];
        let f = output_filename(9, &clip("clip:9", "Tiende", 0, 5000), p);
        assert!(f.starts_with("10-"), "got {f}");
    }

    // ── resolve_presets / defaults ───────────────────────────────────────────────

    #[test]
    fn empty_preset_ids_default_to_vertical() {
        let presets = resolve_presets(&[]);
        assert!(!presets.is_empty());
        use crate::services::export_presets::Aspect;
        assert!(presets.iter().all(|p| matches!(p.aspect, Aspect::Portrait)));
    }

    #[test]
    fn resolve_presets_filters_by_id_and_drops_unknown() {
        let presets = resolve_presets(&["export:reels".into(), "export:nope".into()]);
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].id, "export:reels");
    }

    // ── build_render_plan (fan-out) ────────────────────────────────────────────

    #[test]
    fn fan_out_is_clips_times_platforms() {
        let clips = vec![
            clip("clip:0", "A", 0, 5000),
            clip("clip:1", "B", 6000, 12000),
        ];
        let plan = build_render_plan(
            &clips,
            &["export:reels".into(), "export:tiktok".into()],
            "/out",
        );
        assert_eq!(plan.total, 4); // 2 clips × 2 platforms
        assert_eq!(plan.items.len(), 4);
    }

    #[test]
    fn fan_out_item_ids_are_unique() {
        let clips = vec![
            clip("clip:0", "A", 0, 5000),
            clip("clip:1", "B", 6000, 12000),
        ];
        let plan = build_render_plan(
            &clips,
            &["export:reels".into(), "export:youtube_shorts".into()],
            "/out",
        );
        let ids: HashSet<&str> = plan.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids.len(),
            plan.items.len(),
            "render item ids must be unique"
        );
    }

    #[test]
    fn fan_out_output_paths_are_unique_under_dir() {
        let clips = vec![
            clip("clip:0", "A", 0, 5000),
            clip("clip:1", "B", 6000, 12000),
        ];
        let plan = build_render_plan(
            &clips,
            &["export:reels".into(), "export:youtube_shorts".into()],
            "/out",
        );
        let paths: HashSet<&str> = plan.items.iter().map(|i| i.output_path.as_str()).collect();
        assert_eq!(paths.len(), plan.items.len(), "output paths must be unique");
        assert!(
            plan.items
                .iter()
                .all(|i| i.output_path.starts_with("/out/"))
        );
    }

    #[test]
    fn fan_out_carries_clip_range_and_preset() {
        let clips = vec![clip("clip:0", "Hook", 2000, 9000)];
        let plan = build_render_plan(&clips, &["export:reels".into()], "/out");
        assert_eq!(plan.items[0].clip.start_ms, 2000);
        assert_eq!(plan.items[0].clip.end_ms, 9000);
        assert_eq!(plan.items[0].preset.id, "export:reels");
    }

    #[test]
    fn fan_out_empty_clips_yields_empty_plan() {
        let plan = build_render_plan(&[], &["export:reels".into()], "/out");
        assert_eq!(plan.total, 0);
        assert!(plan.items.is_empty());
    }

    #[test]
    fn fan_out_windows_path_uses_backslash() {
        let clips = vec![clip("clip:0", "A", 0, 5000)];
        let plan = build_render_plan(&clips, &["export:reels".into()], "C:\\Users\\me\\out");
        assert!(
            plan.items[0]
                .output_path
                .starts_with("C:\\Users\\me\\out\\"),
            "got {}",
            plan.items[0].output_path
        );
    }

    // ── render_fraction ──────────────────────────────────────────────────────

    #[test]
    fn fraction_progresses() {
        assert_eq!(render_fraction(0, 4), Some(0.0));
        assert_eq!(render_fraction(2, 4), Some(0.5));
        assert_eq!(render_fraction(4, 4), Some(1.0));
    }

    #[test]
    fn fraction_none_when_empty() {
        assert_eq!(render_fraction(0, 0), None);
    }
}
