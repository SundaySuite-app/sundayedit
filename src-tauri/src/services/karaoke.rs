//! Karaoke word timing — the SHARED source of truth (E4a).
//!
//! Both consumers of per-word caption timing derive from this module:
//!
//!   1. `services::export::write_ass` — emits `{\kNN}` / `{\kfNN}` runs for
//!      libass burn-in (final export + preview proxy).
//!   2. the frontend canvas overlay — mirrors [`KaraokeWord`] / [`WordState`]
//!      (ts-rs-exported to `src/lib/bindings`) so the live preview and the
//!      burned frame agree on which word is lit at any instant.
//!
//! Keeping ONE derivation is the whole point: ASS `\k` durations are
//! CUMULATIVE — libass adds them up from the Dialogue `Start`, so a single
//! centisecond of rounding drift on word 3 shifts every word after it. If the
//! canvas rounded independently, the two sides would diverge further with each
//! word and the flagship promise ("what you saw in preview is what you get")
//! would quietly break. So the durations here are computed from a CUMULATIVE
//! centisecond ladder, never by rounding each word's own span.
//!
//! ### Span derivation
//!
//! ASR word timings have gaps (breaths, silences) and — rarely — overlaps or
//! zero-length spans. Karaoke needs a partition: contiguous spans whose union
//! is exactly the caption's own span, so the sum of `\k` durations equals the
//! Dialogue duration. We therefore:
//!
//!   - anchor the first span at the caption start and the last at the caption end,
//!   - cut at each word's own `start_ms` (clamped into the caption and forced
//!     non-decreasing), which extends every word forward across the gap that
//!     follows it — the standard karaoke behaviour: the current word stays lit
//!     through the pause until the next one begins,
//!   - fall back to a single span covering the whole caption when there are no
//!     words at all.
//!
//! Pure module: no IO, no ffmpeg, no Tauri. Every function is a total function
//! of its inputs.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::Caption;

// ── Types ───────────────────────────────────────────────────────────────────

/// One karaoke-timed word: the derived (gap-closed) span plus the ASS
/// centisecond duration that `{\k}` / `{\kf}` consumes.
///
/// `duration_cs` is NOT `round((end_ms - start_ms) / 10)` — see the module
/// docs. It is the step of a cumulative centisecond ladder anchored on the
/// caption's own ASS timestamps, so `sum(duration_cs)` is exactly the Dialogue
/// span in centiseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/KaraokeWord.ts")]
pub struct KaraokeWord {
    pub text: String,
    // i64 in Rust, serialized as a JSON number by Tauri — tell ts-rs to emit
    // `number` (not `bigint`), same convention as `Word`/`Caption`.
    #[ts(type = "number")]
    pub start_ms: i64,
    #[ts(type = "number")]
    pub end_ms: i64,
    /// ASS centiseconds for this word's `\k` step.
    pub duration_cs: i32,
    /// 0–100 ASR confidence, carried through for confidence tinting.
    pub confidence: f32,
}

/// Where a word sits relative to the playhead. The canvas overlay MUST mirror
/// this predicate exactly — it is the visual contract between preview and
/// burn-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export, export_to = "../../src/lib/bindings/WordState.ts")]
pub enum WordState {
    /// Not reached yet — rendered in the pending colour (ASS SecondaryColour).
    Pending,
    /// The playhead is inside this word's span — rendered in the primary colour.
    Active,
    /// Already sung — rendered in the primary colour.
    Done,
}

/// Which karaoke tag family the ASS writer emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export, export_to = "../../src/lib/bindings/KaraokeStyle.ts")]
pub enum KaraokeStyle {
    /// `\k` — instant per-word switch from SecondaryColour to PrimaryColour.
    Highlight,
    /// `\kf` — smooth left-to-right fill across the word.
    Sweep,
}

