/**
 * Executable mirror parity — the TypeScript half (E8 hardening round).
 *
 * Three E1–E6 seams are implemented twice on purpose: once in Rust (where the
 * export lives) and once in TypeScript (where the preview lives).
 *
 * | seam      | Rust (truth)                         | TypeScript (mirror, here)                   |
 * | --------- | ------------------------------------ | ------------------------------------------- |
 * | karaoke   | `src-tauri/src/services/karaoke.rs`  | `src/features/timeline/karaoke.ts`          |
 * | tile grid | `src-tauri/src/services/tiles.rs`    | `src/features/timeline/filmstrip.ts`        |
 * | effects   | `src-tauri/src/services/effects.rs`  | `src/features/timeline/effects/registry.ts` |
 *
 * Each side already has good unit tests, which is exactly why they can drift:
 * two halves that pass their own tables and disagree on an input neither table
 * contains is the seam-bug shape (see the suite's `reference-seam-bugs` note).
 * The comment "keep these in lockstep" is not a test.
 *
 * `src-tauri/tests/mirror_fixture_parity.rs` runs the RUST side over an
 * adversarial table plus a fixed-seed sweep and freezes inputs and outputs into
 * `__fixtures__/mirror-parity.json`. This file replays the identical inputs
 * through the TypeScript mirrors and demands byte-identical answers. A Rust
 * change that isn't mirrored fails on the Rust side (stale fixture); a
 * TypeScript change that isn't mirrored fails here.
 *
 * Why it matters, concretely: ASS `\k` durations are CUMULATIVE, so one
 * centisecond of disagreement on word 3 shifts every word after it — the
 * karaoke preview and the burned-in caption would slide apart over a line, and
 * "what you saw is what you get" is the flagship promise.
 */

import { describe, expect, it } from "vitest";

import type { Caption } from "@/lib/bindings/Caption";
import type { Effect } from "@/lib/bindings/Effect";
import type { Word } from "@/lib/bindings/Word";
import {
  karaokeWords,
  uncertainFlags,
  wordStatesAt,
  type WordState,
} from "@/features/timeline/karaoke";
import {
  TILE_BASE_SPAN_MS,
  TILE_COLS_DEFAULT,
  TILE_HEIGHT_PX,
  TILE_MAX_TIER,
  parentTile,
  tileIndexAt,
  tileKey,
  tileRangeMs,
  tileSpanMs,
  tilesForRange,
} from "@/features/timeline/filmstrip";
import {
  effectColorMatrix,
  ffmpegFragment,
} from "@/features/timeline/effects/registry";

import fixture from "./__fixtures__/mirror-parity.json";

// ── Decoders (the encodings are documented in the fixture's `encoding` block) ─

/** `text|start_ms|end_ms|confidence|locked|edited` → a full `Word`. */
function decodeWord(encoded: string): Word {
  const [text, startMs, endMs, confidence, locked, edited] = encoded.split("|");
  return {
    text,
    start_ms: Number(startMs),
    end_ms: Number(endMs),
    confidence: Number(confidence),
    edited: edited === "1",
    locked: locked === "1",
    polished: false,
    alternates: [],
  };
}

/** The inverse of the Rust `enc_word_out`, so a mismatch reads as a diff of
 *  two short strings rather than of two object dumps. */
function encodeKaraokeWord(w: {
  text: string;
  start_ms: number;
  end_ms: number;
  duration_cs: number;
  confidence: number;
}): string {
  return `${w.text}|${w.start_ms}|${w.end_ms}|${w.duration_cs}|${w.confidence}`;
}

const STATE_CHAR: Record<WordState, string> = {
  pending: "p",
  active: "a",
  done: "d",
};

function caption(c: {
  caption_start_ms: number;
  caption_end_ms: number;
  words: string[];
}): Caption {
  return {
    id: "c",
    start_ms: c.caption_start_ms,
    end_ms: c.caption_end_ms,
    words: c.words.map(decodeWord),
    speaker_id: null,
    style_id: null,
    notes: null,
    ai_generated: true,
    last_edited_at: 0,
    track_id: null,
  };
}

// ── Karaoke ladder ───────────────────────────────────────────────────────────

