//! Pure caption operations.
//!
//! Every public function takes a `&Project` and returns either a new
//! `Project` on success or an `AppError`. The input is never mutated.
//! This means:
//!
//!   - Undo is trivial — push the previous `Project` to a stack.
//!   - Operations compose without surprise.
//!   - Tests are easy: build input, call, assert output.
//!
//! Invariants enforced after every operation:
//!   1. Captions never overlap in time
//!   2. Captions sorted by start_ms
//!   3. start < end always
//!   4. Word ranges non-decreasing within a caption
//!
//! See `Project::validate()` for the canonical check.

use crate::error::{AppError, AppResult};
use crate::model::{Caption, Project, Word};

// ── splitCaption ────────────────────────────────────────────────────────────

/// Split a caption in two at the given word index. The split caption's
/// word at `at_word_index` becomes the first word of the new (second)
/// caption.
///
/// Timing: the boundary is at the start of the word at `at_word_index`.
/// The first caption ends just before that word; the new caption starts
/// at the word's start time.
pub fn split_caption(
    project: &Project,
    caption_id: &str,
    at_word_index: usize,
    now_ms: i64,
    new_caption_id: String,
) -> AppResult<Project> {
    let (caption_index, original) = find_caption(project, caption_id)?;

    if at_word_index == 0 || at_word_index >= original.words.len() {
        return Err(AppError::Validation(format!(
            "split index {} out of range (caption has {} words, can split at 1..{})",
            at_word_index,
            original.words.len(),
            original.words.len() - 1,
        )));
    }

    let (left_words, right_words) = original.words.split_at(at_word_index);
    let left_words = left_words.to_vec();
    let right_words = right_words.to_vec();

    // The boundary is the start of the first right-word.
    let boundary_ms = right_words[0].start_ms;

    let mut left = original.clone();
    left.words = left_words;
    left.end_ms = boundary_ms;
    left.last_edited_at = now_ms;

    let mut right = original.clone();
    right.id = new_caption_id;
    right.words = right_words;
    right.start_ms = boundary_ms;
    right.last_edited_at = now_ms;
    right.ai_generated = original.ai_generated; // preserve provenance

    // Clone the project ONCE and edit its captions in place — cloning
    // `project.captions` separately would copy the vec twice per split.
    let mut next = project.clone();
    next.captions[caption_index] = left;
    next.captions.insert(caption_index + 1, right);
    next.updated_at = now_ms;
    next.validate().map_err(AppError::Invariant)?;
    Ok(next)
}

// ── mergeCaptions ───────────────────────────────────────────────────────────

/// Merge a range of CONTIGUOUS captions (by index, must be in order).
/// The merged caption inherits the first caption's id, speaker, style.
pub fn merge_captions(project: &Project, caption_ids: &[&str], now_ms: i64) -> AppResult<Project> {
    if caption_ids.len() < 2 {
        return Err(AppError::Validation(
            "merge needs at least 2 caption ids".to_string(),
        ));
    }

    let mut indices = Vec::with_capacity(caption_ids.len());
    for id in caption_ids {
        let (idx, _) = find_caption(project, id)?;
        indices.push(idx);
    }
    indices.sort();

    // Must be contiguous in the project's ordering
    for w in indices.windows(2) {
        if w[1] != w[0] + 1 {
            return Err(AppError::Validation(format!(
                "captions {:?} are not contiguous — only adjacent captions can merge",
                caption_ids
            )));
        }
    }

    let first_idx = indices[0];
    let last_idx = *indices.last().unwrap();

    let first = project.captions[first_idx].clone();
    let last = &project.captions[last_idx];

    let mut merged_words: Vec<Word> = Vec::new();
    for idx in first_idx..=last_idx {
        merged_words.extend(project.captions[idx].words.clone());
    }

    let merged = Caption {
        id: first.id.clone(),
        start_ms: first.start_ms,
        end_ms: last.end_ms,
        words: merged_words,
        speaker_id: first.speaker_id,
        style_id: first.style_id,
        notes: first.notes,
        ai_generated: first.ai_generated
            && project.captions[first_idx..=last_idx]
                .iter()
                .all(|c| c.ai_generated),
        last_edited_at: now_ms,
        track_id: None,
    };

    // Clone the project ONCE and edit its captions in place (see split_caption).
    let mut next = project.clone();
    next.captions[first_idx] = merged;
    next.captions.drain(first_idx + 1..=last_idx); // len >= 2 ⇒ last_idx > first_idx
    next.updated_at = now_ms;
    next.validate().map_err(AppError::Invariant)?;
    Ok(next)
}

