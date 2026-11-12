//! Filler-word + silence removal with ripple editing — Phase 7.2.
//!
//! Three pure pieces:
//!   1. `detect_fillers`  — find filler words ("um", "uh", "eh", "liksom"…)
//!      per language. Returns hits the user reviews.
//!   2. `detect_silences` — gaps longer than a threshold between words.
//!   3. `apply_ripple_cuts` — given a set of time ranges to cut, delete the
//!      words inside them AND shift everything after each cut earlier by the
//!      cut's duration (destructive "ripple" edit). This is the tricky
//!      timing math, and it's exhaustively tested.
//!
//! The UI flow: detect → user reviews/approves → turn approved hits into
//! cut ranges → apply_ripple_cuts.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, AppResult};
use crate::model::Project;

// ── Filler detection ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/FillerHit.ts")]
pub struct FillerHit {
    pub caption_id: String,
    pub word_index: usize,
    pub text: String,
    #[ts(type = "number")]
    pub start_ms: i64,
    #[ts(type = "number")]
    pub end_ms: i64,
}

/// Filler words per language. Lowercased, punctuation-stripped before match.
fn filler_set(language: &str) -> &'static [&'static str] {
    match language {
        "no" | "nb" | "nn" => &["eh", "øh", "hm", "altså", "liksom"],
        // default to English
        _ => &["um", "uh", "uhm", "erm", "hmm", "like", "y'know"],
    }
}

/// Words that are only fillers in certain contexts ("like", "altså", "so")
/// — we still surface them but they need human judgment, so the UI defaults
/// them to "keep". Kept conceptually; for v1 we treat the `filler_set` as
/// the candidate list and let the user approve each.
pub fn detect_fillers(project: &Project, language: &str) -> Vec<FillerHit> {
    let set = filler_set(language);
    let mut hits = Vec::new();
    for cap in &project.captions {
        for (wi, word) in cap.words.iter().enumerate() {
            let core = strip_punct(&word.text).to_lowercase();
            if !core.is_empty() && set.contains(&core.as_str()) {
                hits.push(FillerHit {
                    caption_id: cap.id.clone(),
                    word_index: wi,
                    text: word.text.clone(),
                    start_ms: word.start_ms,
                    end_ms: word.end_ms,
                });
            }
        }
    }
    hits
}

fn strip_punct(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .collect()
}

// ── Silence detection ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/SilenceGap.ts")]
pub struct SilenceGap {
    #[ts(type = "number")]
    pub start_ms: i64,
    #[ts(type = "number")]
    pub end_ms: i64,
    #[ts(type = "number")]
    pub duration_ms: i64,
}

/// Gaps longer than `min_gap_ms` between consecutive words across the whole
/// project (caption boundaries included).
pub fn detect_silences(project: &Project, min_gap_ms: i64) -> Vec<SilenceGap> {
    // Flatten word time ranges in order.
    let mut spans: Vec<(i64, i64)> = Vec::new();
    for cap in &project.captions {
        for w in &cap.words {
            spans.push((w.start_ms, w.end_ms));
        }
    }
    spans.sort_by_key(|s| s.0);

    let mut gaps = Vec::new();
    for win in spans.windows(2) {
        let prev_end = win[0].1;
        let next_start = win[1].0;
        let gap = next_start - prev_end;
        if gap >= min_gap_ms {
            gaps.push(SilenceGap {
                start_ms: prev_end,
                end_ms: next_start,
                duration_ms: gap,
            });
        }
    }
    gaps
}

// ── Ripple cuts ───────────────────────────────────────────────────────────────

/// Apply a set of time-range cuts to the project, rippling subsequent
/// content earlier. This is destructive editing.
///
/// Algorithm:
///   1. Merge overlapping/adjacent cut ranges.
///   2. For every word: if it falls entirely inside a cut → drop it. Words
///      that survive shift earlier by the total cut-duration that precedes
///      their (original) start.
///   3. Recompute caption start/end from remaining words; drop empty captions.
///
/// Words straddling a cut boundary are kept (we don't split a word's audio);
/// they shift by the cut duration that precedes their start. This keeps the
/// math simple and predictable — the user picks clean cut ranges.
pub fn apply_ripple_cuts(
    project: &Project,
    cuts: &[(i64, i64)],
    now_ms: i64,
) -> AppResult<Project> {
    let merged = merge_ranges(cuts);
    if merged.is_empty() {
        return Ok(project.clone());
    }

    let mut next = project.clone();
    for cap in next.captions.iter_mut() {
        let mut kept = Vec::with_capacity(cap.words.len());
        for w in cap.words.drain(..) {
            // Drop if entirely within any cut.
            if merged
                .iter()
                .any(|&(s, e)| w.start_ms >= s && w.end_ms <= e)
            {
                continue;
            }
            let shift = cut_duration_before(&merged, w.start_ms);
            let mut nw = w;
            nw.start_ms = (nw.start_ms - shift).max(0);
            nw.end_ms = (nw.end_ms - shift).max(nw.start_ms + 1);
            kept.push(nw);
        }
        cap.words = kept;
        if !cap.words.is_empty() {
            cap.start_ms = cap.words.first().unwrap().start_ms;
            cap.end_ms = cap.words.last().unwrap().end_ms;
            cap.last_edited_at = now_ms;
        }
    }
    next.captions.retain(|c| !c.words.is_empty());
    next.updated_at = now_ms;
    // A cut straddling a caption boundary can shift the following caption
    // earlier while the preceding one stays put, producing overlapping
    // captions. Enforce the same invariants every caption operation does so
    // corrupted timing never escapes into save/export.
    next.validate().map_err(AppError::Invariant)?;
    Ok(next)
}

