/**
 * Karaoke word timing — TS mirror of `src-tauri/src/services/karaoke.rs`,
 * which IS the source of truth (E4a — docs/OSS-INTEGRATION-PROGRAM.md §E4).
 * Keep the two in lockstep: any change to the span/rounding derivation
 * there must be ported here too, and vice versa. This is what buys
 * preview↔export parity — the canvas overlay and the burned-in ASS `\k`
 * tags must agree on which word is lit at any instant, or the flagship
 * promise ("what you saw in preview is what you get") quietly breaks.
 *
 * `KaraokeWord` / `WordState` are ts-rs-generated bindings
 * (`src/lib/bindings/`) — re-exported here rather than redeclared, so a
 * shape change on the Rust side is a compile error here, not a silent drift.
 *
 * ── Span derivation (ported from `karaoke.rs`'s `spans`/`karaoke_words`) ──
 * ASR word timings have gaps (breaths, silences) and — rarely — overlaps or
 * zero-length spans. Karaoke needs a partition: contiguous spans whose union
 * is exactly the caption's own span. We:
 *   - anchor the first span at the caption start and the last at the
 *     caption end,
 *   - cut at each word's own `start_ms` (clamped into the caption and
 *     forced non-decreasing) — this extends every word FORWARD across the
 *     gap that follows it, the standard karaoke convention: the current
 *     word stays lit through a pause until the next one begins,
 *   - fall back to a single span covering the whole caption when there are
 *     no words at all.
 * Then `\k`/`\kf` durations are read off a CUMULATIVE centisecond ladder
 * (`toCs` at each cut point, monotonic, pinned to the caption's own end) —
 * NOT `round(word_span_ms / 10)` per word — because ASS accumulates `\k`
 * durations from the Dialogue `Start`. Independent per-word rounding would
 * drift the whole line out of sync with the audio by the last word.
 */

import type { Caption } from "@/lib/bindings/Caption";
import type { KaraokeWord } from "@/lib/bindings/KaraokeWord";
import type { WordState } from "@/lib/bindings/WordState";

export type { KaraokeWord, WordState };

/** Internal pre-duration span — mirrors Rust's private `Span` struct. Not
 *  exported; `trusted` only exists to keep `uncertainFlags` built from the
 *  SAME partition as `karaokeWords`, so the two can never drift apart. */
interface Span {
  text: string;
  startMs: number;
  endMs: number;
  confidence: number;
  trusted: boolean;
}

/** Milliseconds → ASS centiseconds, matching EXACTLY the truncation
 *  `export::fmt_ass_time` applies to the Dialogue `Start`/`End` stamps
 *  (negatives clamp to 0 first). Anchoring the ladder on this function is
 *  what makes `sum(duration_cs)` equal the rendered Dialogue span rather
 *  than merely approximate it. */
function toCs(ms: number): number {
  return Math.floor(Math.max(0, ms) / 10);
}

/** Partition the caption into contiguous, non-overlapping spans covering
 *  exactly `[caption.start_ms, caption.end_ms]` (ported from `spans()`). */
function spans(caption: Caption): Span[] {
  const capStart = caption.start_ms;
  // An inverted caption (end < start) would otherwise produce negative
  // durations; collapse it to a zero-length span instead.
  const capEnd = Math.max(caption.end_ms, capStart);

  if (caption.words.length === 0) {
    // Fallback: one span = the whole caption (Caption::text() is "" here,
    // still a well-formed, if empty, Dialogue line).
    return [
      {
        text: "",
        startMs: capStart,
        endMs: capEnd,
        confidence: 100,
        trusted: true,
      },
    ];
  }

  const n = caption.words.length;
  // Cut points: cuts[0] = caption start, cuts[n] = caption end, cuts[i] is
  // where word i begins. Clamping into the caption handles words that sit
  // outside its bounds; the running max handles out-of-order/overlapping
  // ASR words and zero/negative-span words (they get 0 duration instead of
  // stealing time from a neighbour).
  const cuts: number[] = [capStart];
  for (let i = 1; i < n; i++) {
    const prev = cuts[cuts.length - 1];
    const clamped = Math.min(
      Math.max(caption.words[i].start_ms, capStart),
      capEnd,
    );
    cuts.push(Math.max(clamped, prev));
  }
  cuts.push(capEnd);

  return caption.words.map((w, i) => ({
    text: w.text,
    startMs: cuts[i],
    endMs: cuts[i + 1],
    confidence: w.confidence,
    trusted: w.locked || w.edited,
  }));
}