impl KaraokeStyle {
    /// The ASS override tag name (without the leading backslash).
    pub fn tag(self) -> &'static str {
        match self {
            KaraokeStyle::Highlight => "k",
            KaraokeStyle::Sweep => "kf",
        }
    }
}

/// Persisted karaoke settings. Lives on `ExportConfig` (the per-project
/// container that already persists burn-in styling), so the sidecar `.ass`,
/// the final burn-in and the preview proxy all read the SAME values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/KaraokeOptions.ts")]
pub struct KaraokeOptions {
    /// Off by default — enabling it changes every Dialogue line.
    pub enabled: bool,
    pub style: KaraokeStyle,
    /// Colour of not-yet-sung text — becomes the ASS SecondaryColour.
    /// Hex `#RRGGBB`; invalid values fall back to white (`hex_to_ass_bgr`).
    pub pending_color: String,
    /// Tint low-confidence words with an inline `\c` override. OFF by default:
    /// surfacing uncertainty is an EDITOR affordance, and baking it into the
    /// delivered video is a deliberate choice, not a surprise.
    pub confidence_tint: bool,
    /// Words below this 0–100 confidence get the tint. Default 70 — the
    /// tier-2 floor from `docs/CALIBRATION.md`.
    pub confidence_threshold: f32,
    /// Hex `#RRGGBB` used for tinted (low-confidence) words.
    pub low_confidence_color: String,
}

impl Default for KaraokeOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            style: KaraokeStyle::Highlight,
            pending_color: "#7A7A7A".into(),
            confidence_tint: false,
            confidence_threshold: 70.0,
            low_confidence_color: "#F5A524".into(),
        }
    }
}

impl KaraokeOptions {
    /// Explicitly-off options — the state every pre-E4a project deserializes to.
    pub fn disabled() -> Self {
        Self::default()
    }
}

// ── Derivation ──────────────────────────────────────────────────────────────

/// Internal pre-duration span. Carries the trust flags so
/// [`uncertain_flags`] cannot drift out of alignment with [`karaoke_words`] —
/// both are built from this one list.
struct Span {
    text: String,
    start_ms: i64,
    end_ms: i64,
    confidence: f32,
    /// User-locked or user-edited → trusted regardless of ASR confidence
    /// (same rule as `Caption::uncertain_word_count` and the editor's tiers).
    trusted: bool,
}

/// Milliseconds → ASS centiseconds, using EXACTLY the truncation
/// `export::fmt_ass_time` applies to the Dialogue `Start`/`End` stamps
/// (negatives clamp to 0). Anchoring the ladder on this function is what makes
/// `sum(duration_cs)` equal the rendered Dialogue span rather than merely
/// approximate it.
fn to_cs(ms: i64) -> i64 {
    ms.max(0) / 10
}

/// Partition the caption into contiguous, non-overlapping spans covering
/// exactly `[caption.start_ms, caption.end_ms]`.
fn spans(caption: &Caption) -> Vec<Span> {
    let cap_start = caption.start_ms;
    // An inverted caption (end < start) would otherwise produce negative
    // durations; collapse it to a zero-length span instead.
    let cap_end = caption.end_ms.max(cap_start);

    if caption.words.is_empty() {
        // Fallback: one span = the whole caption. `Caption::text()` is "" here,
        // which still yields a well-formed (if empty) Dialogue line.
        return vec![Span {
            text: caption.text(),
            start_ms: cap_start,
            end_ms: cap_end,
            confidence: 100.0,
            trusted: true,
        }];
    }

    let n = caption.words.len();
    // Cut points: b[0] = caption start, b[n] = caption end, and b[i] is where
    // word i begins. Clamping into the caption handles words that sit outside
    // its bounds; the running `max` handles out-of-order/overlapping ASR words
    // and words whose span is zero or negative (they simply get 0 duration
    // instead of stealing time from a neighbour).
    let mut cuts: Vec<i64> = Vec::with_capacity(n + 1);
    cuts.push(cap_start);
    for w in caption.words.iter().skip(1) {
        let prev = *cuts.last().expect("cuts is never empty");
        cuts.push(w.start_ms.clamp(cap_start, cap_end).max(prev));
    }
    cuts.push(cap_end);

    caption
        .words
        .iter()
        .enumerate()
        .map(|(i, w)| Span {
            text: w.text.clone(),
            start_ms: cuts[i],
            end_ms: cuts[i + 1],
            confidence: w.confidence,
            trusted: w.locked || w.edited,
        })
        .collect()
}

