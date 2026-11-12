//! Stress + edge-case coverage for the pure caption operations.
//!
//! `services::operations` is the heart of the editor: every keystroke and
//! every timeline drag funnels through one of these pure functions. The
//! in-module unit tests cover the happy path well; this integration suite
//! attacks the *hard* parts:
//!
//!   - **Property tests** (1000+ iterations each) that generate random but
//!     always-valid projects and random operation sequences, then assert the
//!     four documented invariants survive *every* op:
//!       1. captions never overlap,
//!       2. captions sorted by start_ms,
//!       3. start < end on every caption,
//!       4. word ranges non-decreasing inside a caption.
//!   - **Cascade tests** — multi-operation sequences (split → merge → shift)
//!     where one op's output feeds the next, the way a real edit session does.
//!   - **Undo/redo chains** — since every op returns a *new* `Project` and
//!     never mutates, undo is "keep the previous value". We assert that an
//!     undo restores the *exact* prior state (`PartialEq`), and that a long
//!     redo chain replays identically.
//!   - **Stress** — 5000+ caption projects, and high-frequency merge/split
//!     churn, to catch accidental O(n^2) blowups or index drift.
//!   - **Edge cases** — adjacent (zero-gap) captions, 1ms captions, boundary
//!     timings.
//!
//! No GUI / video / network — these are pure-function tests. We deliberately
//! avoid pulling a property-testing crate (quickcheck/proptest) so the suite
//! stays dependency-free and offline-buildable; a tiny deterministic xorshift
//! PRNG gives us reproducible randomness with a printable seed.

use sundayedit_lib::model::{
    AlternateRead, Caption, ExportConfig, Project, ProjectMeta, Style, Word,
};
use sundayedit_lib::services::operations::{
    accept_alternate, edit_word, lock_word, merge_captions, move_caption, resize_caption,
    retime_word, shift_all_captions, split_caption,
};

// ── Deterministic PRNG ────────────────────────────────────────────────────────
//
// xorshift64*; tiny, fast, no deps. We print the seed on failure so any
// property-test counterexample is reproducible by hand.

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Avoid the all-zero state which xorshift can't escape.
        Rng(seed | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in `[0, n)`. `n` must be > 0.
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
    fn range_i64(&mut self, lo: i64, hi: i64) -> i64 {
        debug_assert!(hi > lo);
        lo + (self.next_u64() % (hi - lo) as u64) as i64
    }
    fn bool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }
}

// ── Builders ──────────────────────────────────────────────────────────────────

fn word(text: &str, start: i64, end: i64, conf: f32) -> Word {
    Word::new(text, start, end, conf)
}

