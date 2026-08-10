import { describe, expect, it } from "vitest";

import type { Caption, Word } from "@/lib/bindings";
import {
  captionAt,
  karaokeWords,
  uncertainFlags,
  wordProgress,
  wordStateAt,
  wordStatesAt,
} from "./karaoke";

function w(text: string, start: number, end: number, confidence = 95): Word {
  return {
    text,
    start_ms: start,
    end_ms: end,
    confidence,
    edited: false,
    locked: false,
    polished: false,
    alternates: [],
  };
}

function caption(
  startMs: number,
  endMs: number,
  words: Word[],
  id = "c1",
): Caption {
  return {
    id,
    start_ms: startMs,
    end_ms: endMs,
    words,
    speaker_id: null,
    style_id: null,
    notes: null,
    ai_generated: true,
    last_edited_at: 0,
    track_id: null,
  };
}

describe("karaokeWords", () => {
  it("derives one karaoke word per caption word, same order", () => {
    const c = caption(1000, 4000, [
      w("a", 1000, 1500),
      w("b", 1600, 2500),
      w("c", 2600, 4000),
    ]);
    const k = karaokeWords(c);
    expect(k.map((x) => x.text)).toEqual(["a", "b", "c"]);
  });

  it("spans are contiguous and cover exactly the whole caption", () => {
    const c = caption(1000, 4000, [
      w("a", 1000, 1500),
      w("b", 1600, 2500),
      w("c", 2600, 3100),
    ]);
    const k = karaokeWords(c);
    expect(k[0].start_ms).toBe(1000);
    expect(k[2].end_ms).toBe(4000);
    for (let i = 0; i < k.length - 1; i++) {
      expect(k[i].end_ms).toBe(k[i + 1].start_ms);
    }
  });

  it("a gap AFTER a word is absorbed by that word (karaoke convention: stays lit through the pause)", () => {
    // "a" ends at 1500 but "b" only starts at 3000 — the 1.5s breath stays
    // lit on "a", not dead time, and NOT charged to "b".
    const c = caption(1000, 4000, [w("a", 1000, 1500), w("b", 3000, 4000)]);
    const k = karaokeWords(c);
    expect(k[0].end_ms).toBe(3000);
    expect(k[0].duration_cs).toBe(200);
    expect(k[1].duration_cs).toBe(100);
  });

  it("falls back to a single empty-text span covering the caption when there are no words", () => {
    const c = caption(1000, 2500, []);
    const k = karaokeWords(c);
    expect(k).toHaveLength(1);
    expect(k[0]).toMatchObject({
      text: "",
      start_ms: 1000,
      end_ms: 2500,
      duration_cs: 150,
    });
  });

  it("clamps words outside the caption bounds into it", () => {
    const c = caption(1000, 2000, [
      w("early", -5000, 0),
      w("late", 9000, 9500),
    ]);
    const k = karaokeWords(c);
    expect(k.every((x) => x.start_ms >= 1000 && x.end_ms <= 2000)).toBe(true);
    expect(k[0].start_ms).toBe(1000);
    expect(k[1].end_ms).toBe(2000);
  });

  it("zero/negative-span words never steal time or go negative", () => {
    const c = caption(0, 3000, [
      w("a", 500, 500),
      w("b", 2000, 1000),
      w("c", 2500, 3000),
    ]);
    const k = karaokeWords(c);
    expect(k.every((x) => x.duration_cs >= 0)).toBe(true);
    for (let i = 0; i < k.length - 1; i++) {
      expect(k[i].end_ms).toBeLessThanOrEqual(k[i + 1].end_ms);
    }
    expect(k.reduce((sum, x) => sum + x.duration_cs, 0)).toBe(300);
  });

  it("out-of-order words stay monotonic (render order wins, not timestamp order)", () => {
    const c = caption(0, 2000, [w("b", 1500, 2000), w("a", 200, 900)]);
    const k = karaokeWords(c);
    expect(k[0].start_ms).toBe(0);
    expect(k[0].end_ms).toBe(k[1].start_ms);
    expect(k[1].end_ms).toBe(2000);
    expect(k.every((x) => x.duration_cs >= 0)).toBe(true);
  });

  it("an inverted caption (end before start) collapses to zero duration", () => {
    const c = caption(5000, 1000, [w("a", 5000, 6000)]);
    const k = karaokeWords(c);
    expect(k).toHaveLength(1);
    expect(k[0]).toMatchObject({
      duration_cs: 0,
      start_ms: 5000,
      end_ms: 5000,
    });
  });

  describe("cumulative-rounding invariant — the reason this module exists", () => {
    /** `\k` durations are cumulative: libass sums them from the Dialogue
     *  Start. If they don't add up to the Dialogue span, every word after
     *  the first rounding error drifts. */
    function assertLadderCloses(c: Caption) {
      const k = karaokeWords(c);
      const sum = k.reduce((s, x) => s + x.duration_cs, 0);
      const capStart = c.start_ms;
      const capEnd = Math.max(c.end_ms, capStart);
      const spanCs =
        Math.floor(capEnd / 10) - Math.floor(Math.max(0, capStart) / 10);
      expect(sum).toBe(spanCs);
      expect(k.every((x) => x.duration_cs >= 0)).toBe(true);
    }

    it("closes exactly across an adversarial table (sub-cs boundaries, empty words, inverted, negative, huge timestamps)", () => {
      const table: Caption[] = [
        // 143ms words: each rounds to 0 or 1cs on its own, but the ladder must
        // still land exactly on the caption end.
        caption(
          0,
          1000,
          Array.from({ length: 7 }, (_, i) => w("x", i * 143, i * 143 + 143)),
        ),
        caption(1007, 4003, [w("a", 1007, 2001), w("b", 2001, 4003)]), // start not on a cs boundary
        caption(500, 900, [w("only", 500, 900)]),
        caption(1234, 1234, [w("a", 1234, 1234)]), // zero-length caption
        caption(1000, 2000, [w("early", 0, 10), w("late", 9000, 9500)]), // words outside bounds
        caption(0, 2000, [w("b", 1500, 2000), w("a", 200, 900)]), // out of order
        caption(1000, 2500, []), // empty fallback
        caption(7_199_993, 7_203_337, [
          w("a", 7_199_993, 7_201_111),
          w("b", 7_201_111, 7_203_337),
        ]), // long-service timestamps
      ];
      for (const c of table) assertLadderCloses(c);
    });

    it("closes exactly under a randomised sweep (fixed-seed xorshift, matches the Rust property test's convention)", () => {
      let state = 0x5eed_1234n;
      const next = () => {
        state ^= state << 13n;
        state &= 0xffff_ffff_ffff_ffffn;
        state ^= state >> 7n;
        state ^= state << 17n;
        state &= 0xffff_ffff_ffff_ffffn;
        return state;
      };
      for (let i = 0; i < 300; i++) {
        const start = Number(next() % 600_000n);
        const span = Number(next() % 12_000n);
        const n = Number(next() % 9n) + 1;
        const words: Word[] = Array.from({ length: n }, () => {
          const ws = start + Number(next() % BigInt(span + 1)) - 200;
          const we = ws + Number(next() % 900n) - 300;
          return w("w", ws, we);
        });
        assertLadderCloses(caption(start, start + span, words));
      }
    });
  });
});