/// Derive the karaoke words for a caption — the single source of truth shared
/// by the ASS writer and the canvas overlay.
///
/// Guarantees (all covered by tests):
///   - spans are contiguous and cover exactly the caption span,
///   - `sum(duration_cs)` == the Dialogue span in centiseconds, exactly,
///   - `duration_cs >= 0` for every word,
///   - the result is never empty (empty word lists fall back to one span).
pub fn karaoke_words(caption: &Caption) -> Vec<KaraokeWord> {
    let spans = spans(caption);
    let start_cs = to_cs(caption.start_ms);
    let end_cs = to_cs(caption.end_ms.max(caption.start_ms));
    let last = spans.len() - 1; // `spans` is never empty

    let mut out = Vec::with_capacity(spans.len());
    let mut prev_cum = 0i64;
    for (i, s) in spans.into_iter().enumerate() {
        // Cumulative centiseconds elapsed at the END of this word. The last
        // word is pinned to the caption end so the ladder closes exactly on
        // the Dialogue `End` stamp — no accumulated drift, ever.
        let cum = if i == last {
            end_cs - start_cs
        } else {
            to_cs(s.end_ms) - start_cs
        }
        .max(prev_cum);
        let duration_cs = (cum - prev_cum) as i32;
        prev_cum = cum;
        out.push(KaraokeWord {
            text: s.text,
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            duration_cs,
            confidence: s.confidence,
        });
    }
    out
}

/// Which karaoke words should be tinted as low-confidence, index-aligned with
/// [`karaoke_words`] (same length, same order — both are built from the same
/// internal span list, so they cannot drift apart).
///
/// Locked and user-edited words are trusted and never flagged — the same rule
/// `Caption::uncertain_word_count` and the editor's confidence tiers use.
pub fn uncertain_flags(caption: &Caption, threshold: f32) -> Vec<bool> {
    spans(caption)
        .iter()
        .map(|s| !s.trusted && s.confidence < threshold)
        .collect()
}

/// State of ONE word at time `t_ms` (absolute project time, same clock as the
/// caption's `start_ms`/`end_ms`).
///
/// Half-open on the right (`start <= t < end`) so exactly one word is Active
/// at any instant inside the caption — a word's end IS the next word's start.
pub fn word_state_at(word: &KaraokeWord, t_ms: i64) -> WordState {
    if t_ms < word.start_ms {
        WordState::Pending
    } else if t_ms < word.end_ms {
        WordState::Active
    } else {
        WordState::Done
    }
}

/// State of EVERY word at `t_ms` — the exact predicate the canvas overlay
/// mirrors. Cheap enough to call per frame.
pub fn word_states_at(words: &[KaraokeWord], t_ms: i64) -> Vec<WordState> {
    words.iter().map(|w| word_state_at(w, t_ms)).collect()
}