/// Total duration of all cuts that end at or before `point` (i.e. precede it).
fn cut_duration_before(cuts: &[(i64, i64)], point: i64) -> i64 {
    let mut total = 0;
    for &(s, e) in cuts {
        if e <= point {
            total += e - s;
        } else if s < point && point < e {
            // point is inside this cut — count the part before it
            total += point - s;
        }
    }
    total
}

/// Merge overlapping or touching ranges, sorted by start.
fn merge_ranges(ranges: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut sorted: Vec<(i64, i64)> = ranges.iter().copied().filter(|&(s, e)| e > s).collect();
    sorted.sort_by_key(|r| r.0);
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (s, e) in sorted {
        if let Some(last) = merged.last_mut()
            && s <= last.1
        {
            last.1 = last.1.max(e);
            continue;
        }
        merged.push((s, e));
    }
    merged
}

/// Convenience: turn approved filler hits into cut ranges.
pub fn fillers_to_cuts(hits: &[FillerHit]) -> Vec<(i64, i64)> {
    hits.iter().map(|h| (h.start_ms, h.end_ms)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Caption, Project, Style, Word};

    fn from_words(words: Vec<(&str, i64, i64)>) -> Project {
        let ws: Vec<Word> = words
            .iter()
            .map(|&(t, s, e)| Word::new(t, s, e, 90.0))
            .collect();
        let start = ws.first().map(|w| w.start_ms).unwrap_or(0);
        let end = ws.last().map(|w| w.end_ms).unwrap_or(0);
        Project {
            id: "p".into(),
            name: "t".into(),
            video_path: "/x".into(),
            video_content_hash: "h".into(),
            video_duration_ms: 60000,
            video_width: 1920,
            video_height: 1080,
            video_fps: 30.0,
            audio_wav_path: None,
            language: "en".into(),
            default_style: Style::broadcast_news(),
            context_description: None,
            captions: vec![Caption {
                id: "c1".into(),
                start_ms: start,
                end_ms: end,
                words: ws,
                speaker_id: None,
                style_id: None,
                notes: None,
                ai_generated: true,
                last_edited_at: 0,
                track_id: None,
            }],
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

    // ── filler detection ────────────────────────────────────────────────────
    #[test]
    fn detects_english_fillers() {
        let p = from_words(vec![
            ("So", 0, 300),
            ("um", 300, 600),
            ("yes", 600, 900),
            ("uh", 900, 1100),
        ]);
        let hits = detect_fillers(&p, "en");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].text, "um");
        assert_eq!(hits[1].text, "uh");
    }

    #[test]
    fn detects_norwegian_fillers() {
        let p = from_words(vec![
            ("Jeg", 0, 300),
            ("altså", 300, 700),
            ("liksom", 700, 1100),
        ]);
        let hits = detect_fillers(&p, "no");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn filler_match_ignores_punctuation_and_case() {
        let p = from_words(vec![("Um,", 0, 300), ("UH.", 300, 600)]);
        let hits = detect_fillers(&p, "en");
        assert_eq!(hits.len(), 2);
    }

    // ── silence detection ─────────────────────────────────────────────────────
    #[test]
    fn detects_silence_gaps() {
        // gap of 2000ms between "a" (ends 500) and "b" (starts 2500)
        let p = from_words(vec![("a", 0, 500), ("b", 2500, 3000), ("c", 3100, 3500)]);
        let gaps = detect_silences(&p, 1000);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].start_ms, 500);
        assert_eq!(gaps[0].end_ms, 2500);
        assert_eq!(gaps[0].duration_ms, 2000);
    }

    #[test]
    fn ignores_small_gaps() {
        let p = from_words(vec![("a", 0, 500), ("b", 600, 1000)]); // 100ms gap
        assert!(detect_silences(&p, 1000).is_empty());
    }

    // ── merge ranges ──────────────────────────────────────────────────────────
    #[test]
    fn merges_overlapping_ranges() {
        let m = merge_ranges(&[(0, 100), (50, 200), (300, 400)]);
        assert_eq!(m, vec![(0, 200), (300, 400)]);
    }

    #[test]
    fn merge_drops_empty_ranges() {
        let m = merge_ranges(&[(100, 100), (0, 50)]);
        assert_eq!(m, vec![(0, 50)]);
    }

    // ── ripple cuts ─────────────────────────────────────────────────────────────
    #[test]
    fn ripple_removes_word_inside_cut() {
        let p = from_words(vec![("So", 0, 300), ("um", 300, 600), ("yes", 600, 900)]);
        // Cut the "um" range exactly.
        let out = apply_ripple_cuts(&p, &[(300, 600)], 100).unwrap();
        let words = &out.captions[0].words;
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "So");
        assert_eq!(words[1].text, "yes");
        // "yes" was 600..900; cut removed 300ms before it → now 300..600
        assert_eq!(words[1].start_ms, 300);
        assert_eq!(words[1].end_ms, 600);
    }

    #[test]
    fn ripple_shifts_caption_bounds() {
        let p = from_words(vec![("a", 0, 500), ("b", 500, 1000), ("c", 1000, 1500)]);
        let out = apply_ripple_cuts(&p, &[(500, 1000)], 100).unwrap(); // cut "b"
        assert_eq!(out.captions[0].words.len(), 2);
        // "c" 1000..1500 shifts -500 → 500..1000
        assert_eq!(out.captions[0].words[1].start_ms, 500);
        assert_eq!(out.captions[0].end_ms, 1000);
    }

    #[test]
    fn ripple_multiple_cuts_accumulate() {
        let p = from_words(vec![
            ("a", 0, 200),
            ("um", 200, 400),
            ("b", 400, 600),
            ("uh", 600, 800),
            ("c", 800, 1000),
        ]);
        let out = apply_ripple_cuts(&p, &[(200, 400), (600, 800)], 100).unwrap();
        let w = &out.captions[0].words;
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].text, "a"); // 0..200 unchanged
        // "b" 400..600: 200ms cut before it → 200..400
        assert_eq!(w[1].text, "b");
        assert_eq!(w[1].start_ms, 200);
        // "c" 800..1000: 400ms cut before it → 400..600
        assert_eq!(w[2].text, "c");
        assert_eq!(w[2].start_ms, 400);
        assert_eq!(w[2].end_ms, 600);
    }

    #[test]
    fn ripple_drops_empty_captions() {
        // A caption made entirely of filler should vanish.
        let p = from_words(vec![("um", 0, 300)]);
        let out = apply_ripple_cuts(&p, &[(0, 300)], 100).unwrap();
        assert!(out.captions.is_empty());
    }

    fn two_captions(c1: Vec<(&str, i64, i64)>, c2: Vec<(&str, i64, i64)>) -> Project {
        let mk = |id: &str, ws: Vec<(&str, i64, i64)>| {
            let words: Vec<Word> = ws
                .iter()
                .map(|&(t, s, e)| Word::new(t, s, e, 90.0))
                .collect();
            Caption {
                id: id.into(),
                start_ms: words.first().unwrap().start_ms,
                end_ms: words.last().unwrap().end_ms,
                words,
                speaker_id: None,
                style_id: None,
                notes: None,
                ai_generated: true,
                last_edited_at: 0,
                track_id: None,
            }
        };
        let mut p = from_words(vec![("placeholder", 0, 1)]);
        p.captions = vec![mk("c1", c1), mk("c2", c2)];
        p
    }

    #[test]
    fn ripple_never_produces_overlapping_captions() {
        // A cut straddling the boundary between two captions shifts caption 2's
        // first word earlier (full cut duration) while caption 1's last word —
        // which starts before the cut — does not move, producing an overlap that
        // Project::validate rejects. apply_ripple_cuts must surface that as an
        // error rather than silently committing the corrupted timing.
        let p = two_captions(
            vec![("end", 800, 1000)],   // caption 1: starts before the cut
            vec![("next", 1100, 1300)], // caption 2: starts after the cut
        );
        // Cut 900..1100 straddles the c1/c2 boundary. Either the operation
        // rejects it (Invariant error) or it must return a valid project —
        // it must never silently commit overlapping captions.
        match apply_ripple_cuts(&p, &[(900, 1100)], 100) {
            Ok(out) => out
                .validate()
                .expect("ripple output must satisfy project invariants"),
            Err(AppError::Invariant(_)) => {}
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }

    #[test]
    fn no_cuts_is_noop() {
        let p = from_words(vec![("a", 0, 500)]);
        let out = apply_ripple_cuts(&p, &[], 100).unwrap();
        assert_eq!(out.captions, p.captions);
    }

    #[test]
    fn fillers_to_cuts_maps_ranges() {
        let p = from_words(vec![("um", 100, 400), ("uh", 700, 900)]);
        let hits = detect_fillers(&p, "en");
        let cuts = fillers_to_cuts(&hits);
        assert_eq!(cuts, vec![(100, 400), (700, 900)]);
    }

    #[test]
    fn end_to_end_filler_removal() {
        let p = from_words(vec![
            ("So", 0, 300),
            ("um", 300, 600),
            ("we", 600, 900),
            ("uh", 900, 1200),
            ("go", 1200, 1500),
        ]);
        let hits = detect_fillers(&p, "en");
        let cuts = fillers_to_cuts(&hits);
        let out = apply_ripple_cuts(&p, &cuts, 100).unwrap();
        assert_eq!(out.captions[0].text(), "So we go");
        // total cut = 600ms; "go" 1200..1500 → 600..900
        let go = out.captions[0].words.last().unwrap();
        assert_eq!(go.start_ms, 600);
        assert_eq!(go.end_ms, 900);
    }
}