describe("uncertainFlags", () => {
  it("is index-aligned with karaokeWords and exempts locked/edited words", () => {
    const words = [
      w("sure", 0, 500, 95),
      w("shaky", 500, 1000, 40),
      { ...w("locked", 1000, 1500, 10), locked: true },
      { ...w("edited", 1500, 2000, 5), edited: true },
    ];
    const c = caption(0, 2000, words);
    const k = karaokeWords(c);
    const flags = uncertainFlags(c, 70);
    expect(flags).toHaveLength(k.length);
    expect(flags).toEqual([false, true, false, false]);
  });

  it("the threshold is exclusive at the boundary — equal to threshold is NOT flagged", () => {
    const c = caption(0, 1000, [w("edge", 0, 1000, 70)]);
    expect(uncertainFlags(c, 70)).toEqual([false]);
    expect(uncertainFlags(c, 70.1)).toEqual([true]);
  });

  it("matches the empty-word fallback's single span", () => {
    const c = caption(0, 1000, []);
    expect(uncertainFlags(c, 70)).toEqual([false]);
  });
});

describe("wordStateAt / wordStatesAt", () => {
  it("boundaries are half-open: start is IN, end is OUT", () => {
    const [word] = karaokeWords(caption(1000, 2000, [w("a", 1000, 2000)]));
    expect(wordStateAt(word, 999)).toBe("pending");
    expect(wordStateAt(word, 1000)).toBe("active");
    expect(wordStateAt(word, 1999)).toBe("active");
    expect(wordStateAt(word, 2000)).toBe("done");
  });

  it("exactly one word is active at any instant inside the caption", () => {
    const c = caption(1000, 4000, [
      w("a", 1000, 1500),
      w("b", 1600, 2500),
      w("c", 2600, 4000),
    ]);
    const words = karaokeWords(c);
    for (let t = 1000; t < 4000; t += 7) {
      const actives = wordStatesAt(words, t).filter(
        (s) => s === "active",
      ).length;
      expect(actives).toBe(1);
    }
  });

  it("state order is always done, then active, then pending — never done after pending", () => {
    const c = caption(0, 5000, [
      w("a", 0, 800),
      w("b", 900, 1800),
      w("c", 1800, 1800), // zero-length: skipped, never active
      w("d", 2400, 5000),
    ]);
    const words = karaokeWords(c);
    const rank = { done: 0, active: 1, pending: 2 } as const;
    for (let t = -500; t < 6000; t += 11) {
      const states = wordStatesAt(words, t);
      for (let i = 0; i < states.length - 1; i++) {
        expect(rank[states[i]]).toBeLessThanOrEqual(rank[states[i + 1]]);
      }
      expect(states.filter((s) => s === "active").length).toBeLessThanOrEqual(
        1,
      );
    }
  });

  it("all pending before the caption, all done after it", () => {
    const words = karaokeWords(
      caption(1000, 2000, [w("a", 1000, 1500), w("b", 1500, 2000)]),
    );
    expect(wordStatesAt(words, 0).every((s) => s === "pending")).toBe(true);
    expect(wordStatesAt(words, 99_999).every((s) => s === "done")).toBe(true);
  });
});