/**
 * Derive the karaoke words for a caption — mirrors `karaoke_words` exactly.
 *
 * Guarantees (see `karaoke.test.ts`, ported from the Rust test table):
 *   - spans are contiguous and cover exactly the caption span,
 *   - `sum(duration_cs)` == the Dialogue span in centiseconds, exactly,
 *   - `duration_cs >= 0` for every word,
 *   - the result is never empty (empty word lists fall back to one span).
 */
export function karaokeWords(caption: Caption): KaraokeWord[] {
  const sp = spans(caption);
  const startCs = toCs(caption.start_ms);
  const endCs = toCs(Math.max(caption.end_ms, caption.start_ms));
  const last = sp.length - 1;

  const out: KaraokeWord[] = [];
  let prevCum = 0;
  for (let i = 0; i < sp.length; i++) {
    const s = sp[i];
    // Cumulative centiseconds elapsed at the END of this word. The last
    // word is pinned to the caption end so the ladder closes exactly on the
    // Dialogue `End` stamp — no accumulated drift, ever.
    const rawCum = i === last ? endCs - startCs : toCs(s.endMs) - startCs;
    const cum = Math.max(rawCum, prevCum);
    const durationCs = cum - prevCum;
    prevCum = cum;
    out.push({
      text: s.text,
      start_ms: s.startMs,
      end_ms: s.endMs,
      duration_cs: durationCs,
      confidence: s.confidence,
    });
  }
  return out;
}

/**
 * Which karaoke words should be tinted as low-confidence, index-aligned
 * with `karaokeWords` (mirrors `uncertain_flags` — same length, same order,
 * both built from the same span partition so they cannot drift apart).
 * Locked/edited words are trusted and never flagged, matching
 * `Caption::uncertain_word_count` and the editor's confidence tiers.
 */
export function uncertainFlags(caption: Caption, threshold: number): boolean[] {
  return spans(caption).map((s) => !s.trusted && s.confidence < threshold);
}

/**
 * State of ONE word at time `tMs` (absolute project time, same clock as the
 * caption's `start_ms`/`end_ms`). Half-open on the right — `start <= t <
 * end` — so exactly one word is Active at any instant inside the caption; a
 * word's end IS the next word's start. Mirrors `word_state_at` exactly; the
 * canvas overlay MUST match this predicate — it's the visual contract
 * between preview and burn-in.
 */
export function wordStateAt(word: KaraokeWord, tMs: number): WordState {
  if (tMs < word.start_ms) return "pending";
  if (tMs < word.end_ms) return "active";
  return "done";
}

/** State of EVERY word at `tMs` — mirrors `word_states_at`. Cheap enough to
 *  call per frame (used by the preview overlay). */
export function wordStatesAt(words: KaraokeWord[], tMs: number): WordState[] {
  return words.map((w) => wordStateAt(w, tMs));
}

/**
 * Fill fraction (0–1) through a single word — drives the "sweep" style's
 * partial fill in the DOM overlay. Frontend-only: `karaoke.rs` has no
 * equivalent, because it only emits the `\kf` tag and lets libass compute
 * the actual sweep at render time. A zero-length word (its span collapsed
 * against a neighbour) reports 1 rather than dividing by zero.
 */
export function wordProgress(word: KaraokeWord, tMs: number): number {
  // Check "reached the end" FIRST: a zero-length word has start_ms ===
  // end_ms, so at exactly that instant it must read as fully filled,
  // matching `wordStateAt`'s `tMs < end_ms` → NOT done — not as unstarted.
  if (tMs >= word.end_ms) return 1;
  if (tMs <= word.start_ms) return 0;
  const span = word.end_ms - word.start_ms;
  return span <= 0 ? 1 : (tMs - word.start_ms) / span;
}

/**
 * The caption active at `tMs`, or `null` between captions. A frontend-only
 * concern — `karaoke.rs` never needs "which caption", since `write_ass`
 * iterates every caption unconditionally. Mirrors that iteration's
 * ordering: the FIRST caption (array order) whose `[start_ms, end_ms)`
 * contains `tMs` wins — overlapping captions on separate caption tracks are
 * a rare edge case this preview does not attempt to stack.
 */
export function captionAt(captions: Caption[], tMs: number): Caption | null {
  for (const c of captions) {
    if (tMs >= c.start_ms && tMs < c.end_ms) return c;
  }
  return null;
}