// ── shiftAllCaptions ────────────────────────────────────────────────────────

/// Nudge every caption by `offset_ms` (negative = earlier, positive = later).
/// Useful for fixing a constant offset between audio and Whisper output.
pub fn shift_all_captions(project: &Project, offset_ms: i64, now_ms: i64) -> AppResult<Project> {
    if offset_ms == 0 {
        return Ok(project.clone());
    }

    let mut next = project.clone();
    for c in next.captions.iter_mut() {
        c.start_ms = c.start_ms.saturating_add(offset_ms).max(0);
        c.end_ms = c.end_ms.saturating_add(offset_ms).max(0);
        for w in c.words.iter_mut() {
            w.start_ms = w.start_ms.saturating_add(offset_ms).max(0);
            w.end_ms = w.end_ms.saturating_add(offset_ms).max(0);
        }
        c.last_edited_at = now_ms;
    }
    next.updated_at = now_ms;
    next.validate().map_err(AppError::Invariant)?;
    Ok(next)
}

// ── editWord ────────────────────────────────────────────────────────────────

/// Replace a word's text. Marks the word as `edited` (which forces tier
/// 1 — no more confidence highlighting on this word).
pub fn edit_word(
    project: &Project,
    caption_id: &str,
    word_index: usize,
    new_text: &str,
    now_ms: i64,
) -> AppResult<Project> {
    let new_text = new_text.trim();
    if new_text.is_empty() {
        return Err(AppError::Validation(
            "word text cannot be empty".to_string(),
        ));
    }

    let mut next = project.clone();
    let cap = mutable_caption(&mut next, caption_id)?;
    if word_index >= cap.words.len() {
        return Err(AppError::Validation(format!(
            "word index {} out of range (caption has {} words)",
            word_index,
            cap.words.len()
        )));
    }
    let w = &mut cap.words[word_index];
    if w.text != new_text {
        w.text = new_text.to_string();
        w.edited = true;
    }
    cap.last_edited_at = now_ms;
    next.updated_at = now_ms;
    Ok(next)
}

// ── lockWord ────────────────────────────────────────────────────────────────

/// Mark a word as confirmed by the user. Removes the confidence highlight
/// even if the confidence score is low.
pub fn lock_word(
    project: &Project,
    caption_id: &str,
    word_index: usize,
    locked: bool,
    now_ms: i64,
) -> AppResult<Project> {
    let mut next = project.clone();
    let cap = mutable_caption(&mut next, caption_id)?;
    if word_index >= cap.words.len() {
        return Err(AppError::Validation(format!(
            "word index {} out of range",
            word_index
        )));
    }
    cap.words[word_index].locked = locked;
    cap.last_edited_at = now_ms;
    next.updated_at = now_ms;
    Ok(next)
}

// ── acceptAlternate ─────────────────────────────────────────────────────────

/// Replace a word with one of its ASR alternates. Marks the word as
/// `edited` so it loses its confidence highlight.
pub fn accept_alternate(
    project: &Project,
    caption_id: &str,
    word_index: usize,
    alternate_index: usize,
    now_ms: i64,
) -> AppResult<Project> {
    let mut next = project.clone();
    let cap = mutable_caption(&mut next, caption_id)?;
    if word_index >= cap.words.len() {
        return Err(AppError::Validation(format!(
            "word index {} out of range",
            word_index
        )));
    }
    let w = &mut cap.words[word_index];
    let alt = w.alternates.get(alternate_index).ok_or_else(|| {
        AppError::Validation(format!(
            "alternate index {} out of range ({} alternates)",
            alternate_index,
            w.alternates.len()
        ))
    })?;
    let new_text = alt.text.clone();
    let new_confidence = alt.confidence;
    w.text = new_text;
    w.confidence = new_confidence;
    w.edited = true;
    cap.last_edited_at = now_ms;
    next.updated_at = now_ms;
    Ok(next)
}