describe("wordProgress", () => {
  const [word] = karaokeWords(caption(0, 400, [w("hi", 0, 400)]));

  it("is 0 before the word starts and 1 at/after its end", () => {
    expect(wordProgress(word, -1)).toBe(0);
    expect(wordProgress(word, 0)).toBe(0);
    expect(wordProgress(word, 400)).toBe(1);
    expect(wordProgress(word, 999)).toBe(1);
  });

  it("is linear through the middle", () => {
    expect(wordProgress(word, 100)).toBeCloseTo(0.25);
    expect(wordProgress(word, 300)).toBeCloseTo(0.75);
  });

  it("reports 1 for a zero-length word instead of dividing by zero", () => {
    const zero = {
      text: "x",
      start_ms: 100,
      end_ms: 100,
      duration_cs: 0,
      confidence: 90,
    };
    expect(wordProgress(zero, 100)).toBe(1);
  });
});

describe("captionAt", () => {
  const captions = [
    caption(0, 1000, [w("a", 0, 1000)]),
    caption(1500, 3000, [w("b", 1500, 3000)], "c2"),
  ];

  it("finds the caption containing the given time", () => {
    expect(captionAt(captions, 500)?.id).toBe("c1");
    expect(captionAt(captions, 2000)?.id).toBe("c2");
  });

  it("is half-open at each caption's end boundary", () => {
    expect(captionAt(captions, 1000)).toBeNull();
  });

  it("returns null between captions and outside the whole range", () => {
    expect(captionAt(captions, 1200)).toBeNull();
    expect(captionAt(captions, -1)).toBeNull();
    expect(captionAt(captions, 9999)).toBeNull();
  });

  it("returns the first array match when captions overlap across tracks", () => {
    const overlapping = [
      caption(0, 1000, [w("a", 0, 1000)], "c1"),
      caption(0, 1000, [w("b", 0, 1000)], "c2"),
    ];
    expect(captionAt(overlapping, 500)?.id).toBe("c1");
  });
});
