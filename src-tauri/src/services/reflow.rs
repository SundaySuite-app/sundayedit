//! Caption quality + re-flow — readability operations (Phase 7 depth).
//!
//! Professional captioning has hard readability limits that ASR output
//! routinely violates: too many characters on screen per second (CPS),
//! lines too long to read, or too many lines stacked at once. This module
//! is the pure analysis + repair logic for those constraints.
//!
//! Four pure pieces, all `Project`/`Caption` in → value or new state out:
//!
//!   1. `cps`            — characters-per-second for a caption (the single
//!      most-cited subtitle readability metric; broadcast targets ~17 CPS,
//!      Netflix caps at 17 for adults / 20 absolute).
//!   2. `wrap_lines`     — greedy line-break a caption's text into at most
//!      `max_lines` lines no wider than `max_chars_per_line`. Mirrors the
//!      box-wrapping libass does, so the editor preview and burn-in agree.
//!   3. `analyze`        — flag every caption that breaks a `ReflowConfig`
//!      limit (CPS / line length / line count / min-duration), so the UI can
//!      surface them the same way confidence highlighting surfaces words.
//!   4. `snap_caption_to_words` / `retime_caption_even` — pure timing repair:
//!      tighten a caption's box to its words, or redistribute word timings
//!      evenly across the caption when ASR word-timing is missing/garbage.
//!
//! Everything here is deliberately Project-free where it can be (string and
//! number math) so it's trivially unit-tested without building a fixture.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, AppResult};
use crate::model::{Caption, Project, Word};

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/ReflowConfig.ts")]
pub struct ReflowConfig {
    /// Max readable characters-per-second. Broadcast/Netflix adult default 17.
    pub max_cps: f32,
    /// Max characters on a single line before it must wrap. BBC uses 37–40.
    pub max_chars_per_line: usize,
    /// Max lines visible at once (2 is the broadcast standard).
    pub max_lines: usize,
    /// Minimum on-screen time in ms. Below this a caption flashes (Netflix 833ms ≈ 5/6 s).
    #[ts(type = "number")]
    pub min_duration_ms: i64,
}

impl Default for ReflowConfig {
    fn default() -> Self {
        // Conservative broadcast defaults — see module docs for sources.
        Self {
            max_cps: 17.0,
            max_chars_per_line: 37,
            max_lines: 2,
            min_duration_ms: 833,
        }
    }
}

// ── CPS ─────────────────────────────────────────────────────────────────────

/// Characters-per-second for a caption: visible characters (the rendered
/// text length, counting spaces, by Unicode scalar) over its on-screen
/// duration in seconds. A zero/negative-duration caption reports `f32::INFINITY`
/// so it always trips the limit rather than dividing by zero.
pub fn cps(caption: &Caption) -> f32 {
    let chars = caption.text().chars().count() as f32;
    let dur_s = (caption.end_ms - caption.start_ms) as f32 / 1000.0;
    if dur_s <= 0.0 {
        return f32::INFINITY;
    }
    chars / dur_s
}

// ── Line wrapping ─────────────────────────────────────────────────────────────