// ── retimeWord ──────────────────────────────────────────────────────────────

/// Manually adjust a word's timing. The new range must stay within the
/// caption's range and not cross adjacent words.
pub fn retime_word(
    project: &Project,
    caption_id: &str,
    word_index: usize,
    new_start_ms: i64,
    new_end_ms: i64,
    now_ms: i64,
) -> AppResult<Project> {
    if new_start_ms >= new_end_ms {
        return Err(AppError::Validation(
            "start must be less than end".to_string(),
        ));
    }

    let mut next = project.clone();
    let cap = mutable_caption(&mut next, caption_id)?;
    if word_index >= cap.words.len() {
        return Err(AppError::Validation(format!(
            "word index {} out of range",
            word_index
        )));
    }
    // Bounds vs caption + neighbours
    let lower_bound = if word_index == 0 {
        cap.start_ms
    } else {
        cap.words[word_index - 1].end_ms
    };
    let upper_bound = if word_index + 1 >= cap.words.len() {
        cap.end_ms
    } else {
        cap.words[word_index + 1].start_ms
    };
    if new_start_ms < lower_bound || new_end_ms > upper_bound {
        return Err(AppError::Validation(format!(
            "retime ({}, {}) outside allowed bounds [{}, {}]",
            new_start_ms, new_end_ms, lower_bound, upper_bound
        )));
    }
    let w = &mut cap.words[word_index];
    w.start_ms = new_start_ms;
    w.end_ms = new_end_ms;
    cap.last_edited_at = now_ms;
    next.updated_at = now_ms;
    Ok(next)
}

// ── moveCaption (timeline drag) ───────────────────────────────────────────────

/// Slide a whole caption (and all its words) by `delta_ms`, used when the user
/// drags a caption box along the timeline. The move is clamped so the caption
/// can't cross its neighbours or go negative — a partial slide stops at the gap,
/// NLE-style. Returns the project unchanged if there's no room to move.
pub fn move_caption(
    project: &Project,
    caption_id: &str,
    delta_ms: i64,
    now_ms: i64,
) -> AppResult<Project> {
    if delta_ms == 0 {
        return Ok(project.clone());
    }

    let mut next = project.clone();
    let (idx, _) = find_caption(&next, caption_id)?;

    let prev_end = if idx > 0 {
        next.captions[idx - 1].end_ms
    } else {
        0
    };
    let next_start = next.captions.get(idx + 1).map(|c| c.start_ms);

    let cap = &next.captions[idx];
    let dur = cap.end_ms - cap.start_ms;
    let lo = prev_end.max(0);
    // With no right neighbour the caption is unbounded on the right, but the
    // ceiling must still leave room for `end_ms = start_ms + dur` (and the
    // later `+= applied`) so an extreme delta_ms from the IPC boundary can't
    // overflow. saturating_sub keeps it well within i64 range.
    let hi = next_start
        .map(|ns| ns - dur)
        .unwrap_or_else(|| i64::MAX.saturating_sub(dur));

    let applied = if hi < lo {
        0 // no gap to move into
    } else {
        cap.start_ms.saturating_add(delta_ms).clamp(lo, hi) - cap.start_ms
    };
    if applied == 0 {
        return Ok(project.clone());
    }

    let cap = &mut next.captions[idx];
    cap.start_ms += applied;
    cap.end_ms += applied;
    for w in cap.words.iter_mut() {
        w.start_ms += applied;
        w.end_ms += applied;
    }
    cap.last_edited_at = now_ms;
    next.updated_at = now_ms;
    next.validate().map_err(AppError::Invariant)?;
    Ok(next)
}

// ── resizeCaption (timeline edge drag) ──────────────────────────────────────────

