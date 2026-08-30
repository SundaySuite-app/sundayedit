import { describe, it, expect } from "vitest";

import {
  GAIN_DB_MIN,
  GAIN_DB_MAX,
  clampGainDb,
  dbToLinear,
  combinedGainDb,
  previewVolumeFor,
  applyGainDetent,
  formatDb,
} from "./audioLevels";

describe("clampGainDb", () => {
  it("passes an in-range value through unchanged", () => {
    expect(clampGainDb(-6)).toBe(-6);
    expect(clampGainDb(0)).toBe(0);
  });

  it("clamps to the documented range both ways", () => {
    expect(clampGainDb(GAIN_DB_MIN)).toBe(GAIN_DB_MIN);
    expect(clampGainDb(GAIN_DB_MAX)).toBe(GAIN_DB_MAX);
    expect(clampGainDb(-1000)).toBe(GAIN_DB_MIN);
    expect(clampGainDb(1000)).toBe(GAIN_DB_MAX);
  });

  it("maps NaN to unity, mirroring the Rust side", () => {
    expect(clampGainDb(NaN)).toBe(0);
  });

  it("clamps signed infinities to the matching bound", () => {
    expect(clampGainDb(Infinity)).toBe(GAIN_DB_MAX);
    expect(clampGainDb(-Infinity)).toBe(GAIN_DB_MIN);
  });
});

describe("dbToLinear", () => {
  it("unity at 0 dB", () => {
    expect(dbToLinear(0)).toBeCloseTo(1);
  });

  it("halves roughly every -6 dB", () => {
    expect(dbToLinear(-6)).toBeCloseTo(0.5012, 3);
  });

  it("doubles roughly every +6 dB", () => {
    expect(dbToLinear(6)).toBeCloseTo(1.9953, 3);
  });
});

describe("combinedGainDb", () => {
  it("dB-adds the two levels, not multiplies the linear amplitudes", () => {
    expect(combinedGainDb(-6, -6)).toBe(-12);
    expect(combinedGainDb(3, 3)).toBe(6);
  });

  it("clamps each side before summing, mirroring effective_gain_db()", () => {
    // A hand-edited file could carry an out-of-range value; the combined
    // level must reflect what the RENDER will actually use (the clamped
    // value), not the raw stored one.
    expect(combinedGainDb(1000, 0)).toBe(GAIN_DB_MAX);
    expect(combinedGainDb(0, -1000)).toBe(GAIN_DB_MIN);
  });

  it("defaults a missing track fader to unity", () => {
    expect(combinedGainDb(-3, 0)).toBe(-3);
  });
});

describe("previewVolumeFor", () => {
  it("is unclipped and full at 0 dB", () => {
    const { volume, clipped } = previewVolumeFor(0);
    expect(volume).toBeCloseTo(1);
    expect(clipped).toBe(false);
  });

  it("is unclipped for any negative combined gain", () => {
    expect(previewVolumeFor(-6).clipped).toBe(false);
    expect(previewVolumeFor(GAIN_DB_MIN * 2).clipped).toBe(false);
  });

  it("clips and saturates at 1 for positive combined gain", () => {
    const { volume, clipped } = previewVolumeFor(6);
    expect(clipped).toBe(true);
    expect(volume).toBe(1);
  });

  it("clips even for a combined gain beyond either single fader's own range", () => {
    // Two +12 dB faders sum to +24 dB — legal (compose.rs never caps the
    // COMBINED total, only each side individually) and definitely clipped.
    const { volume, clipped } = previewVolumeFor(GAIN_DB_MAX * 2);
    expect(clipped).toBe(true);
    expect(volume).toBe(1);
  });

  it("never returns a negative volume or a NaN for a NaN input", () => {
    const { volume, clipped } = previewVolumeFor(NaN);
    expect(volume).toBeCloseTo(1);
    expect(clipped).toBe(false);
    expect(previewVolumeFor(GAIN_DB_MIN).volume).toBeGreaterThanOrEqual(0);
  });
});

describe("applyGainDetent", () => {
  it("snaps a value close to 0 dB to exactly 0", () => {
    expect(applyGainDetent(0.1)).toBe(0);
    expect(applyGainDetent(-0.2)).toBe(0);
    expect(applyGainDetent(0)).toBe(0);
  });

  it("leaves a value clearly outside the detent window untouched", () => {
    expect(applyGainDetent(1)).toBe(1);
    expect(applyGainDetent(-3.5)).toBe(-3.5);
  });
});

describe("formatDb", () => {
  it("always signs positive values", () => {
    expect(formatDb(3)).toBe("+3.0 dB");
  });

  it("signs negative values with a minus, zero with neither", () => {
    expect(formatDb(-6)).toBe("-6.0 dB");
    expect(formatDb(0)).toBe("0.0 dB");
  });

  it("rounds to one decimal", () => {
    expect(formatDb(1.234)).toBe("+1.2 dB");
  });
});