/// Greedy word-wrap `text` into lines no wider than `max_chars` (by char
/// count, including the joining spaces). A single word longer than `max_chars`
/// gets its own line rather than being split mid-word — splitting words is
/// never acceptable in subtitles.
///
/// Returns the lines in order. Whitespace is collapsed to single spaces.
pub fn wrap_lines(text: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
            continue;
        }
        // +1 for the space we'd insert.
        if current.chars().count() + 1 + word.chars().count() <= max_chars {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Balance a two-line wrap so the lines are closer in length ("top-heavy"
/// reads better than a long line over a stub). Only rebalances when the wrap
/// produced exactly two lines and rebalancing keeps both within `max_chars`.
/// Returns the (possibly rebalanced) lines.
pub fn wrap_balanced(text: &str, max_chars: usize) -> Vec<String> {
    let greedy = wrap_lines(text, max_chars);
    if greedy.len() != 2 {
        return greedy;
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 2 {
        return greedy;
    }
    // Try every split point; pick the one that minimises the line-length
    // difference while keeping both lines within max_chars. Prefer a
    // top-heavy break (first line >= second) on ties — the subtitle norm.
    let mut best: Option<(usize, Vec<String>)> = None;
    for split in 1..words.len() {
        let top = words[..split].join(" ");
        let bottom = words[split..].join(" ");
        let tlen = top.chars().count();
        let blen = bottom.chars().count();
        if tlen > max_chars || blen > max_chars {
            continue;
        }
        // Cost: length difference; tie-break toward top-heavy (smaller when top >= bottom).
        let diff = tlen.abs_diff(blen);
        let cost = diff * 2 + usize::from(tlen < blen);
        if best.as_ref().is_none_or(|(c, _)| cost < *c) {
            best = Some((cost, vec![top, bottom]));
        }
    }
    best.map(|(_, l)| l).unwrap_or(greedy)
}

// ── Analysis ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/ReflowIssue.ts")]
pub struct ReflowIssue {
    pub caption_id: String,
    /// "cps" | "line_length" | "line_count" | "min_duration"
    pub kind: String,
    /// The offending measured value (CPS, longest line length, line count, or duration ms).
    pub value: f32,
    /// The limit it broke.
    pub limit: f32,
    /// Human-readable one-liner for the UI.
    pub message: String,
}

/// Flag every caption that breaks a readability limit. One caption can yield
/// several issues (e.g. both too-fast and too-long). Empty result = clean.
pub fn analyze(project: &Project, cfg: &ReflowConfig) -> Vec<ReflowIssue> {
    let mut issues = Vec::new();
    for c in &project.captions {
        if c.words.is_empty() {
            continue;
        }
        let id = c.id.clone();

        let c_cps = cps(c);
        if c_cps > cfg.max_cps {
            issues.push(ReflowIssue {
                caption_id: id.clone(),
                kind: "cps".into(),
                value: c_cps,
                limit: cfg.max_cps,
                message: format!("{c_cps:.1} CPS exceeds {:.0} — reads too fast", cfg.max_cps),
            });
        }

        let lines = wrap_lines(&c.text(), cfg.max_chars_per_line);
        let longest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        if longest > cfg.max_chars_per_line {
            issues.push(ReflowIssue {
                caption_id: id.clone(),
                kind: "line_length".into(),
                value: longest as f32,
                limit: cfg.max_chars_per_line as f32,
                message: format!(
                    "line of {longest} chars exceeds {} — a single word is too long to wrap",
                    cfg.max_chars_per_line
                ),
            });
        }
        if lines.len() > cfg.max_lines {
            issues.push(ReflowIssue {
                caption_id: id.clone(),
                kind: "line_count".into(),
                value: lines.len() as f32,
                limit: cfg.max_lines as f32,
                message: format!(
                    "{} lines exceeds {} — split this caption",
                    lines.len(),
                    cfg.max_lines
                ),
            });
        }

        let dur = c.end_ms - c.start_ms;
        if dur < cfg.min_duration_ms {
            issues.push(ReflowIssue {
                caption_id: id,
                kind: "min_duration".into(),
                value: dur as f32,
                limit: cfg.min_duration_ms as f32,
                message: format!(
                    "{dur}ms on screen is below {}ms — flashes",
                    cfg.min_duration_ms
                ),
            });
        }
    }
    issues
}

// ── Repair (auto-split) ─────────────────────────────────────────────────────

/// Upper-bound readability of a contiguous word slice, snapped to its words
/// (start = first word start, end = last word end). Returns `true` when the
/// slice satisfies every *cap* in `cfg` that splitting can fix:
/// CPS, max line length, max line count. (Min-duration is a *lower* bound that
/// a split can only make worse, so it is handled separately by the splitter.)
fn slice_within_caps(words: &[Word], cfg: &ReflowConfig) -> bool {
    if words.is_empty() {
        return true;
    }
    let text = words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let chars = text.chars().count() as f32;
    let dur_s = (words.last().unwrap().end_ms - words.first().unwrap().start_ms) as f32 / 1000.0;
    if dur_s <= 0.0 {
        // Zero/negative span over real text is infinite CPS — never compliant.
        return text.is_empty();
    }
    if chars / dur_s > cfg.max_cps {
        return false;
    }
    let lines = wrap_lines(&text, cfg.max_chars_per_line);
    if lines.len() > cfg.max_lines {
        return false;
    }
    lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) <= cfg.max_chars_per_line
}

/// Greedily partition `words` into the fewest contiguous runs that each stay
/// within the `cfg` caps (`slice_within_caps`). A single word that cannot fit
/// any cap on its own (e.g. an unbreakably long token) still gets its own run —
/// the splitter never drops or reorders words, so such a run is emitted and
/// `analyze` will still flag it (we cannot fix it by splitting).
fn partition_words(words: &[Word], cfg: &ReflowConfig) -> Vec<Vec<Word>> {
    let mut runs: Vec<Vec<Word>> = Vec::new();
    let mut current: Vec<Word> = Vec::new();
    for w in words {
        current.push(w.clone());
        if !slice_within_caps(&current, cfg) && current.len() > 1 {
            // Adding this word broke a cap — flush everything before it and
            // start a fresh run with this word.
            let last = current.pop().unwrap();
            runs.push(std::mem::take(&mut current));
            current.push(last);
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}

/// Merge any run whose snapped duration is below `min_duration_ms` into a
/// neighbour so the result never flashes. We prefer merging a too-short tail
/// *backward* into the previous run (extending its end), falling back to
/// forward when it is the very first run. Merging only ever *widens* a run's
/// time span, so it can re-introduce a CPS/length cap violation — that is the
/// inherent tension between "long enough to read" and "few enough characters",
/// and `analyze` will surface the residual issue honestly rather than us
/// silently dropping words.
fn merge_short_runs(mut runs: Vec<Vec<Word>>, min_duration_ms: i64) -> Vec<Vec<Word>> {
    if runs.len() < 2 {
        return runs;
    }
    let dur = |run: &[Word]| run.last().unwrap().end_ms - run.first().unwrap().start_ms;
    let mut i = 0;
    while i < runs.len() {
        if runs.len() == 1 {
            break;
        }
        if dur(&runs[i]) < min_duration_ms {
            if i > 0 {
                // Merge backward into the previous run.
                let mut tail = runs.remove(i);
                runs[i - 1].append(&mut tail);
                // Re-check the merged previous run from its position.
                i -= 1;
            } else {
                // First run is short — merge the next run forward into it.
                let mut next = runs.remove(i + 1);
                runs[i].append(&mut next);
                // Stay on i to re-check the now-larger first run.
            }
        } else {
            i += 1;
        }
    }
    runs
}

/// Auto-repair every caption that `analyze` would flag by splitting it at word
/// boundaries into the fewest contiguous sub-captions that each satisfy `cfg`.
/// Captions that are already clean (or empty) pass through untouched.
///
/// Each produced sub-caption is snapped to its words (start = first word start,
/// end = last word end) and gets a fresh id from `id_for` (the caller passes a
/// UUID generator; tests pass a counter — same contract as `captionize`). Word
/// text, order, and confidence are conserved exactly; only the partitioning and
/// the per-caption box change. The result is re-validated so the project's
/// monotonic / non-overlapping invariants always hold.
///
/// Note: splitting cannot fix an unbreakably-long single word or a caption that
/// is *both* too fast and too short to stretch — those residual issues remain
/// visible to `analyze`. Repair never makes a clean project worse.
pub fn repair(
    project: &Project,
    cfg: &ReflowConfig,
    now_ms: i64,
    mut id_for: impl FnMut(usize) -> String,
) -> AppResult<Project> {
    let issues = analyze(project, cfg);
    if issues.is_empty() {
        return Ok(project.clone());
    }
    let flagged: std::collections::HashSet<&str> =
        issues.iter().map(|i| i.caption_id.as_str()).collect();

    let mut next = project.clone();
    let mut new_captions: Vec<Caption> = Vec::with_capacity(next.captions.len());
    let mut index = 0usize;

    for cap in std::mem::take(&mut next.captions) {
        if cap.words.is_empty() || !flagged.contains(cap.id.as_str()) {
            new_captions.push(cap);
            continue;
        }
        let runs = merge_short_runs(partition_words(&cap.words, cfg), cfg.min_duration_ms);
        if runs.len() <= 1 {
            // Nothing splitting can do (one unbreakable run) — keep the caption
            // intact, including its original id, rather than churn it.
            new_captions.push(cap);
            continue;
        }
        for run in runs {
            let start_ms = run.first().unwrap().start_ms;
            let end_ms = run.last().unwrap().end_ms;
            new_captions.push(Caption {
                id: id_for(index),
                start_ms,
                end_ms,
                words: run,
                speaker_id: cap.speaker_id.clone(),
                style_id: cap.style_id.clone(),
                notes: cap.notes.clone(),
                ai_generated: cap.ai_generated,
                last_edited_at: now_ms,
                track_id: None,
            });
            index += 1;
        }
    }

    next.captions = new_captions;
    next.updated_at = now_ms;
    next.validate().map_err(AppError::Invariant)?;
    Ok(next)
}

// ── Timing repair ───────────────────────────────────────────────────────────

/// Tighten (snap) a caption's box to its words: `start_ms` = first word start,
/// `end_ms` = last word end. No-op if the caption has no words. Keeps the
/// project valid because snapping inward never creates an overlap.
pub fn snap_caption_to_words(
    project: &Project,
    caption_id: &str,
    now_ms: i64,
) -> AppResult<Project> {
    let mut next = project.clone();
    let cap = next
        .captions
        .iter_mut()
        .find(|c| c.id == caption_id)
        .ok_or_else(|| AppError::NotFound {
            entity: "caption",
            id: caption_id.to_string(),
        })?;
    if cap.words.is_empty() {
        return Ok(project.clone());
    }
    cap.start_ms = cap.words.first().unwrap().start_ms;
    cap.end_ms = cap.words.last().unwrap().end_ms;
    cap.last_edited_at = now_ms;
    next.updated_at = now_ms;
    next.validate().map_err(AppError::Invariant)?;
    Ok(next)
}

/// Redistribute a caption's word timings evenly across its `[start_ms, end_ms]`
/// box. Used when ASR returned a caption-level time range but garbage (or no)
/// per-word timing — even spacing is a defensible, monotonic fallback that the
/// confidence-highlighting word stepping needs in order to work at all.
///
/// Word order and text are preserved; only `start_ms`/`end_ms` per word change.
pub fn retime_caption_even(project: &Project, caption_id: &str, now_ms: i64) -> AppResult<Project> {
    let mut next = project.clone();
    let cap = next
        .captions
        .iter_mut()
        .find(|c| c.id == caption_id)
        .ok_or_else(|| AppError::NotFound {
            entity: "caption",
            id: caption_id.to_string(),
        })?;
    let n = cap.words.len();
    if n == 0 {
        return Ok(project.clone());
    }
    let span = cap.end_ms - cap.start_ms;
    if span < n as i64 {
        return Err(AppError::Validation(format!(
            "caption span {span}ms too short to give {n} words ≥1ms each"
        )));
    }
    // Integer-fair partition: distribute the remainder across the first words
    // so the boundaries stay monotonic and the last word ends exactly at end_ms.
    let base = span / n as i64;
    let rem = span % n as i64;
    let mut t = cap.start_ms;
    for (i, w) in cap.words.iter_mut().enumerate() {
        let slot = base + i64::from((i as i64) < rem);
        w.start_ms = t;
        w.end_ms = t + slot;
        t += slot;
    }
    cap.last_edited_at = now_ms;
    next.updated_at = now_ms;
    next.validate().map_err(AppError::Invariant)?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Caption, Project, Style, Word};

    fn caption(id: &str, start: i64, end: i64, words: Vec<Word>) -> Caption {
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

    fn proj(captions: Vec<Caption>) -> Project {
        Project {
            id: "p".into(),
            name: "t".into(),
            video_path: "/x".into(),
            video_content_hash: "h".into(),
            video_duration_ms: 600_000,
            video_width: 1920,
            video_height: 1080,
            video_fps: 30.0,
            audio_wav_path: None,
            language: "en".into(),
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

    fn w(text: &str, s: i64, e: i64) -> Word {
        Word::new(text, s, e, 90.0)
    }

    // ── cps ──────────────────────────────────────────────────────────────────
    #[test]
    fn cps_counts_chars_over_seconds() {
        // "Hello world" = 11 chars, over 1s = 11 CPS.
        let c = caption(
            "c",
            0,
            1000,
            vec![w("Hello", 0, 500), w("world", 500, 1000)],
        );
        assert!((cps(&c) - 11.0).abs() < 0.001);
    }

    #[test]
    fn cps_infinite_on_zero_duration() {
        let c = caption("c", 1000, 1000, vec![w("x", 1000, 1000)]);
        assert!(cps(&c).is_infinite());
    }

    // ── wrap_lines ─────────────────────────────────────────────────────────────
    #[test]
    fn wrap_greedy_breaks_at_limit() {
        // "the cat sat" with max 7 → "the cat" (7), "sat".
        let lines = wrap_lines("the cat sat", 7);
        assert_eq!(lines, vec!["the cat", "sat"]);
    }

    #[test]
    fn wrap_keeps_overlong_word_whole() {
        let lines = wrap_lines("a supercalifragilistic b", 5);
        // The long word gets its own line rather than being split.
        assert_eq!(lines, vec!["a", "supercalifragilistic", "b"]);
    }

    #[test]
    fn wrap_collapses_whitespace() {
        let lines = wrap_lines("  one   two  ", 100);
        assert_eq!(lines, vec!["one two"]);
    }

    #[test]
    fn wrap_unicode_counts_scalars_not_bytes() {
        // "café" is 4 chars but 5 bytes; with max 4 it must fit on one line.
        let lines = wrap_lines("café té", 4);
        assert_eq!(lines, vec!["café", "té"]);
    }

    // ── wrap_balanced ──────────────────────────────────────────────────────────
    #[test]
    fn balanced_evens_out_a_lopsided_break() {
        // Greedy would pack the first line full and strand a stub; balanced
        // makes the two lines closer in length.
        let text = "one two three four five";
        let greedy = wrap_lines(text, 14);
        let balanced = wrap_balanced(text, 14);
        assert_eq!(greedy.len(), 2);
        assert_eq!(balanced.len(), 2);
        let g_diff = greedy[0].len().abs_diff(greedy[1].len());
        let b_diff = balanced[0].len().abs_diff(balanced[1].len());
        assert!(b_diff <= g_diff);
    }

    #[test]
    fn balanced_prefers_top_heavy_on_ties() {
        // "ab cd ef" with width 8 → balanced two-line, top line not shorter.
        let lines = wrap_balanced("aaa bbb ccc", 7);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].chars().count() >= lines[1].chars().count());
    }

    #[test]
    fn balanced_passthrough_when_one_line() {
        assert_eq!(wrap_balanced("short", 80), vec!["short"]);
    }

    // ── analyze ────────────────────────────────────────────────────────────────
    #[test]
    fn analyze_flags_fast_caption() {
        // 30 chars in 1s = 30 CPS, way over 17.
        let c = caption(
            "fast",
            0,
            1000,
            vec![w("abcdefghij", 0, 500), w("klmnopqrst", 500, 1000)],
        );
        let issues = analyze(&proj(vec![c]), &ReflowConfig::default());
        assert!(issues.iter().any(|i| i.kind == "cps"));
    }

    #[test]
    fn analyze_flags_too_long_unbreakable_line() {
        let long = "x".repeat(50);
        let c = caption("long", 0, 10_000, vec![w(&long, 0, 5000)]);
        let issues = analyze(&proj(vec![c]), &ReflowConfig::default());
        let line = issues.iter().find(|i| i.kind == "line_length").unwrap();
        assert_eq!(line.value, 50.0);
    }

    #[test]
    fn analyze_flags_too_many_lines() {
        // Five short words, max 3 chars/line → 5 lines, over max_lines 2.
        let c = caption(
            "many",
            0,
            10_000,
            vec![
                w("aaa", 0, 2000),
                w("bbb", 2000, 4000),
                w("ccc", 4000, 6000),
                w("ddd", 6000, 8000),
                w("eee", 8000, 10_000),
            ],
        );
        let cfg = ReflowConfig {
            max_chars_per_line: 3,
            ..ReflowConfig::default()
        };
        let issues = analyze(&proj(vec![c]), &cfg);
        assert!(
            issues
                .iter()
                .any(|i| i.kind == "line_count" && i.value == 5.0)
        );
    }

    #[test]
    fn analyze_flags_flash_caption() {
        let c = caption("flash", 0, 300, vec![w("hi", 0, 300)]);
        let issues = analyze(&proj(vec![c]), &ReflowConfig::default());
        let d = issues.iter().find(|i| i.kind == "min_duration").unwrap();
        assert_eq!(d.value, 300.0);
    }

    #[test]
    fn analyze_clean_caption_has_no_issues() {
        // "Hello there" 11 chars over 2s = 5.5 CPS, one short line, 2s on screen.
        let c = caption(
            "ok",
            0,
            2000,
            vec![w("Hello", 0, 1000), w("there", 1000, 2000)],
        );
        assert!(analyze(&proj(vec![c]), &ReflowConfig::default()).is_empty());
    }

    #[test]
    fn analyze_skips_empty_captions() {
        let c = caption("empty", 0, 100, vec![]);
        assert!(analyze(&proj(vec![c]), &ReflowConfig::default()).is_empty());
    }

    // ── snap_caption_to_words ───────────────────────────────────────────────────
    #[test]
    fn snap_tightens_box_to_words() {
        // Caption box 0..5000 but words only span 1000..3000.
        let c = caption("c", 0, 5000, vec![w("a", 1000, 2000), w("b", 2000, 3000)]);
        let out = snap_caption_to_words(&proj(vec![c]), "c", 9).unwrap();
        assert_eq!(out.captions[0].start_ms, 1000);
        assert_eq!(out.captions[0].end_ms, 3000);
        out.validate().unwrap();
    }

    #[test]
    fn snap_missing_caption_errors() {
        let c = caption("c", 0, 1000, vec![w("a", 0, 1000)]);
        let err = snap_caption_to_words(&proj(vec![c]), "nope", 9).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    // ── retime_caption_even ─────────────────────────────────────────────────────
    #[test]
    fn retime_even_distributes_words() {
        // 3 words across 0..900 → 300ms each.
        let c = caption(
            "c",
            0,
            900,
            vec![w("a", 0, 10), w("b", 10, 20), w("c", 20, 30)],
        );
        let out = retime_caption_even(&proj(vec![c]), "c", 9).unwrap();
        let ws = &out.captions[0].words;
        assert_eq!((ws[0].start_ms, ws[0].end_ms), (0, 300));
        assert_eq!((ws[1].start_ms, ws[1].end_ms), (300, 600));
        assert_eq!((ws[2].start_ms, ws[2].end_ms), (600, 900));
        out.validate().unwrap();
    }

    #[test]
    fn retime_even_remainder_goes_to_first_words_and_ends_exactly() {
        // 4 words across 0..1000 → 1000/4 = 250 base, 0 remainder; clean.
        // 3 words across 0..1000 → base 333, rem 1 → first word gets +1.
        let c = caption("c", 0, 1000, vec![w("a", 0, 1), w("b", 1, 2), w("c", 2, 3)]);
        let out = retime_caption_even(&proj(vec![c]), "c", 9).unwrap();
        let ws = &out.captions[0].words;
        assert_eq!(ws[0].start_ms, 0);
        assert_eq!(ws[0].end_ms, 334); // 333 + 1 remainder
        assert_eq!(ws[1].start_ms, 334);
        assert_eq!(ws.last().unwrap().end_ms, 1000); // exact landing
        out.validate().unwrap();
    }

    #[test]
    fn retime_even_too_short_span_errors() {
        // 3 words but only 2ms of span — can't give each ≥1ms.
        let c = caption("c", 0, 2, vec![w("a", 0, 1), w("b", 1, 2), w("c", 1, 2)]);
        let err = retime_caption_even(&proj(vec![c]), "c", 9).unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    // ── repair ──────────────────────────────────────────────────────────────────
    fn counter_ids() -> impl FnMut(usize) -> String {
        move |i| format!("r{i}")
    }

    #[test]
    fn repair_clean_project_is_unchanged() {
        // "Hello there" — 5.5 CPS, short line, 2s on screen: already clean.
        let c = caption(
            "ok",
            0,
            2000,
            vec![w("Hello", 0, 1000), w("there", 1000, 2000)],
        );
        let p = proj(vec![c]);
        let out = repair(&p, &ReflowConfig::default(), 9, counter_ids()).unwrap();
        assert_eq!(out.captions, p.captions);
    }

    #[test]
    fn repair_splits_an_over_cps_caption_into_compliant_ones() {
        // Ten 5-char words back-to-back at 350ms each. Whole caption: 59 chars
        // (10×5 + 9 joining spaces) over 3.5s = 16.9 CPS — just over... actually
        // the spaces tip it over because they count in the box text but vanish
        // at each split boundary. A 1–2-word sub-caption snapped to its words
        // reads at ≤15.7 CPS, so splitting genuinely makes the project clean.
        let words: Vec<Word> = (0..10)
            .map(|i| w("abcde", i * 350, i * 350 + 350))
            .collect();
        // Box snapped to the words: 0..3500. Text 59 chars / 3.5s ≈ 16.86 CPS.
        let c = caption("fast", 0, 3500, words);
        let p = proj(vec![c]);
        let cfg = ReflowConfig {
            max_cps: 16.0,      // tighten just below the box CPS so it must split
            min_duration_ms: 0, // isolate the CPS axis for this fixture
            ..ReflowConfig::default()
        };
        assert!(
            analyze(&p, &cfg).iter().any(|i| i.kind == "cps"),
            "fixture must trip the CPS limit"
        );

        let out = repair(&p, &cfg, 9, counter_ids()).unwrap();
        assert!(out.captions.len() > 1, "fast caption must be split");
        // The literal core promise: re-analysis is now clean.
        assert!(
            analyze(&out, &cfg).iter().all(|i| i.kind != "cps"),
            "repaired project still has CPS issues"
        );
        out.validate().unwrap();
    }

    #[test]
    fn repair_splits_a_too_long_unbreakable_count_into_two_lines() {
        // Eight words, each "wordword" (8 chars) → one caption would wrap into
        // 4 lines at 37 cpl-ish; force a tiny line width so line_count trips.
        let words: Vec<Word> = (0..8)
            .map(|i| w("alpha", i * 1000, i * 1000 + 900))
            .collect();
        let c = caption("many", 0, 8000, words);
        let cfg = ReflowConfig {
            max_chars_per_line: 5, // each "alpha" is its own line
            max_lines: 2,
            ..ReflowConfig::default()
        };
        let p = proj(vec![c]);
        assert!(analyze(&p, &cfg).iter().any(|i| i.kind == "line_count"));

        let out = repair(&p, &cfg, 9, counter_ids()).unwrap();
        assert!(analyze(&out, &cfg).is_empty());
        out.validate().unwrap();
    }

    #[test]
    fn repair_conserves_word_text_order_and_confidence() {
        let words = vec![
            Word::new("one", 0, 900, 91.0),
            Word::new("two", 1000, 1900, 42.0),
            Word::new("three", 2000, 2900, 88.0),
            Word::new("four", 3000, 3900, 13.0),
            Word::new("five", 4000, 4900, 77.0),
            Word::new("six", 5000, 5900, 55.0),
        ];
        let c = caption("fast", 0, 5900, words.clone());
        let p = proj(vec![c]);
        // Force splitting via a tight CPS budget.
        let cfg = ReflowConfig {
            max_cps: 4.0,
            ..ReflowConfig::default()
        };
        let out = repair(&p, &cfg, 9, counter_ids()).unwrap();

        let flat: Vec<(&str, f32)> = out
            .captions
            .iter()
            .flat_map(|c| c.words.iter())
            .map(|w| (w.text.as_str(), w.confidence))
            .collect();
        let expected: Vec<(&str, f32)> = words
            .iter()
            .map(|w| (w.text.as_str(), w.confidence))
            .collect();
        assert_eq!(flat, expected, "words must be conserved exactly, in order");
    }

    #[test]
    fn repair_keeps_metadata_on_split_captions() {
        // Force a split via too-many-lines (tight chars-per-line).
        let mut c = caption(
            "many",
            0,
            8000,
            (0..8)
                .map(|i| w("alpha", i * 1000, i * 1000 + 900))
                .collect(),
        );
        c.speaker_id = Some("spk".into());
        c.style_id = Some("sty".into());
        c.notes = Some("n".into());
        let cfg = ReflowConfig {
            max_chars_per_line: 5,
            max_lines: 2,
            ..ReflowConfig::default()
        };
        let out = repair(&proj(vec![c]), &cfg, 9, counter_ids()).unwrap();
        assert!(out.captions.len() > 1);
        for cap in &out.captions {
            assert_eq!(cap.speaker_id.as_deref(), Some("spk"));
            assert_eq!(cap.style_id.as_deref(), Some("sty"));
            assert_eq!(cap.notes.as_deref(), Some("n"));
            assert_eq!(cap.last_edited_at, 9);
        }
    }

    #[test]
    fn repair_unbreakable_single_word_keeps_caption_intact() {
        // One 50-char word can't be split or wrapped; repair must not churn it.
        let long = "x".repeat(50);
        let c = caption("long", 0, 10_000, vec![w(&long, 0, 5000)]);
        let p = proj(vec![c]);
        let out = repair(&p, &ReflowConfig::default(), 9, counter_ids()).unwrap();
        assert_eq!(out.captions.len(), 1);
        assert_eq!(out.captions[0].id, "long"); // original id preserved
        // The issue is still honestly reported — splitting can't fix it.
        assert!(
            analyze(&out, &ReflowConfig::default())
                .iter()
                .any(|i| i.kind == "line_length")
        );
    }

    #[test]
    fn repair_result_is_monotonic_and_non_overlapping() {
        // Tight chars-per-line forces the two long captions to split; the short
        // clean one between them must survive untouched.
        let cfg = ReflowConfig {
            max_chars_per_line: 5,
            max_lines: 2,
            ..ReflowConfig::default()
        };
        let many1 = caption(
            "a",
            0,
            6000,
            (0..6)
                .map(|i| w("alpha", i * 1000, i * 1000 + 900))
                .collect(),
        );
        let clean = caption(
            "b",
            7000,
            9000,
            vec![w("Hi", 7000, 8000), w("there", 8000, 9000)],
        );
        let many2 = caption(
            "c",
            10_000,
            16_000,
            (0..6)
                .map(|i| w("alpha", 10_000 + i * 1000, 10_000 + i * 1000 + 900))
                .collect(),
        );
        let p = proj(vec![many1, clean, many2]);
        let out = repair(&p, &cfg, 9, counter_ids()).unwrap();
        out.validate().unwrap();
        // The clean caption is untouched (same id).
        assert!(out.captions.iter().any(|c| c.id == "b"));
        assert!(analyze(&out, &cfg).is_empty());
    }

    #[test]
    fn repair_never_drops_or_reorders_words_property() {
        // Deterministic "property" sweep (no rand dep in this crate): vary word
        // count, per-word duration, and CPS budget; repair must always conserve
        // the exact word sequence and produce a valid project.
        let cfg_budgets = [4.0_f32, 8.0, 12.0, 17.0, 25.0];
        for n in 2..=12usize {
            for step in [400_i64, 700, 1000, 1500] {
                let words: Vec<Word> = (0..n)
                    .map(|i| {
                        let s = i as i64 * step;
                        Word::new(format!("w{i}"), s, s + step - 50, 50.0 + (i % 40) as f32)
                    })
                    .collect();
                let last_end = words.last().unwrap().end_ms;
                let c = caption("c", 0, last_end, words.clone());
                let p = proj(vec![c]);
                for &max_cps in &cfg_budgets {
                    let cfg = ReflowConfig {
                        max_cps,
                        ..ReflowConfig::default()
                    };
                    let out = repair(&p, &cfg, 1, counter_ids()).unwrap();
                    out.validate().expect("repaired project must validate");
                    let flat: Vec<&str> = out
                        .captions
                        .iter()
                        .flat_map(|c| c.words.iter())
                        .map(|w| w.text.as_str())
                        .collect();
                    let expected: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
                    assert_eq!(flat, expected, "n={n} step={step} cps={max_cps}");
                }
            }
        }
    }

    // ── Property: wrap_lines never splits a word and never exceeds max_chars ─────
    //
    // The two load-bearing promises of `wrap_lines` (per its doc): (1) a word is
    // never split mid-token — every output token is one of the input whitespace
    // tokens; and (2) no line is wider than `max_chars` UNLESS it is a single
    // word that alone exceeds `max_chars` (an unbreakable token). We also assert
    // the word multiset/order is conserved (concatenating output tokens == input
    // tokens), which together pins "no words dropped/reordered/duplicated".
    //
    // Fixed-seed PRNG, capped at 500 iterations — cheap, finds edge cases.
    struct PRng(u64);
    impl PRng {
        fn new(seed: u64) -> Self {
            PRng(seed | 1)
        }
        fn next_u64(&mut self) -> u64 {
            // xorshift64*
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next_u64() % n as u64) as usize
        }
    }

    #[test]
    fn wrap_lines_property_no_split_no_overflow_conserves_words() {
        let mut rng = PRng::new(0xA55E_1B1E_C0FF_EE01);
        // A small alphabet incl. multibyte chars so char-count != byte-count.
        let alphabet = ['a', 'b', 'z', 'é', 'ø', '中', '🙂'];
        for iter in 0..500 {
            let max_chars = 1 + rng.below(20); // 1..=20
            let word_count = rng.below(10); // 0..=9 words
            let mut words: Vec<String> = Vec::with_capacity(word_count);
            for _ in 0..word_count {
                let len = 1 + rng.below(12); // 1..=12 chars per word (can exceed max)
                let s: String = (0..len)
                    .map(|_| alphabet[rng.below(alphabet.len())])
                    .collect();
                words.push(s);
            }
            // Build the input text by joining with random whitespace runs (the
            // function must collapse these). Empty whitespace-only inputs too.
            let mut text = String::new();
            for (wi, word) in words.iter().enumerate() {
                if wi > 0 {
                    // 1..=3 whitespace chars, mix of space/tab/newline.
                    for _ in 0..(1 + rng.below(3)) {
                        text.push([' ', '\t', '\n'][rng.below(3)]);
                    }
                }
                text.push_str(word);
            }

            let lines = wrap_lines(&text, max_chars);

            // (1) No line overflows unless it is a single unbreakable word.
            for line in &lines {
                let llen = line.chars().count();
                if llen > max_chars {
                    let tokens: Vec<&str> = line.split(' ').collect();
                    assert_eq!(
                        tokens.len(),
                        1,
                        "iter={iter} max={max_chars}: overflowing line {line:?} is not a single word"
                    );
                    assert!(
                        tokens[0].chars().count() > max_chars,
                        "iter={iter} max={max_chars}: line {line:?} overflows but its only word fits"
                    );
                }
            }

            // (2) Word conservation: flatten output tokens, compare to input
            // whitespace tokens exactly (order + multiplicity). This proves no
            // word was split (a split would produce a token not in the input),
            // dropped, duplicated, or reordered.
            let out_tokens: Vec<&str> = lines
                .iter()
                .flat_map(|l| l.split(' '))
                .filter(|t| !t.is_empty())
                .collect();
            let in_tokens: Vec<&str> = text.split_whitespace().collect();
            assert_eq!(
                out_tokens, in_tokens,
                "iter={iter} max={max_chars}: words not conserved in order for {text:?}"
            );
        }
    }

    // ── Property: repair is idempotent ──────────────────────────────────────────
    //
    // `repair` is the headline auto-fix. Its doc promises it "never makes a clean
    // project worse" and re-validates the result. A subtler, highly valuable
    // property the existing tests do NOT pin: running repair a SECOND time must be
    // a no-op (repair(repair(p)) == repair(p)). If it weren't, the UI's "fix all"
    // button would churn caption ids/boxes on repeated presses, and any residual
    // (unbreakable) issue could ping-pong. Idempotence is the contract that makes
    // repair safe to call freely.
    //
    // We feed adversarial captions across a grid of configs, fixed seed, ≤500
    // total iterations.
    #[test]
    fn repair_is_idempotent_property() {
        let mut rng = PRng::new(0xBADD_CAFE_1234_5678);
        let cfg_grid = [
            ReflowConfig::default(),
            ReflowConfig {
                max_cps: 4.0,
                ..Default::default()
            },
            ReflowConfig {
                max_chars_per_line: 5,
                max_lines: 2,
                ..Default::default()
            },
            ReflowConfig {
                max_cps: 8.0,
                max_chars_per_line: 10,
                min_duration_ms: 1500,
                ..Default::default()
            },
        ];
        let mut iters = 0;
        'outer: for trial in 0..200 {
            // Build one caption with a random number of words, random per-word
            // span and text length (so some words are unbreakably long).
            let n = 1 + rng.below(10); // 1..=10 words
            let step = 200 + (rng.below(8) as i64) * 200; // 200..=1600 ms
            let mut words = Vec::with_capacity(n);
            let mut t = 0i64;
            for i in 0..n {
                let dur = 30 + (rng.below(step as usize) as i64).max(1);
                let len = 2 + rng.below(14); // 2..=15 chars
                let txt: String = "x".repeat(len);
                words.push(Word::new(format!("{txt}{i}"), t, t + dur, 60.0));
                t += dur + (rng.below(50) as i64);
            }
            let end = words.last().unwrap().end_ms;
            let c = caption("seed", 0, end, words);
            let p = proj(vec![c]);

            for cfg in &cfg_grid {
                let once = repair(&p, cfg, 9, counter_ids());
                let once = match once {
                    Ok(o) => o,
                    Err(_) => continue, // some adversarial inputs validly error; skip
                };
                let twice =
                    repair(&once, cfg, 9, counter_ids()).expect("second repair must succeed");
                assert_eq!(
                    once.captions, twice.captions,
                    "repair not idempotent (trial={trial}, cfg={cfg:?})"
                );
                // And the result is always valid.
                once.validate().expect("repaired project must validate");

                iters += 1;
                if iters >= 500 {
                    break 'outer;
                }
            }
        }
        assert!(iters > 0, "property exercised no cases");
    }

    // ── Property: fmt round-trip for an even word retiming ───────────────────────
    //
    // `retime_caption_even` must (a) preserve word count/text/order, (b) leave
    // boundaries strictly monotonic so the project validates, and (c) land the
    // last word's end exactly on the caption's end_ms (no drift from integer
    // division). The existing tests check a couple of hand cases; this sweeps a
    // grid to pin the "exact landing + monotonic" invariant generally.
    #[test]
    fn retime_even_lands_exactly_and_is_monotonic_property() {
        for n in 1..=12i64 {
            for span in [n, n + 1, 1000, 1001, 99_999] {
                if span < n {
                    continue;
                }
                let words: Vec<Word> = (0..n)
                    .map(|i| Word::new(format!("w{i}"), i, i + 1, 50.0))
                    .collect();
                let c = caption("c", 0, span, words);
                let out = retime_caption_even(&proj(vec![c]), "c", 9).unwrap();
                let ws = &out.captions[0].words;
                assert_eq!(ws.len() as i64, n);
                assert_eq!(ws[0].start_ms, 0, "first word starts at caption start");
                assert_eq!(
                    ws.last().unwrap().end_ms,
                    span,
                    "last word must end exactly at caption end (n={n} span={span})"
                );
                // Strictly monotonic, each word ≥1ms, contiguous.
                for win in ws.windows(2) {
                    assert_eq!(win[0].end_ms, win[1].start_ms, "contiguous");
                }
                for word in ws {
                    assert!(word.end_ms > word.start_ms, "each word ≥1ms");
                }
                out.validate().unwrap();
            }
        }
    }

    // ── repair: empty-run safety (panic regression) ─────────────────────────────
    #[test]
    fn repair_pathological_long_words_never_panics() {
        // Every word on its own is already over the per-line cap, so
        // `partition_words` is forced to emit one run per word and
        // `merge_short_runs` then merges aggressively. Exercises the run
        // helpers' `.first()/.last()` on every produced run — must not panic
        // and must conserve every word.
        let cfg = ReflowConfig {
            max_cps: 1.0,
            max_chars_per_line: 3,
            max_lines: 1,
            min_duration_ms: 5000,
        };
        let words: Vec<Word> = (0..8i64)
            .map(|i| w("supercalifragilistic", i * 10, i * 10 + 1))
            .collect();
        let c = caption("c", 0, 80, words);
        let out = repair(&proj(vec![c]), &cfg, 100, |i| format!("n{i}")).unwrap();
        let total: usize = out.captions.iter().map(|c| c.words.len()).sum();
        assert_eq!(total, 8, "every word conserved");
        out.validate().unwrap();
    }
}