/// Adjust a caption's start/end edges (timeline edge drag). The new bounds are
/// clamped so the caption can't overlap a neighbour and can't shrink past its
/// own words (you can't hide content) — words are left untouched.
pub fn resize_caption(
    project: &Project,
    caption_id: &str,
    new_start_ms: i64,
    new_end_ms: i64,
    now_ms: i64,
) -> AppResult<Project> {
    if new_start_ms >= new_end_ms {
        return Err(AppError::Validation(
            "start must be less than end".to_string(),
        ));
    }

    let mut next = project.clone();
    let (idx, _) = find_caption(&next, caption_id)?;

    let prev_end = if idx > 0 {
        next.captions[idx - 1].end_ms
    } else {
        0
    };
    let next_start = next.captions.get(idx + 1).map(|c| c.start_ms);

    let cap = &mut next.captions[idx];
    let words_lo = cap.words.first().map(|w| w.start_ms);
    let words_hi = cap.words.last().map(|w| w.end_ms);

    // Start edge: at most the first word's start, at least the previous caption end.
    let mut start = new_start_ms.max(prev_end).max(0);
    if let Some(wl) = words_lo {
        start = start.min(wl);
    }
    // End edge: at least the last word's end, at most the next caption start.
    let mut end = new_end_ms;
    if let Some(ns) = next_start {
        end = end.min(ns);
    }
    if let Some(wh) = words_hi {
        end = end.max(wh);
    }
    if start >= end {
        return Err(AppError::Validation(
            "resize leaves the caption with no duration".to_string(),
        ));
    }

    cap.start_ms = start;
    cap.end_ms = end;
    cap.last_edited_at = now_ms;
    next.updated_at = now_ms;
    next.validate().map_err(AppError::Invariant)?;
    Ok(next)
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn find_caption<'a>(project: &'a Project, id: &str) -> AppResult<(usize, &'a Caption)> {
    project
        .captions
        .iter()
        .enumerate()
        .find(|(_, c)| c.id == id)
        .ok_or_else(|| AppError::NotFound {
            entity: "caption",
            id: id.to_string(),
        })
}