/// Index of the word that is Active at `t_ms`, if any. Convenience for
/// scrubbing/auto-scroll; `None` before the first word, after the last, or when
/// the active span happens to be zero-length.
pub fn active_index_at(words: &[KaraokeWord], t_ms: i64) -> Option<usize> {
    words
        .iter()
        .position(|w| word_state_at(w, t_ms) == WordState::Active)
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Word;

    fn cap(start_ms: i64, end_ms: i64, words: Vec<Word>) -> Caption {
        Caption {
            id: "c".into(),
            start_ms,
            end_ms,
            words,
            speaker_id: None,
            style_id: None,
            notes: None,
            ai_generated: true,
            last_edited_at: 0,
            track_id: None,
        }
    }

    fn w(text: &str, start_ms: i64, end_ms: i64) -> Word {
        Word::new(text, start_ms, end_ms, 95.0)
    }

    // ── Shape ──────────────────────────────────────────────────────────────

    #[test]
    fn derives_one_karaoke_word_per_caption_word() {
        let c = cap(
            1000,
            4000,
            vec![w("a", 1000, 1500), w("b", 1600, 2500), w("c", 2600, 4000)],
        );
        let k = karaoke_words(&c);
        assert_eq!(k.len(), 3);
        assert_eq!(
            k.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn spans_are_contiguous_and_cover_the_whole_caption() {
        let c = cap(
            1000,
            4000,
            vec![w("a", 1000, 1500), w("b", 1600, 2500), w("c", 2600, 3100)],
        );
        let k = karaoke_words(&c);
        assert_eq!(k[0].start_ms, 1000, "first span anchors at caption start");
        assert_eq!(k[2].end_ms, 4000, "last span anchors at caption end");
        for pair in k.windows(2) {
            assert_eq!(
                pair[0].end_ms, pair[1].start_ms,
                "no gap and no overlap between consecutive spans"
            );
        }
    }

    #[test]
    fn gap_after_a_word_is_absorbed_by_that_word() {
        // "a" ends at 1500 but "b" only starts at 3000 — the 1.5 s breath stays
        // lit on "a" (karaoke convention), it is not dead time.
        let c = cap(1000, 4000, vec![w("a", 1000, 1500), w("b", 3000, 4000)]);
        let k = karaoke_words(&c);
        assert_eq!(k[0].end_ms, 3000);
        assert_eq!(k[0].duration_cs, 200);
        assert_eq!(k[1].duration_cs, 100);
    }

    // ── The cumulative-rounding invariant (the reason this module exists) ───

    /// `\k` durations are cumulative: libass sums them from the Dialogue Start.
    /// If they do not add up to the Dialogue span, every word after the first
    /// rounding error drifts. This is the load-bearing invariant.
    fn assert_ladder_closes(c: &Caption) {
        let k = karaoke_words(c);
        let sum: i64 = k.iter().map(|x| x.duration_cs as i64).sum();
        let span_cs = to_cs(c.end_ms.max(c.start_ms)) - to_cs(c.start_ms);
        assert_eq!(
            sum, span_cs,
            "sum(duration_cs) must equal the Dialogue span in cs exactly ({c:?})"
        );
        // And therefore within one centisecond of the true millisecond span.
        let span_ms = (c.end_ms.max(c.start_ms) - c.start_ms.max(0)).max(0);
        if c.start_ms >= 0 {
            assert!(
                (sum * 10 - span_ms).abs() < 10,
                "sum(duration_cs)*10 must be within 1 cs of the caption span \
                 (sum={sum} cs, span={span_ms} ms)"
            );
        }
        assert!(
            k.iter().all(|x| x.duration_cs >= 0),
            "no negative \\k duration"
        );
    }

    #[test]
    fn duration_ladder_closes_on_adversarial_table() {
        let table: Vec<Caption> = vec![
            // Sub-centisecond word boundaries — the classic drift generator:
            // each word's own span rounds to 0 or 1 cs, but the ladder must
            // still land exactly on the caption end.
            cap(
                0,
                1_000,
                (0..7)
                    .map(|i| w("x", i * 143, i * 143 + 143))
                    .collect::<Vec<_>>(),
            ),
            // Caption start not on a centisecond boundary.
            cap(
                1_007,
                4_003,
                vec![w("a", 1_007, 2_001), w("b", 2_001, 4_003)],
            ),
            // Single word.
            cap(500, 900, vec![w("only", 500, 900)]),
            // Zero-length caption.
            cap(1_234, 1_234, vec![w("a", 1_234, 1_234)]),
            // Inverted caption (end before start).
            cap(5_000, 1_000, vec![w("a", 5_000, 6_000)]),
            // Words entirely outside the caption bounds.
            cap(
                1_000,
                2_000,
                vec![w("early", 0, 10), w("late", 9_000, 9_500)],
            ),
            // Zero/negative word spans.
            cap(
                0,
                3_000,
                vec![w("a", 500, 500), w("b", 2_000, 1_000), w("c", 2_500, 3_000)],
            ),
            // Out-of-order words.
            cap(0, 2_000, vec![w("b", 1_500, 2_000), w("a", 200, 900)]),
            // Empty word list → single fallback span.
            cap(1_000, 2_500, vec![]),
            // Negative caption start (clamped like fmt_ass_time does).
            cap(-500, 1_500, vec![w("a", -500, 0), w("b", 0, 1_500)]),
            // Nine-figure timeline position (long service recording).
            cap(
                7_199_993,
                7_203_337,
                vec![w("a", 7_199_993, 7_201_111), w("b", 7_201_111, 7_203_337)],
            ),
        ];
        for c in &table {
            assert_ladder_closes(c);
        }
    }

    #[test]
    fn duration_ladder_closes_under_randomised_timings() {
        // Fixed-seed xorshift64* sweep — same convention as the timecode
        // round-trip property test in `services::export`.
        let mut state: u64 = 0x5EED_1234_ABCD_0F01;
        let mut next = || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };
        for _ in 0..500 {
            let start = (next() % 600_000) as i64;
            let span = (next() % 12_000) as i64;
            let n = (next() % 9) as usize + 1;
            let words: Vec<Word> = (0..n)
                .map(|_| {
                    // Deliberately sloppy: starts anywhere near the caption,
                    // ends possibly before their own start.
                    let ws = start + (next() % (span as u64 + 1)) as i64 - 200;
                    let we = ws + (next() % 900) as i64 - 300;
                    w("w", ws, we)
                })
                .collect();
            assert_ladder_closes(&cap(start, start + span, words));
        }
    }

    // ── Degenerate inputs ──────────────────────────────────────────────────

    #[test]
    fn empty_word_list_falls_back_to_one_span_covering_the_caption() {
        let c = cap(1_000, 2_500, vec![]);
        let k = karaoke_words(&c);
        assert_eq!(k.len(), 1);
        assert_eq!(k[0].start_ms, 1_000);
        assert_eq!(k[0].end_ms, 2_500);
        assert_eq!(k[0].duration_cs, 150);
        assert_eq!(k[0].text, "");
    }

    #[test]
    fn words_outside_the_caption_are_clamped_into_it() {
        let c = cap(
            1_000,
            2_000,
            vec![w("early", -5_000, 0), w("late", 9_000, 9_500)],
        );
        let k = karaoke_words(&c);
        assert!(
            k.iter().all(|x| x.start_ms >= 1_000 && x.end_ms <= 2_000),
            "every span stays inside the caption: {k:?}"
        );
        assert_eq!(k[0].start_ms, 1_000);
        assert_eq!(k[1].end_ms, 2_000);
    }

    #[test]
    fn zero_and_negative_span_words_never_steal_time() {
        // "b" ends before it starts; "a" is zero-length. Neither may produce a
        // negative duration or push a neighbour backwards.
        let c = cap(
            0,
            3_000,
            vec![w("a", 500, 500), w("b", 2_000, 1_000), w("c", 2_500, 3_000)],
        );
        let k = karaoke_words(&c);
        assert!(k.iter().all(|x| x.duration_cs >= 0));
        for pair in k.windows(2) {
            assert!(pair[0].end_ms <= pair[1].end_ms, "spans stay monotonic");
        }
        assert_eq!(k.iter().map(|x| x.duration_cs as i64).sum::<i64>(), 300);
    }

    #[test]
    fn out_of_order_words_stay_monotonic() {
        // Word order in `Caption::words` is the RENDER order — it is what the
        // viewer reads left-to-right — so the ladder follows the list, not the
        // timestamps. A word list whose timestamps run backwards still yields a
        // valid contiguous partition (degenerate, but never negative and never
        // out of order); garbage timings can't produce a corrupt ASS line.
        let c = cap(0, 2_000, vec![w("b", 1_500, 2_000), w("a", 200, 900)]);
        let k = karaoke_words(&c);
        assert_eq!(
            k[0].start_ms, 0,
            "first span still anchors at caption start"
        );
        assert_eq!(k[0].end_ms, k[1].start_ms, "contiguous");
        assert_eq!(k[1].end_ms, 2_000, "last span still anchors at caption end");
        assert!(k.iter().all(|x| x.duration_cs >= 0));
        assert_eq!(k.iter().map(|x| x.duration_cs as i64).sum::<i64>(), 200);
    }

    #[test]
    fn inverted_caption_collapses_to_zero_duration() {
        let c = cap(5_000, 1_000, vec![w("a", 5_000, 6_000)]);
        let k = karaoke_words(&c);
        assert_eq!(k.len(), 1);
        assert_eq!(k[0].duration_cs, 0);
        assert_eq!(k[0].start_ms, 5_000);
        assert_eq!(k[0].end_ms, 5_000);
    }

    // ── Word state ─────────────────────────────────────────────────────────

    #[test]
    fn word_state_boundaries_are_half_open() {
        let k = KaraokeWord {
            text: "a".into(),
            start_ms: 1_000,
            end_ms: 2_000,
            duration_cs: 100,
            confidence: 90.0,
        };
        assert_eq!(word_state_at(&k, 999), WordState::Pending);
        assert_eq!(word_state_at(&k, 1_000), WordState::Active, "start is IN");
        assert_eq!(word_state_at(&k, 1_999), WordState::Active);
        assert_eq!(word_state_at(&k, 2_000), WordState::Done, "end is OUT");
    }

    #[test]
    fn exactly_one_word_is_active_inside_the_caption() {
        let c = cap(
            1_000,
            4_000,
            vec![
                w("a", 1_000, 1_500),
                w("b", 1_600, 2_500),
                w("c", 2_600, 4_000),
            ],
        );
        let k = karaoke_words(&c);
        for t in (1_000..4_000).step_by(7) {
            let actives = word_states_at(&k, t)
                .into_iter()
                .filter(|s| *s == WordState::Active)
                .count();
            assert_eq!(actives, 1, "exactly one active word at t={t}");
        }
    }

    #[test]
    fn states_are_ordered_done_then_active_then_pending() {
        // The canvas relies on this: a Done word can never follow a Pending one.
        let c = cap(
            0,
            5_000,
            vec![
                w("a", 0, 800),
                w("b", 900, 1_800),
                w("c", 1_800, 1_800), // zero-length: skipped, never Active
                w("d", 2_400, 5_000),
            ],
        );
        let k = karaoke_words(&c);
        for t in (-500..6_000).step_by(11) {
            let states = word_states_at(&k, t);
            let rank = |s: &WordState| match s {
                WordState::Done => 0,
                WordState::Active => 1,
                WordState::Pending => 2,
            };
            for pair in states.windows(2) {
                assert!(
                    rank(&pair[0]) <= rank(&pair[1]),
                    "state order broke at t={t}: {states:?}"
                );
            }
            assert!(
                states.iter().filter(|s| **s == WordState::Active).count() <= 1,
                "at most one active word at t={t}"
            );
        }
    }

    #[test]
    fn all_pending_before_and_all_done_after_the_caption() {
        let c = cap(
            1_000,
            2_000,
            vec![w("a", 1_000, 1_500), w("b", 1_500, 2_000)],
        );
        let k = karaoke_words(&c);
        assert!(word_states_at(&k, 0)
            .iter()
            .all(|s| *s == WordState::Pending));
        assert!(word_states_at(&k, 99_999)
            .iter()
            .all(|s| *s == WordState::Done));
    }

    #[test]
    fn active_index_tracks_the_playhead() {
        let c = cap(
            0,
            3_000,
            vec![w("a", 0, 1_000), w("b", 1_000, 2_000), w("c", 2_000, 3_000)],
        );
        let k = karaoke_words(&c);
        assert_eq!(active_index_at(&k, 0), Some(0));
        assert_eq!(active_index_at(&k, 1_500), Some(1));
        assert_eq!(active_index_at(&k, 2_999), Some(2));
        assert_eq!(active_index_at(&k, 3_000), None);
        assert_eq!(active_index_at(&k, -1), None);
    }

    // ── Confidence flags ───────────────────────────────────────────────────

    #[test]
    fn uncertain_flags_align_with_karaoke_words() {
        let mut words = vec![
            Word::new("sure", 0, 500, 95.0),
            Word::new("shaky", 500, 1_000, 40.0),
            Word::new("locked", 1_000, 1_500, 10.0),
            Word::new("edited", 1_500, 2_000, 5.0),
        ];
        words[2].locked = true;
        words[3].edited = true;
        let c = cap(0, 2_000, words);
        let k = karaoke_words(&c);
        let flags = uncertain_flags(&c, 70.0);
        assert_eq!(flags.len(), k.len(), "index-aligned with karaoke_words");
        assert_eq!(flags, vec![false, true, false, false]);
    }

    #[test]
    fn uncertain_flags_length_matches_for_the_empty_fallback() {
        let c = cap(0, 1_000, vec![]);
        assert_eq!(uncertain_flags(&c, 70.0).len(), karaoke_words(&c).len());
        assert_eq!(uncertain_flags(&c, 70.0), vec![false]);
    }

    #[test]
    fn threshold_is_exclusive_at_the_boundary() {
        let c = cap(0, 1_000, vec![Word::new("edge", 0, 1_000, 70.0)]);
        assert_eq!(uncertain_flags(&c, 70.0), vec![false], "70 is NOT below 70");
        assert_eq!(uncertain_flags(&c, 70.1), vec![true]);
    }

    // ── Options ────────────────────────────────────────────────────────────

    #[test]
    fn karaoke_is_off_by_default() {
        let o = KaraokeOptions::default();
        assert!(!o.enabled, "karaoke must never turn itself on");
        assert!(!o.confidence_tint, "tinting must never turn itself on");
        assert_eq!(o.style, KaraokeStyle::Highlight);
        assert_eq!(o.confidence_threshold, 70.0);
    }

    #[test]
    fn style_tags_are_the_ass_names() {
        assert_eq!(KaraokeStyle::Highlight.tag(), "k");
        assert_eq!(KaraokeStyle::Sweep.tag(), "kf");
    }

    #[test]
    fn options_deserialize_from_kebab_case_style() {
        let o: KaraokeOptions = serde_json::from_str(
            r##"{"enabled":true,"style":"sweep","pending_color":"#111111",
                "confidence_tint":true,"confidence_threshold":50.0,
                "low_confidence_color":"#FF0000"}"##,
        )
        .expect("kebab-case style round-trips");
        assert_eq!(o.style, KaraokeStyle::Sweep);
        assert!(o.enabled);
    }

    #[test]
    fn to_cs_matches_ass_timestamp_truncation() {
        // Must agree with `export::fmt_ass_time`, which truncates to cs and
        // clamps negatives to zero.
        assert_eq!(to_cs(0), 0);
        assert_eq!(to_cs(9), 0);
        assert_eq!(to_cs(10), 1);
        assert_eq!(to_cs(1_999), 199);
        assert_eq!(to_cs(-5_000), 0);
    }
}