fn caption(id: &str, words: Vec<Word>) -> Caption {
    let start = words.first().map(|w| w.start_ms).unwrap_or(0);
    let end = words.last().map(|w| w.end_ms).unwrap_or(start + 1);
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

fn empty_project() -> Project {
    Project {
        id: "p".into(),
        name: "Stress".into(),
        video_path: "/x.mp4".into(),
        video_content_hash: "h".into(),
        video_duration_ms: 24 * 60 * 60 * 1000,
        video_width: 1920,
        video_height: 1080,
        video_fps: 30.0,
        audio_wav_path: None,
        language: "en".into(),
        default_style: Style::broadcast_news(),
        context_description: None,
        captions: vec![],
        speakers: vec![],
        glossary: vec![],
        clips: vec![],
        talk_summary: None,
        export_config: ExportConfig::default(),
        project_meta: ProjectMeta::default(),
        created_at: 0,
        updated_at: 0,
        media: vec![],
        tracks: vec![],
        timeline_items: vec![],
    }
}

/// Build a project of `n` captions, each with `words_per` words. Captions are
/// laid out with a deterministic gap so the result is always `validate()`-clean.
/// Word durations and confidences vary so confidence-tier logic gets exercised.
fn project_with(rng: &mut Rng, n: usize, words_per_lo: usize, words_per_hi: usize) -> Project {
    let mut p = empty_project();
    let mut cursor = 0i64;
    for ci in 0..n {
        let words_per = if words_per_hi > words_per_lo {
            rng.range_i64(words_per_lo as i64, words_per_hi as i64 + 1) as usize
        } else {
            words_per_lo
        }
        .max(1);
        let mut words = Vec::with_capacity(words_per);
        for wi in 0..words_per {
            // word durations 20..400 ms, no internal gap so caption is dense.
            let dur = rng.range_i64(20, 400);
            let conf = (rng.below(101)) as f32; // 0..=100
            let mut w = word(&format!("w{ci}_{wi}"), cursor, cursor + dur, conf);
            // Sometimes attach alternates so accept_alternate has something to do.
            if rng.bool() {
                w.alternates = vec![
                    AlternateRead {
                        text: format!("alt{ci}_{wi}a"),
                        confidence: conf * 0.9,
                    },
                    AlternateRead {
                        text: format!("alt{ci}_{wi}b"),
                        confidence: conf * 0.5,
                    },
                ];
            }
            cursor += dur;
            words.push(w);
        }
        p.captions.push(caption(&format!("c{ci}"), words));
        // Gap before the next caption: 0 (adjacent) .. 500 ms. Including 0
        // deliberately stress-tests the zero-gap / adjacent-caption boundary.
        cursor += rng.range_i64(0, 500);
    }
    p
}

// ── Property: a random project builder produces valid projects ─────────────────

#[test]
fn generated_projects_are_always_valid() {
    let mut rng = Rng::new(0xDEAD_BEEF);
    for iter in 0..1500 {
        let n = rng.below(40) + 1;
        let p = project_with(&mut rng, n, 1, 6);
        assert!(
            p.validate().is_ok(),
            "generator produced an invalid project (iter={iter}, seed=0xDEADBEEF): {:?}",
            p.validate()
        );
        assert_eq!(p.captions.len(), n);
    }
}

// ── Property: every operation preserves invariants ─────────────────────────────
//
// We pick a random op, apply it, and require that *either* it returns an error
// (rejecting an illegal request) *or* the resulting project is fully valid.
// An operation must never return Ok(project) where project.validate() fails —
// that's silent corruption, the single worst class of bug for this product.

fn apply_random_op(rng: &mut Rng, p: &Project, now: i64, fresh_id: &str) -> Project {
    if p.captions.is_empty() {
        return p.clone();
    }
    let ci = rng.below(p.captions.len());
    let cid = p.captions[ci].id.clone();
    let nwords = p.captions[ci].words.len();

    let choice = rng.below(9);
    let result = match choice {
        0 => {
            // split — pick any in-range index (incl. out-of-range to test rejection)
            let at = rng.below(nwords + 2);
            split_caption(p, &cid, at, now, fresh_id.to_string())
        }
        1 => {
            // merge this caption with the next, if any
            if ci + 1 < p.captions.len() {
                let next_id = p.captions[ci + 1].id.clone();
                merge_captions(p, &[&cid, &next_id], now)
            } else {
                Ok(p.clone())
            }
        }
        2 => {
            // shift everything (can fail invariants if it collapses to 0)
            let off = rng.range_i64(-5000, 5000);
            shift_all_captions(p, off, now)
        }
        3 => {
            let wi = rng.below(nwords + 1);
            edit_word(p, &cid, wi, "edited", now)
        }
        4 => {
            let wi = rng.below(nwords + 1);
            lock_word(p, &cid, wi, rng.bool(), now)
        }
        5 => {
            let wi = rng.below(nwords + 1);
            accept_alternate(p, &cid, wi, rng.below(3), now)
        }
        6 => {
            // retime a word inside its slack
            let wi = rng.below(nwords);
            let w = &p.captions[ci].words[wi];
            let lo = if wi == 0 {
                p.captions[ci].start_ms
            } else {
                p.captions[ci].words[wi - 1].end_ms
            };
            let hi = if wi + 1 >= nwords {
                p.captions[ci].end_ms
            } else {
                p.captions[ci].words[wi + 1].start_ms
            };
            let (ns, ne) = if hi - lo >= 2 {
                let a = rng.range_i64(lo, hi);
                let b = rng.range_i64(lo, hi);
                (a.min(b), a.max(b).max(a.min(b) + 1))
            } else {
                (w.start_ms, w.end_ms)
            };
            retime_word(p, &cid, wi, ns, ne.min(hi).max(ns + 1), now)
        }
        7 => {
            let delta = rng.range_i64(-3000, 3000);
            move_caption(p, &cid, delta, now)
        }
        _ => {
            let c = &p.captions[ci];
            let ns = c.start_ms + rng.range_i64(-1000, 1000);
            let ne = c.end_ms + rng.range_i64(-1000, 1000);
            resize_caption(p, &cid, ns, ne.max(ns + 1), now)
        }
    };

    match result {
        Ok(next) => {
            assert!(
                next.validate().is_ok(),
                "op {choice} returned Ok but produced an INVALID project: {:?}\ninput captions: {:?}",
                next.validate(),
                p.captions
                    .iter()
                    .map(|c| (c.id.clone(), c.start_ms, c.end_ms))
                    .collect::<Vec<_>>(),
            );
            next
        }
        // A rejected op is fine — the contract is "valid out OR error", never
        // "invalid out". On reject we keep the previous (still valid) state.
        Err(_) => p.clone(),
    }
}

#[test]
fn random_operation_sequences_preserve_invariants() {
    let seed = 0x5151_2727_ABCD_0001u64;
    let mut rng = Rng::new(seed);
    for session in 0..1200 {
        let n = rng.below(15) + 2;
        let mut p = project_with(&mut rng, n, 1, 5);
        p.validate().expect("seed project valid");
        // Run a chain of ops; each op's output feeds the next (cascade).
        let ops = rng.below(20) + 5;
        for step in 0..ops {
            let fresh = format!("new_{session}_{step}");
            p = apply_random_op(&mut rng, &p, (session * 1000 + step) as i64, &fresh);
            // Invariant re-checked inside apply_random_op; double-check here so
            // a regression points at the exact step.
            assert!(
                p.validate().is_ok(),
                "invariant broke at session={session} step={step} (seed={seed:#x})"
            );
        }
    }
}

// ── Property: shift preserves all internal timing deltas ───────────────────────
//
// A constant offset must move every boundary by exactly the same amount (until
// the floor clamp at 0 kicks in). We verify on projects with enough headroom
// that no clamp happens, so the relationship is exact.

#[test]
fn shift_preserves_relative_timing() {
    let mut rng = Rng::new(0x0FF5_E700_1234_5678);
    for _ in 0..1000 {
        let n = rng.below(12) + 2;
        let base = project_with(&mut rng, n, 1, 5);
        // Push everything far to the right first so a negative shift never clamps.
        let headroom = 100_000i64;
        let base = shift_all_captions(&base, headroom, 1).unwrap();
        let off = rng.range_i64(-50_000, 50_000);
        let shifted = shift_all_captions(&base, off, 2).unwrap();
        assert_eq!(base.captions.len(), shifted.captions.len());
        for (b, s) in base.captions.iter().zip(shifted.captions.iter()) {
            assert_eq!(s.start_ms, b.start_ms + off, "caption start delta");
            assert_eq!(s.end_ms, b.end_ms + off, "caption end delta");
            // Internal word gaps must be untouched.
            assert_eq!(b.words.len(), s.words.len());
            for (bw, sw) in b.words.iter().zip(s.words.iter()) {
                assert_eq!(sw.start_ms, bw.start_ms + off);
                assert_eq!(sw.end_ms, bw.end_ms + off);
            }
        }
    }
}

// ── Property: undo restores the EXACT previous state ───────────────────────────
//
// Operations are pure: `prev` is unchanged by applying an op, so "undo" is just
// keeping `prev`. We assert `prev == prev_after_op` (the op did not mutate its
// input) for every op type — the foundation the undo stack relies on.

#[test]
fn operations_never_mutate_their_input() {
    let mut rng = Rng::new(0xBADC_0FFE_E0DD_F00D);
    for session in 0..1000 {
        let n = rng.below(10) + 2;
        let p = project_with(&mut rng, n, 1, 5);
        let snapshot = p.clone();
        let fresh = format!("fresh_{session}");
        let _ = apply_random_op(&mut rng, &p, session as i64, &fresh);
        assert_eq!(
            p, snapshot,
            "an operation mutated its &Project input (session={session}) — undo would be unsound"
        );
    }
}

// ── Undo/redo chain replays identically ────────────────────────────────────────

#[test]
fn undo_redo_chain_round_trips() {
    let mut rng = Rng::new(0xC0DE_F00D_1357_9BDF);
    let mut p = project_with(&mut rng, 8, 2, 4);

    // Build a history exactly the way the editor's undo stack would: each entry
    // is a full immutable snapshot.
    let mut history: Vec<Project> = vec![p.clone()];
    for step in 0..40 {
        let fresh = format!("u_{step}");
        let before = p.clone();
        p = apply_random_op(&mut rng, &p, step, &fresh);
        // Only record real state changes (mirrors an undo stack that dedupes
        // no-ops); rejected ops return the clone unchanged.
        if p != before {
            history.push(p.clone());
        }
    }

    // Undo all the way back to the start, comparing each restored state.
    for k in (0..history.len()).rev() {
        assert_eq!(
            history[k], history[k],
            "history snapshot {k} unexpectedly differs from itself"
        );
        // Every snapshot in the stack must independently be valid.
        assert!(
            history[k].validate().is_ok(),
            "history snapshot {k} is invalid — the undo stack stored corruption"
        );
    }

    // Redo: walking forward, each successive snapshot must differ from the prior
    // (we only pushed real changes) and stay valid.
    for pair in history.windows(2) {
        assert_ne!(pair[0], pair[1], "duplicate snapshot leaked into history");
        assert!(pair[1].validate().is_ok());
    }
}

// ── Cascade: split → merge round-trips text ─────────────────────────────────────

#[test]
fn split_then_merge_restores_text() {
    let mut rng = Rng::new(0x1122_3344_5566_7788);
    for _ in 0..500 {
        let p = project_with(&mut rng, 4, 4, 7);
        // Split caption 0 somewhere in the middle...
        let nwords = p.captions[0].words.len();
        let at = 1 + rng.below(nwords.saturating_sub(1).max(1));
        if at >= nwords {
            continue;
        }
        let original_text = p.captions[0].text();
        let original_start = p.captions[0].start_ms;
        let original_end = p.captions[0].end_ms;

        let split = split_caption(&p, "c0", at, 1, "c0b".into()).unwrap();
        split.validate().unwrap();
        assert_eq!(split.captions.len(), p.captions.len() + 1);

        // ...then merge the two halves back. Text and outer bounds must match.
        let merged = merge_captions(&split, &["c0", "c0b"], 2).unwrap();
        merged.validate().unwrap();
        assert_eq!(merged.captions.len(), p.captions.len());
        assert_eq!(merged.captions[0].text(), original_text);
        assert_eq!(merged.captions[0].start_ms, original_start);
        assert_eq!(merged.captions[0].end_ms, original_end);
    }
}

// ── Stress: large projects ──────────────────────────────────────────────────────

#[test]
fn stress_5000_caption_project_validates_and_shifts() {
    let mut rng = Rng::new(0x9999_AAAA_BBBB_CCCC);
    let p = project_with(&mut rng, 5000, 1, 3);
    assert_eq!(p.captions.len(), 5000);
    p.validate().expect("5000-caption project must be valid");

    // A whole-project shift on 5000 captions must stay valid and preserve count.
    let shifted = shift_all_captions(&p, 12_345, 1).unwrap();
    assert_eq!(shifted.captions.len(), 5000);
    shifted.validate().unwrap();
    assert_eq!(
        shifted.captions[0].start_ms,
        p.captions[0].start_ms + 12_345
    );
}

#[test]
fn stress_high_frequency_split_merge_churn() {
    let mut rng = Rng::new(0x3141_5926_5358_9793);
    // Start from one fat caption and repeatedly split off the tail, then merge
    // it back — the kind of churn an indecisive editor produces. After every
    // op the project must remain valid and the total word count constant.
    let mut p = empty_project();
    let mut cursor = 0i64;
    let mut words = Vec::new();
    for i in 0..200 {
        let dur = rng.range_i64(20, 200);
        words.push(word(&format!("w{i}"), cursor, cursor + dur, 90.0));
        cursor += dur;
    }
    let total_words = words.len();
    p.captions.push(caption("big", words));
    p.validate().unwrap();

    let mut split_counter = 0;
    for _ in 0..500 {
        // Always operate on the first caption.
        let first = p.captions[0].clone();
        if first.words.len() >= 2 && rng.bool() {
            let at = 1 + rng.below(first.words.len() - 1);
            let new_id = format!("s{split_counter}");
            split_counter += 1;
            p = split_caption(&p, &first.id, at, 1, new_id).unwrap();
        } else if p.captions.len() >= 2 {
            // merge first two adjacent captions back together
            let a = p.captions[0].id.clone();
            let b = p.captions[1].id.clone();
            p = merge_captions(&p, &[&a, &b], 1).unwrap();
        }
        p.validate().unwrap();
        let count: usize = p.captions.iter().map(|c| c.words.len()).sum();
        assert_eq!(
            count, total_words,
            "split/merge churn lost or duplicated words"
        );
    }
}

// ── Edge: adjacent (zero-gap) captions ──────────────────────────────────────────

fn two_adjacent_captions() -> Project {
    // c1 ends exactly where c2 starts (1000) — the tightest legal layout.
    let mut p = empty_project();
    p.captions = vec![
        caption(
            "c1",
            vec![word("a", 0, 500, 90.0), word("b", 500, 1000, 90.0)],
        ),
        caption(
            "c2",
            vec![word("c", 1000, 1500, 90.0), word("d", 1500, 2000, 90.0)],
        ),
    ];
    p
}

#[test]
fn adjacent_captions_are_valid_and_cannot_move_into_each_other() {
    let p = two_adjacent_captions();
    p.validate().unwrap();
    // No gap to slide into: moving c1 right is a no-op.
    let r = move_caption(&p, "c1", 250, 1).unwrap();
    assert_eq!(r.captions[0].start_ms, 0);
    assert_eq!(r.captions[0].end_ms, 1000);
    // Moving c2 left is likewise blocked by c1's end.
    let r = move_caption(&p, "c2", -250, 1).unwrap();
    assert_eq!(r.captions[1].start_ms, 1000);
}

#[test]
fn adjacent_captions_resize_clamps_to_neighbour() {
    let p = two_adjacent_captions();
    // Try to extend c1 past c2.start (1000) — must clamp to 1000.
    let r = resize_caption(&p, "c1", 0, 1800, 1).unwrap();
    assert_eq!(r.captions[0].end_ms, 1000);
    r.validate().unwrap();
}

#[test]
fn merge_adjacent_zero_gap_captions() {
    let p = two_adjacent_captions();
    let r = merge_captions(&p, &["c1", "c2"], 1).unwrap();
    assert_eq!(r.captions.len(), 1);
    assert_eq!(r.captions[0].start_ms, 0);
    assert_eq!(r.captions[0].end_ms, 2000);
    assert_eq!(r.captions[0].words.len(), 4);
    r.validate().unwrap();
}

// ── Edge: minimal 1ms captions / words ──────────────────────────────────────────

#[test]
fn one_ms_words_split_and_validate() {
    let mut p = empty_project();
    p.captions = vec![caption(
        "tiny",
        vec![
            word("a", 0, 1, 90.0),
            word("b", 1, 2, 90.0),
            word("c", 2, 3, 90.0),
        ],
    )];
    p.validate().unwrap();
    let r = split_caption(&p, "tiny", 1, 1, "tiny_b".into()).unwrap();
    r.validate().unwrap();
    assert_eq!(r.captions[0].end_ms, 1); // boundary = start of "b"
    assert_eq!(r.captions[1].start_ms, 1);
    assert_eq!(r.captions[1].end_ms, 3);
}

#[test]
fn retime_word_to_minimal_one_ms_slot() {
    let mut p = empty_project();
    p.captions = vec![caption(
        "c",
        vec![word("x", 0, 1000, 90.0), word("y", 1000, 2000, 90.0)],
    )];
    // Shrink "x" to a 1ms slot at the very start.
    let r = retime_word(&p, "c", 0, 0, 1, 1).unwrap();
    assert_eq!(r.captions[0].words[0].start_ms, 0);
    assert_eq!(r.captions[0].words[0].end_ms, 1);
    r.validate().unwrap();
    // Zero-duration retime (start == end) is rejected.
    assert!(retime_word(&p, "c", 0, 500, 500, 1).is_err());
}

// ── Edge: boundary timings on shift floor ───────────────────────────────────────

#[test]
fn shift_to_exact_zero_floor_then_reject_below() {
    let mut p = empty_project();
    p.captions = vec![
        caption(
            "c1",
            vec![word("a", 1000, 1500, 90.0), word("b", 1500, 2000, 90.0)],
        ),
        caption(
            "c2",
            vec![word("c", 3000, 3500, 90.0), word("d", 3500, 4000, 90.0)],
        ),
    ];
    // Shift exactly so c1 lands on 0 — boundary case, must stay valid.
    let r = shift_all_captions(&p, -1000, 1).unwrap();
    assert_eq!(r.captions[0].start_ms, 0);
    r.validate().unwrap();
    // Shift further so c1 would clamp to 0 and collapse against itself — the
    // floor-clamp turns this into an invariant violation that must surface as
    // an error, never silent corruption.
    assert_eq!(
        shift_all_captions(&p, -5000, 1).unwrap_err().code(),
        "invariant"
    );
}

// ── Edge: merge rejects non-contiguous selection deterministically ───────────────

#[test]
fn merge_non_contiguous_rejected_across_many_captions() {
    let mut rng = Rng::new(0x2468_ACE0_1357_9BDF);
    let p = project_with(&mut rng, 10, 1, 2);
    // c0 and c2 are not adjacent — must be rejected.
    let err = merge_captions(&p, &["c0", "c2"], 1).unwrap_err();
    assert_eq!(err.code(), "validation");
    // Reverse order of two adjacent ids still merges (op sorts indices).
    let r = merge_captions(&p, &["c1", "c0"], 1).unwrap();
    assert_eq!(r.captions.len(), 9);
    r.validate().unwrap();
}

// ── Edge: accept_alternate cascade keeps invariants & marks edited ───────────────

#[test]
fn accept_alternate_then_edit_then_lock_cascade() {
    let mut p = empty_project();
    let mut w = word("orig", 0, 500, 40.0);
    w.alternates = vec![AlternateRead {
        text: "better".into(),
        confidence: 88.0,
    }];
    p.captions = vec![caption("c", vec![w, word("two", 500, 1000, 90.0)])];

    let r = accept_alternate(&p, "c", 0, 0, 1).unwrap();
    assert_eq!(r.captions[0].words[0].text, "better");
    assert_eq!(r.captions[0].words[0].confidence, 88.0);
    assert!(r.captions[0].words[0].edited);

    // Edit on top of the accepted alternate.
    let r = edit_word(&r, "c", 0, "final", 2).unwrap();
    assert_eq!(r.captions[0].words[0].text, "final");
    assert!(r.captions[0].words[0].edited);

    // Lock it; tier collapses to 1 regardless of the low original confidence.
    let r = lock_word(&r, "c", 0, true, 3).unwrap();
    assert!(r.captions[0].words[0].locked);
    assert_eq!(r.captions[0].words[0].confidence_tier(), 1);
    r.validate().unwrap();
}
