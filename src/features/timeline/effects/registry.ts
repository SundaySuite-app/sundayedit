/**
 * Curated clip effects — the TypeScript half of the preview↔export parity
 * contract (programme stage E6, docs/OSS-INTEGRATION-PROGRAM.md; ADR-013).
 *
 * The architecture invariant is that **ffmpeg `filter_complex` is the export
 * truth** (ADR-009). A GPU preview that can show effects the export cannot
 * render breaks the product promise "what you export matches what you saw", so
 * we did NOT adopt `@clypra-studio/engine`'s 233-entry catalogue. Instead this
 * file is a deliberately small registry of effects BOTH sides can produce:
 *
 * | id           | params                    | ffmpeg              | Pixi                 |
 * | ------------ | ------------------------- | ------------------- | -------------------- |
 * | `brightness` | `amount` −1…1 (default 0) | `eq=brightness=<a>` | colour-matrix offset |
 * | `contrast`   | `amount` 0…3 (default 1)  | `eq=contrast=<a>`   | colour-matrix scale  |
 * | `saturation` | `amount` 0…3 (default 1)  | `eq=saturation=<a>` | luma-preserving mix  |
 * | `grayscale`  | —                         | `hue=s=0`           | saturation 0         |
 *
 * The Rust mirror is `src-tauri/src/services/effects.rs`, and the two are
 * pinned against each other by `src-tauri/tests/effects_registry_parity.rs`,
 * which parses the literal below out of THIS file — so keep `CURATED_EFFECTS`
 * a flat array of object literals with plain numeric fields.
 *
 * ── How exact is the parity? ────────────────────────────────────────────────
 * The **ffmpeg fragment is the specification**; the Pixi colour matrices are a
 * faithful RGB approximation of it, not a pixel-exact reimplementation.
 * `vf_eq` works in YUV — it applies `v = contrast*(v−0.5) + 0.5 + brightness`
 * to the LUMA plane and scales the chroma planes around 128 for saturation
 * (libavfilter/vf_eq.c) — while a WebGL colour matrix works on RGB. The
 * matrices below use the same formulas with Rec.601 luma weights, which is
 * where the two models agree most closely, and the preview is documented (here
 * and in ADR-013) as an approximation. Anything that must be exact is answered
 * by the preview-render proxy, which runs the real ffmpeg graph (ADR-009 §3).
 */

import type { Effect } from "@/lib/bindings/Effect";
import type { TKey } from "@/lib/i18n";

/** One tunable parameter of a curated effect. */
export interface EffectParamDef {
  /** Key inside `Effect.params`. */
  name: string;
  min: number;
  max: number;
  /** Slider granularity (UI only — not part of the Rust mirror). */
  step: number;
  /** The value at which the effect is a no-op (and emits no filter). */
  default: number;
}

export interface EffectDef {
  id: string;
  /** i18n key for the effect's display name. */
  labelKey: TKey;
  params: readonly EffectParamDef[];
}

/**
 * Every effect the UI may offer and the export can render. The inspector
 * renders exactly this list — an effect that is not here is not selectable,
 * which is what stops the preview promising something ffmpeg cannot deliver.
 */
export const CURATED_EFFECTS: readonly EffectDef[] = [
  {
    id: "brightness",
    labelKey: "effectBrightness",
    params: [{ name: "amount", min: -1, max: 1, step: 0.01, default: 0 }],
  },
  {
    id: "contrast",
    labelKey: "effectContrast",
    params: [{ name: "amount", min: 0, max: 3, step: 0.01, default: 1 }],
  },
  {
    id: "saturation",
    labelKey: "effectSaturation",
    params: [{ name: "amount", min: 0, max: 3, step: 0.01, default: 1 }],
  },
  {
    id: "grayscale",
    labelKey: "effectGrayscale",
    params: [],
  },
];

/** The curated definition for `kind`, or undefined for anything we don't render. */
export function effectDef(kind: string): EffectDef | undefined {
  return CURATED_EFFECTS.find((d) => d.id === kind);
}

/** Is this effect kind one the UI may offer and the export can render? */
export function isCurated(kind: string): boolean {
  return effectDef(kind) !== undefined;
}

/**
 * Read one parameter out of an effect's opaque JSON bag, clamped to its
 * declared range. Missing / non-numeric / non-finite falls back to the neutral
 * default — mirrors `effects::param` in Rust.
 */
export function effectParam(effect: Effect, def: EffectParamDef): number {
  const bag = effect.params as Record<string, unknown> | null | undefined;
  const raw = bag && typeof bag === "object" ? bag[def.name] : undefined;
  const n = typeof raw === "number" && Number.isFinite(raw) ? raw : def.default;
  return Math.min(def.max, Math.max(def.min, n));
}

/**
 * Format a parameter for an ffmpeg filter argument — the exact mirror of
 * `effects::fmt` in Rust (fixed notation, ≤4 decimals, no trailing zeros,
 * never `-0`). `toFixed` is locale-independent, unlike `toLocaleString`.
 */
function fmt(v: number): string {
  let s = v.toFixed(4);
  if (s.includes(".")) s = s.replace(/0+$/, "").replace(/\.$/, "");
  if (s === "-0") s = "0";
  return s;
}