fn mutable_caption<'a>(project: &'a mut Project, id: &str) -> AppResult<&'a mut Caption> {
    project
        .captions
        .iter_mut()
        .find(|c| c.id == id)
        .ok_or_else(|| AppError::NotFound {
            entity: "caption",
            id: id.to_string(),
        })
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AlternateRead, Style};

    fn word(text: &str, start: i64, end: i64, conf: f32) -> Word {
        Word::new(text, start, end, conf)
    }

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

    fn fixture() -> Project {
        Project {
            id: "p1".into(),
            name: "Test".into(),
            video_path: "/x.mp4".into(),
            video_content_hash: "abc".into(),
            video_duration_ms: 60_000,
            video_width: 1920,
            video_height: 1080,
            video_fps: 30.0,
            audio_wav_path: None,
            language: "en".into(),
            default_style: Style::broadcast_news(),
            context_description: None,
            captions: vec![
                caption(
                    "c1",
                    0,
                    2000,
                    vec![
                        word("Hello", 0, 500, 95.0),
                        word("there", 500, 1000, 88.0),
                        word("world", 1000, 1500, 60.0),
                        word("again", 1500, 2000, 35.0),
                    ],
                ),
                caption(
                    "c2",
                    3000,
                    5000,
                    vec![
                        word("This", 3000, 3300, 92.0),
                        word("is", 3300, 3500, 90.0),
                        word("two", 3500, 4000, 75.0),
                    ],
                ),
            ],
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

    // ── moveCaption ────────────────────────────────────────────────────────
    #[test]
    fn move_caption_slides_caption_and_words_together() {
        let p = fixture();
        // c1 [0,2000] can move right up to where c2 starts (3000) − dur (2000) = 1000.
        let next = move_caption(&p, "c1", 800, 1).unwrap();
        let c1 = &next.captions[0];
        assert_eq!((c1.start_ms, c1.end_ms), (800, 2800));
        assert_eq!(c1.words[0].start_ms, 800); // first word slid with it
        assert_eq!(c1.words[3].end_ms, 2800);
        next.validate().unwrap();
    }

    #[test]
    fn move_caption_clamps_at_neighbour() {
        let p = fixture();
        // Asking to move c1 way right: clamps so end stops at c2.start (3000).
        let next = move_caption(&p, "c1", 9999, 1).unwrap();
        let c1 = &next.captions[0];
        assert_eq!(c1.end_ms, 3000);
        assert_eq!(c1.start_ms, 1000);
        next.validate().unwrap();
    }

    #[test]
    fn move_caption_clamps_at_zero() {
        let p = fixture();
        let next = move_caption(&p, "c1", -500, 1).unwrap();
        assert_eq!(next.captions[0].start_ms, 0); // already at 0, can't go left
    }

    #[test]
    fn move_caption_handles_extreme_delta_on_last_caption() {
        let p = fixture();
        // c2 is the last caption (no neighbour to the right → hi = i64::MAX).
        // A near-MAX delta from the IPC boundary must not overflow; the caption
        // stays put (no room to slide a full i64::MAX) instead of panicking
        // (debug) or wrapping to a large negative and sliding LEFT (release).
        let next = move_caption(&p, "c2", i64::MAX, 1).unwrap();
        let c2 = &next.captions[1];
        assert!(
            c2.start_ms >= 3000,
            "extreme positive delta must never move the caption left, got start_ms={}",
            c2.start_ms
        );
        next.validate().unwrap();
    }

    // ── resizeCaption ──────────────────────────────────────────────────────
    #[test]
    fn resize_caption_extends_end_into_gap() {
        let p = fixture();
        let next = resize_caption(&p, "c1", 0, 2500, 1).unwrap();
        assert_eq!(next.captions[0].end_ms, 2500);
        next.validate().unwrap();
    }

    #[test]
    fn resize_caption_cannot_cross_next_caption() {
        let p = fixture();
        // Try to extend c1 end past c2.start (3000) → clamped to 3000.
        let next = resize_caption(&p, "c1", 0, 4000, 1).unwrap();
        assert_eq!(next.captions[0].end_ms, 3000);
        next.validate().unwrap();
    }

    #[test]
    fn resize_caption_cannot_shrink_past_its_words() {
        let p = fixture();
        // c1's last word ends at 2000; asking end=1200 clamps up to 2000.
        let next = resize_caption(&p, "c1", 0, 1200, 1).unwrap();
        assert_eq!(next.captions[0].end_ms, 2000);
        // Start can't pass first word start (0) either.
        let next = resize_caption(&p, "c2", 3200, 5000, 1).unwrap();
        assert_eq!(next.captions[1].start_ms, 3000); // first word at 3000
        next.validate().unwrap();
    }

    // ── confidence tier ────────────────────────────────────────────────────
    #[test]
    fn confidence_tier_boundaries() {
        assert_eq!(word("a", 0, 1, 100.0).confidence_tier(), 1);
        assert_eq!(word("a", 0, 1, 85.0).confidence_tier(), 1);
        assert_eq!(word("a", 0, 1, 84.9).confidence_tier(), 2);
        assert_eq!(word("a", 0, 1, 70.0).confidence_tier(), 2);
        assert_eq!(word("a", 0, 1, 69.9).confidence_tier(), 3);
        assert_eq!(word("a", 0, 1, 50.0).confidence_tier(), 3);
        assert_eq!(word("a", 0, 1, 49.9).confidence_tier(), 4);
        assert_eq!(word("a", 0, 1, 0.0).confidence_tier(), 4);
    }

    #[test]
    fn locked_word_is_tier_1_regardless_of_confidence() {
        let mut w = word("a", 0, 1, 10.0);
        assert_eq!(w.confidence_tier(), 4);
        w.locked = true;
        assert_eq!(w.confidence_tier(), 1);
    }

    #[test]
    fn edited_word_is_tier_1_regardless_of_confidence() {
        let mut w = word("a", 0, 1, 10.0);
        assert_eq!(w.confidence_tier(), 4);
        w.edited = true;
        assert_eq!(w.confidence_tier(), 1);
    }

    // ── Caption helpers ───────────────────────────────────────────────────
    #[test]
    fn text_derived_from_words() {
        let p = fixture();
        assert_eq!(p.captions[0].text(), "Hello there world again");
    }

    #[test]
    fn uncertain_word_count_respects_locked_and_edited() {
        let mut c = fixture().captions[0].clone();
        // 95, 88, 60, 35 — at threshold 70, two are uncertain (60 and 35)
        assert_eq!(c.uncertain_word_count(70.0), 2);
        c.words[2].locked = true; // 60 → locked → not uncertain
        assert_eq!(c.uncertain_word_count(70.0), 1);
        c.words[3].edited = true; // 35 → edited → not uncertain
        assert_eq!(c.uncertain_word_count(70.0), 0);
    }

    // ── split_caption ─────────────────────────────────────────────────────
    #[test]
    fn split_at_word_2_produces_two_captions() {
        let p = fixture();
        let r = split_caption(&p, "c1", 2, 100, "c1b".into()).unwrap();
        assert_eq!(r.captions.len(), 3);
        assert_eq!(r.captions[0].id, "c1");
        assert_eq!(r.captions[0].words.len(), 2);
        assert_eq!(r.captions[0].end_ms, 1000); // boundary = start of "world"
        assert_eq!(r.captions[1].id, "c1b");
        assert_eq!(r.captions[1].words.len(), 2);
        assert_eq!(r.captions[1].start_ms, 1000);
        assert_eq!(r.captions[1].end_ms, 2000);
        assert_eq!(r.captions[2].id, "c2");
    }

    #[test]
    fn split_at_index_0_rejected() {
        let p = fixture();
        let err = split_caption(&p, "c1", 0, 100, "x".into()).unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    #[test]
    fn split_at_last_index_rejected() {
        let p = fixture();
        // Last valid split index is words.len() - 1 = 3; len() = 4 should reject
        let err = split_caption(&p, "c1", 4, 100, "x".into()).unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    #[test]
    fn split_missing_caption() {
        let p = fixture();
        let err = split_caption(&p, "no-such-id", 1, 100, "x".into()).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    // ── merge_captions ────────────────────────────────────────────────────
    #[test]
    fn merge_two_adjacent_captions() {
        let p = fixture();
        let r = merge_captions(&p, &["c1", "c2"], 100).unwrap();
        assert_eq!(r.captions.len(), 1);
        assert_eq!(r.captions[0].id, "c1");
        assert_eq!(r.captions[0].start_ms, 0);
        assert_eq!(r.captions[0].end_ms, 5000);
        assert_eq!(r.captions[0].words.len(), 7);
        assert_eq!(r.captions[0].text(), "Hello there world again This is two");
    }

    #[test]
    fn merge_single_caption_rejected() {
        let p = fixture();
        let err = merge_captions(&p, &["c1"], 100).unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    // ── shift_all_captions ────────────────────────────────────────────────
    #[test]
    fn shift_positive_moves_everything_later() {
        let p = fixture();
        let r = shift_all_captions(&p, 1000, 100).unwrap();
        assert_eq!(r.captions[0].start_ms, 1000);
        assert_eq!(r.captions[0].end_ms, 3000);
        assert_eq!(r.captions[0].words[0].start_ms, 1000);
        assert_eq!(r.captions[1].end_ms, 6000);
    }

    #[test]
    fn shift_negative_moves_everything_earlier_with_headroom() {
        // Give the project headroom first (+2000), then shift back -500.
        // Net: everything 1500ms later than the fixture, no floor clamp.
        let p = shift_all_captions(&fixture(), 2000, 50).unwrap();
        let r = shift_all_captions(&p, -500, 100).unwrap();
        assert_eq!(r.captions[0].start_ms, 1500); // 0 + 2000 - 500
        assert_eq!(r.captions[0].end_ms, 3500); // 2000 + 2000 - 500
        assert_eq!(r.captions[1].start_ms, 4500); // 3000 + 2000 - 500
        assert_eq!(r.captions[1].end_ms, 6500);
    }

    #[test]
    fn shift_that_would_collapse_captions_is_rejected_by_invariants() {
        let p = fixture();
        // -10000 collapses both captions toward 0, violating no-overlap /
        // start<end. The operation must surface this as an invariant error
        // rather than silently corrupt state.
        let err = shift_all_captions(&p, -10_000, 100).unwrap_err();
        assert_eq!(err.code(), "invariant");
    }

    // ── edit_word ─────────────────────────────────────────────────────────
    #[test]
    fn edit_word_marks_edited() {
        let p = fixture();
        let r = edit_word(&p, "c1", 2, "World", 100).unwrap();
        assert_eq!(r.captions[0].words[2].text, "World");
        assert!(r.captions[0].words[2].edited);
        assert_eq!(r.captions[0].last_edited_at, 100);
    }

    #[test]
    fn edit_word_unchanged_text_does_not_mark_edited() {
        let p = fixture();
        let r = edit_word(&p, "c1", 2, "world", 100).unwrap();
        assert!(!r.captions[0].words[2].edited);
    }

    #[test]
    fn edit_word_empty_rejected() {
        let p = fixture();
        let err = edit_word(&p, "c1", 2, "   ", 100).unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    #[test]
    fn edit_word_out_of_range_rejected() {
        let p = fixture();
        let err = edit_word(&p, "c1", 99, "x", 100).unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    // ── lock_word ─────────────────────────────────────────────────────────
    #[test]
    fn lock_word_toggles_locked_field() {
        let p = fixture();
        let r = lock_word(&p, "c1", 3, true, 100).unwrap();
        assert!(r.captions[0].words[3].locked);
        let r2 = lock_word(&r, "c1", 3, false, 200).unwrap();
        assert!(!r2.captions[0].words[3].locked);
    }

    // ── accept_alternate ──────────────────────────────────────────────────
    #[test]
    fn accept_alternate_replaces_text_and_confidence() {
        let mut p = fixture();
        p.captions[0].words[3].alternates = vec![
            AlternateRead {
                text: "again,".into(),
                confidence: 80.0,
            },
            AlternateRead {
                text: "Egypt".into(),
                confidence: 30.0,
            },
        ];
        let r = accept_alternate(&p, "c1", 3, 0, 100).unwrap();
        assert_eq!(r.captions[0].words[3].text, "again,");
        assert_eq!(r.captions[0].words[3].confidence, 80.0);
        assert!(r.captions[0].words[3].edited);
    }

    #[test]
    fn accept_alternate_out_of_range_rejected() {
        let mut p = fixture();
        p.captions[0].words[3].alternates = vec![AlternateRead {
            text: "again,".into(),
            confidence: 80.0,
        }];
        let err = accept_alternate(&p, "c1", 3, 5, 100).unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    // ── retime_word ───────────────────────────────────────────────────────
    #[test]
    fn retime_word_within_bounds() {
        let p = fixture();
        let r = retime_word(&p, "c1", 1, 600, 900, 100).unwrap();
        assert_eq!(r.captions[0].words[1].start_ms, 600);
        assert_eq!(r.captions[0].words[1].end_ms, 900);
    }

    #[test]
    fn retime_word_crossing_neighbour_rejected() {
        let p = fixture();
        // Word 1 is "there" 500..1000. Word 0 ends at 500.
        // Trying to start at 400 should be rejected (encroaches on word 0).
        let err = retime_word(&p, "c1", 1, 400, 900, 100).unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    #[test]
    fn retime_word_inverted_rejected() {
        let p = fixture();
        let err = retime_word(&p, "c1", 1, 800, 600, 100).unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    // ── invariants ────────────────────────────────────────────────────────
    #[test]
    fn validate_accepts_fixture() {
        assert!(fixture().validate().is_ok());
    }

    #[test]
    fn validate_rejects_overlap() {
        let mut p = fixture();
        p.captions[1].start_ms = 1500; // overlaps c1 which ends at 2000
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_rejects_start_after_end() {
        let mut p = fixture();
        p.captions[0].start_ms = 3000;
        p.captions[0].end_ms = 2000;
        assert!(p.validate().is_err());
    }
}
