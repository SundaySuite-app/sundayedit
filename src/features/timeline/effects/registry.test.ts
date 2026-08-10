import { describe, it, expect } from "vitest";

import type { Effect } from "@/lib/bindings/Effect";
import {
  CURATED_EFFECTS,
  effectColorMatrix,
  effectDef,
  effectParam,
  ffmpegFragment,
  isCurated,
  stackColorMatrix,
  stackFragments,
} from "./registry";

function fx(kind: string, params: unknown = {}, enabled = true): Effect {
  return { id: `fx-${kind}`, kind, params, enabled };
}

describe("curated effect registry", () => {
  it("offers only effects the export can render", () => {
    expect(CURATED_EFFECTS.map((d) => d.id)).toEqual([
      "brightness",
      "contrast",
      "saturation",
      "grayscale",
    ]);
  });

  it("has unique ids", () => {
    const ids = CURATED_EFFECTS.map((d) => d.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("keeps every default inside its own range", () => {
    for (const def of CURATED_EFFECTS) {
      for (const p of def.params) {
        expect(p.min).toBeLessThanOrEqual(p.default);
        expect(p.default).toBeLessThanOrEqual(p.max);
        expect(p.step).toBeGreaterThan(0);
      }
    }
  });

  it("rejects an unknown kind", () => {
    expect(isCurated("bloom")).toBe(false);
    expect(effectDef("bloom")).toBeUndefined();
  });
});

// ── ffmpeg fragments — the mirror of `effects::filter_fragment` ──────────────

describe("ffmpegFragment", () => {
  it("emits eq= for the three eq effects", () => {
    expect(ffmpegFragment(fx("brightness", { amount: 0.25 }))).toBe(
      "eq=brightness=0.25",
    );
    expect(ffmpegFragment(fx("contrast", { amount: 1.5 }))).toBe(
      "eq=contrast=1.5",
    );
    expect(ffmpegFragment(fx("saturation", { amount: 0.5 }))).toBe(
      "eq=saturation=0.5",
    );
  });

  it("emits hue=s=0 for grayscale", () => {
    expect(ffmpegFragment(fx("grayscale"))).toBe("hue=s=0");
  });

  it("keeps a negative sign", () => {
    expect(ffmpegFragment(fx("brightness", { amount: -0.4 }))).toBe(
      "eq=brightness=-0.4",
    );
  });

  it("emits nothing for disabled, unknown or neutral effects", () => {
    expect(ffmpegFragment(fx("brightness", { amount: 0.5 }, false))).toBeNull();
    expect(ffmpegFragment(fx("bloom", { radius: 4 }))).toBeNull();
    expect(ffmpegFragment(fx("brightness", { amount: 0 }))).toBeNull();
    expect(ffmpegFragment(fx("contrast", { amount: 1 }))).toBeNull();
    expect(ffmpegFragment(fx("saturation", { amount: 1 }))).toBeNull();
  });

  it("falls back to neutral for missing or non-numeric params", () => {
    expect(ffmpegFragment(fx("brightness", {}))).toBeNull();
    expect(ffmpegFragment(fx("contrast", { amount: "loud" }))).toBeNull();
    expect(ffmpegFragment(fx("contrast", { amount: NaN }))).toBeNull();
    expect(ffmpegFragment(fx("contrast", null))).toBeNull();
  });

  it("clamps out-of-range values instead of rejecting them", () => {
    expect(ffmpegFragment(fx("brightness", { amount: 9 }))).toBe(
      "eq=brightness=1",
    );
    expect(ffmpegFragment(fx("brightness", { amount: -9 }))).toBe(
      "eq=brightness=-1",
    );
    expect(ffmpegFragment(fx("saturation", { amount: -2 }))).toBe(
      "eq=saturation=0",
    );
    // A huge FINITE value is the one JSON can actually carry (Infinity/NaN
    // cannot survive a project file) — and the Rust mirror clamps it the same.
    expect(ffmpegFragment(fx("contrast", { amount: 1e308 }))).toBe(
      "eq=contrast=3",
    );
    // Non-finite is not clampable, so it falls back to neutral on both sides.
    expect(ffmpegFragment(fx("contrast", { amount: Infinity }))).toBeNull();
  });

  it("formats without float noise", () => {
    // 0.1 + 0.2 === 0.30000000000000004 — a naive template literal ships that.
    expect(ffmpegFragment(fx("brightness", { amount: 0.1 + 0.2 }))).toBe(
      "eq=brightness=0.3",
    );
  });

  it("stacks fragments in order, skipping the silent ones", () => {
    expect(
      stackFragments([
        fx("brightness", { amount: 0.2 }),
        fx("bloom"),
        fx("saturation", { amount: 1 }),
        fx("grayscale"),
      ]),
    ).toEqual(["eq=brightness=0.2", "hue=s=0"]);
  });

  it("lowers every curated effect to a fragment when pushed off neutral", () => {
    // The seam-bug shape: a registry entry added but never lowered.
    for (const def of CURATED_EFFECTS) {
      const p = def.params[0];
      const e = fx(def.id, p ? { [p.name]: p.max } : {});
      expect(ffmpegFragment(e), `${def.id} lowers to nothing`).not.toBeNull();
    }
  });
});

describe("effectParam", () => {
  it("clamps and defaults", () => {
    const p = effectDef("contrast")!.params[0];
    expect(effectParam(fx("contrast", { amount: 2 }), p)).toBe(2);
    expect(effectParam(fx("contrast", { amount: 99 }), p)).toBe(3);
    expect(effectParam(fx("contrast", {}), p)).toBe(1);
  });
});

// ── Pixi colour matrices ─────────────────────────────────────────────────────

/** Apply a 5×4 colour matrix to an RGB triple, the way the shader does. */
function apply(m: number[], rgb: [number, number, number]): number[] {
  const [r, g, b] = rgb;
  return [0, 1, 2].map(
    (row) =>
      m[row * 5] * r + m[row * 5 + 1] * g + m[row * 5 + 2] * b + m[row * 5 + 4],
  );
}

describe("effectColorMatrix", () => {
  it("skips exactly what ffmpegFragment skips", () => {
    // The neutrality rule must be ONE rule, or the preview shows a change the
    // export does not make (or the reverse) — the classic seam bug.
    const cases: Effect[] = [
      fx("brightness", { amount: 0 }),
      fx("contrast", { amount: 1 }),
      fx("saturation", { amount: 1 }),
      fx("brightness", { amount: 0.5 }, false),
      fx("bloom"),
      fx("brightness", { amount: 0.5 }),
      fx("grayscale"),
      fx("contrast", { amount: 2 }),
    ];
    for (const e of cases) {
      expect(
        effectColorMatrix(e) === null,
        `${e.kind} ${JSON.stringify(e.params)} enabled=${e.enabled}`,
      ).toBe(ffmpegFragment(e) === null);
    }
  });

  it("brightness is ADDITIVE, matching vf_eq (not Pixi's multiply helper)", () => {
    const m = effectColorMatrix(fx("brightness", { amount: 0.2 }))!;
    expect(apply(m, [0.5, 0.5, 0.5])).toEqual([0.7, 0.7, 0.7]);
    // A multiplicative reading would give 0.1 here, not 0.3.
    expect(apply(m, [0.1, 0.1, 0.1])[0]).toBeCloseTo(0.3, 10);
  });

  it("contrast pivots around mid grey, matching vf_eq's LUT formula", () => {
    const m = effectColorMatrix(fx("contrast", { amount: 2 }))!;
    // v = 2*(v - 0.5) + 0.5
    expect(apply(m, [0.5, 0.5, 0.5])[0]).toBeCloseTo(0.5, 10);
    expect(apply(m, [0.75, 0.75, 0.75])[0]).toBeCloseTo(1.0, 10);
    expect(apply(m, [0.25, 0.25, 0.25])[0]).toBeCloseTo(0.0, 10);
  });

  it("saturation preserves luma", () => {
    const m = effectColorMatrix(fx("saturation", { amount: 2 }))!;
    const rgb: [number, number, number] = [0.2, 0.6, 0.4];
    const luma = (c: number[]) => 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
    expect(luma(apply(m, rgb))).toBeCloseTo(luma(rgb), 10);
  });

  it("grayscale collapses every channel onto the luma", () => {
    const m = effectColorMatrix(fx("grayscale"))!;
    const out = apply(m, [0.2, 0.6, 0.4]);
    const expected = 0.299 * 0.2 + 0.587 * 0.6 + 0.114 * 0.4;
    expect(out[0]).toBeCloseTo(expected, 10);
    expect(out[1]).toBeCloseTo(expected, 10);
    expect(out[2]).toBeCloseTo(expected, 10);
  });

  it("clamps like the export does", () => {
    // Same clamp on both sides, so an over-range project file previews as it
    // exports rather than blowing the shader out.
    const clamped = effectColorMatrix(fx("brightness", { amount: 9 }))!;
    const atMax = effectColorMatrix(fx("brightness", { amount: 1 }))!;
    expect(clamped).toEqual(atMax);
  });
});

describe("stackColorMatrix", () => {
  it("is null when nothing contributes", () => {
    expect(stackColorMatrix([])).toBeNull();
    expect(
      stackColorMatrix([fx("bloom"), fx("contrast", { amount: 1 })]),
    ).toBeNull();
  });

  it("returns the single matrix untouched for a one-effect stack", () => {
    const one = fx("brightness", { amount: 0.3 });
    expect(stackColorMatrix([one])).toEqual(effectColorMatrix(one));
  });

  it("composes in stack order", () => {
    // brightness +0.2 then contrast ×2 → (v + 0.2 - 0.5)*2 + 0.5
    const m = stackColorMatrix([
      fx("brightness", { amount: 0.2 }),
      fx("contrast", { amount: 2 }),
    ])!;
    expect(apply(m, [0.5, 0.5, 0.5])[0]).toBeCloseTo(0.9, 10);

    // The reverse order is genuinely different — (v - 0.5)*2 + 0.5 + 0.2
    const rev = stackColorMatrix([
      fx("contrast", { amount: 2 }),
      fx("brightness", { amount: 0.2 }),
    ])!;
    expect(apply(rev, [0.5, 0.5, 0.5])[0]).toBeCloseTo(0.7, 10);
  });

  it("skips the effects the export skips", () => {
    const withNoise = stackColorMatrix([
      fx("bloom"),
      fx("grayscale"),
      fx("saturation", { amount: 1 }),
      fx("contrast", { amount: 2 }, false),
    ]);
    expect(withNoise).toEqual(effectColorMatrix(fx("grayscale")));
  });
});