/**
 * The ffmpeg filter fragment for ONE effect, or null when it contributes
 * nothing: disabled, an unknown kind, or every parameter still neutral.
 *
 * Byte-identical to `effects::filter_fragment` in Rust for the same input —
 * that equality is what `effects_registry_parity.rs` and
 * `registry.test.ts` between them keep true. The frontend does not build the
 * export command (Rust does); this exists so the UI can SHOW what a clip will
 * lower to, and so the parity can be tested from both sides.
 */
export function ffmpegFragment(effect: Effect): string | null {
  if (!effect.enabled) return null;
  const def = effectDef(effect.kind);
  if (!def) return null;
  switch (def.id) {
    case "brightness":
    case "contrast":
    case "saturation": {
      const p = def.params[0];
      const a = effectParam(effect, p);
      if (a === p.default) return null;
      return `eq=${def.id}=${fmt(a)}`;
    }
    case "grayscale":
      return "hue=s=0";
    default:
      return null;
  }
}

// ── Pixi side: 5×4 colour matrices ───────────────────────────────────────────

/** Identity for Pixi's `ColorMatrixFilter.matrix` (row-major, 4 rows of 5). */
const IDENTITY: readonly number[] = [
  1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0,
];

/** Rec.601 luma weights — the ones `vf_eq`'s YUV model is defined against. */
const LUMA_R = 0.299;
const LUMA_G = 0.587;
const LUMA_B = 0.114;

/**
 * The colour matrix for ONE effect, or null when it contributes nothing.
 * Neutrality is decided by the SAME rule as `ffmpegFragment`, so preview and
 * export never disagree about whether an effect is doing anything at all.
 */
export function effectColorMatrix(effect: Effect): number[] | null {
  if (!effect.enabled) return null;
  const def = effectDef(effect.kind);
  if (!def) return null;

  switch (def.id) {
    case "brightness": {
      // `vf_eq`: brightness is ADDITIVE (…+ brightness), not a gain — Pixi's
      // own `ColorMatrixFilter.brightness()` multiplies, which would drift
      // from the export. Use the matrix's offset column instead.
      const b = effectParam(effect, def.params[0]);
      if (b === def.params[0].default) return null;
      return [1, 0, 0, 0, b, 0, 1, 0, 0, b, 0, 0, 1, 0, b, 0, 0, 0, 1, 0];
    }
    case "contrast": {
      // `vf_eq`: v = contrast*(v − 0.5) + 0.5  →  scale c, offset 0.5 − 0.5c.
      const c = effectParam(effect, def.params[0]);
      if (c === def.params[0].default) return null;
      const o = 0.5 - 0.5 * c;
      return [c, 0, 0, 0, o, 0, c, 0, 0, o, 0, 0, c, 0, o, 0, 0, 0, 1, 0];
    }
    case "saturation": {
      const s = effectParam(effect, def.params[0]);
      if (s === def.params[0].default) return null;
      return saturationMatrix(s);
    }
    case "grayscale":
      return saturationMatrix(0);
    default:
      return null;
  }
}

/** Luma-preserving saturation: `(1−s)·luma + s·identity`. */
function saturationMatrix(s: number): number[] {
  const i = 1 - s;
  return [
    i * LUMA_R + s,
    i * LUMA_G,
    i * LUMA_B,
    0,
    0,
    i * LUMA_R,
    i * LUMA_G + s,
    i * LUMA_B,
    0,
    0,
    i * LUMA_R,
    i * LUMA_G,
    i * LUMA_B + s,
    0,
    0,
    0,
    0,
    0,
    1,
    0,
  ];
}

/** `a` then `b`, as a single 5×4 matrix (b ∘ a). */
function multiply(a: readonly number[], b: readonly number[]): number[] {
  const out = new Array<number>(20).fill(0);
  for (let row = 0; row < 4; row++) {
    for (let col = 0; col < 5; col++) {
      let sum = 0;
      for (let k = 0; k < 4; k++) sum += b[row * 5 + k] * a[k * 5 + col];
      // The 5th column is an offset, so `b`'s own offset rides along untouched.
      if (col === 4) sum += b[row * 5 + 4];
      out[row * 5 + col] = sum;
    }
  }
  return out;
}

/**
 * The single colour matrix for a clip's whole effect stack, applied in the
 * order the user stacked them, or null when nothing contributes. Same skip
 * rules as `effect_filters` in Rust, so an unknown or neutral effect is
 * invisible on both sides.
 */
export function stackColorMatrix(effects: readonly Effect[]): number[] | null {
  let acc: number[] | null = null;
  for (const e of effects) {
    const m = effectColorMatrix(e);
    if (!m) continue;
    acc = acc === null ? m : multiply(acc, m);
  }
  return acc;
}

/** The ffmpeg fragments a clip's effect stack lowers to, in order. */
export function stackFragments(effects: readonly Effect[]): string[] {
  return effects.map(ffmpegFragment).filter((f): f is string => f !== null);
}

export { IDENTITY as IDENTITY_COLOR_MATRIX };