describe("karaoke.ts mirrors services::karaoke", () => {
  it("has a fixture with teeth", () => {
    // Guard against the fixture silently emptying out (a regenerate from a
    // broken generator would otherwise turn this whole file green-and-useless).
    expect(fixture.karaoke.length).toBeGreaterThanOrEqual(60);
    expect(
      fixture.karaoke.some((c) => c.words.length === 0),
      "no empty-word-list case",
    ).toBe(true);
  });

  it.each(fixture.karaoke.map((c) => [c.name, c] as const))(
    "derives the same karaoke words as Rust — %s",
    (_name, c) => {
      expect(karaokeWords(caption(c)).map(encodeKaraokeWord)).toEqual(c.out);
    },
  );

  it.each(fixture.karaoke.map((c) => [c.name, c] as const))(
    "flags the same low-confidence words as Rust — %s",
    (_name, c) => {
      const flags = uncertainFlags(caption(c), c.threshold)
        .map((f) => (f ? "1" : "0"))
        .join("");
      expect(flags).toBe(c.uncertain);
    },
  );

  it.each(fixture.karaoke.map((c) => [c.name, c] as const))(
    "reports the same per-word state at every sampled instant — %s",
    (_name, c) => {
      const words = karaokeWords(caption(c));
      const actual = c.samples.map((sample) => {
        const tMs = Number(sample.split("|")[0]);
        const states = wordStatesAt(words, tMs);
        // Rust's `active_index_at` is `position(state == Active)`; the mirror
        // derives it from the same predicate rather than duplicating it.
        const active = states.indexOf("active");
        const encoded = states.map((s) => STATE_CHAR[s]).join("");
        return `${tMs}|${encoded}|${active < 0 ? "-" : active}`;
      });
      expect(actual).toEqual(c.samples);
    },
  );

  it("the ladder closes on the Dialogue span in every fixture case", () => {
    // The invariant the whole module exists for, asserted against the FROZEN
    // Rust output rather than recomputed: `sum(duration_cs)` must be exactly
    // the caption span in centiseconds, or libass drifts word by word.
    const toCs = (ms: number) => Math.floor(Math.max(0, ms) / 10);
    for (const c of fixture.karaoke) {
      const sum = c.out.reduce((acc, w) => acc + Number(w.split("|")[3]), 0);
      const spanCs =
        toCs(Math.max(c.caption_end_ms, c.caption_start_ms)) -
        toCs(c.caption_start_ms);
      expect(sum, `${c.name}: sum(duration_cs) != Dialogue span`).toBe(spanCs);
    }
  });
});

// ── Tile grid ────────────────────────────────────────────────────────────────

describe("filmstrip.ts mirrors services::tiles", () => {
  it("agrees on the grid constants", () => {
    expect(TILE_BASE_SPAN_MS).toBe(fixture.tiles.base_span_ms);
    expect(TILE_MAX_TIER).toBe(fixture.tiles.max_tier);
    expect(TILE_COLS_DEFAULT).toBe(fixture.tiles.cols_default);
    expect(TILE_HEIGHT_PX).toBe(fixture.tiles.height_px);
  });

  it("computes the same span, key, index, range and parent for every case", () => {
    for (const c of fixture.tiles.cases) {
      const where = `tier=${c.tier} range=[${c.start_ms},${c.end_ms})`;
      expect(tileSpanMs(c.tier), `${where} span`).toBe(c.span_ms);
      expect(tileIndexAt(c.start_ms, c.tier), `${where} index`).toBe(
        c.index_at_start,
      );
      expect(tileKey(c.tier, c.index_at_start), `${where} key`).toBe(c.key);
      expect(tileRangeMs(c.tier, c.index_at_start), `${where} range`).toEqual(
        c.range_at_index,
      );
      expect(parentTile(c.tier, c.index_at_start), `${where} parent`).toBe(
        c.parent,
      );
    }
  });

  it("covers every range with exactly the tiles Rust would address", () => {
    for (const c of fixture.tiles.cases) {
      const actual = tilesForRange(c.start_ms, c.end_ms, c.tier).map(
        (t) => `${t.tier}|${t.index}|${t.startMs}|${t.endMs}|${t.key}`,
      );
      expect(
        actual,
        `tier=${c.tier} range=[${c.start_ms},${c.end_ms})`,
      ).toEqual(c.covering);
    }
  });
});

// ── Effect registry ──────────────────────────────────────────────────────────

describe("effects/registry.ts mirrors services::effects", () => {
  const effectOf = (c: (typeof fixture.effects)[number]): Effect => ({
    id: `fx-${c.kind}`,
    kind: c.kind,
    params: c.params,
    enabled: c.enabled,
  });

  it("lowers every effect to the identical ffmpeg fragment", () => {
    for (const c of fixture.effects) {
      expect(
        ffmpegFragment(effectOf(c)),
        `${c.kind} ${JSON.stringify(c.params)} enabled=${c.enabled}`,
      ).toBe(c.fragment ?? null);
    }
  });

  it("agrees with the GPU preview about which effects do nothing", () => {
    // The other half of the same seam, and the one the fixture cannot see:
    // `effectColorMatrix` decides what the Pixi preview DRAWS while
    // `filter_fragment` decides what ffmpeg RENDERS. If one of them treats an
    // input as neutral and the other doesn't, the preview shows a grade the
    // export drops (or the reverse) — and both sides' own tests stay green.
    for (const c of fixture.effects) {
      const matrix = effectColorMatrix(effectOf(c));
      expect(
        matrix === null,
        `${c.kind} ${JSON.stringify(c.params)} enabled=${c.enabled}: ` +
          `preview ${matrix === null ? "draws nothing" : "draws a grade"} but ` +
          `export emits ${c.fragment ?? "nothing"}`,
      ).toBe(c.fragment === null);
      // Pixi's `ColorMatrixFilter` takes exactly 5×4 numbers; a short array is
      // a silent no-op there rather than a type error.
      if (matrix) expect(matrix).toHaveLength(20);
    }
  });
});
