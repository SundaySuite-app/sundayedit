/**
 * Preview quality ladder — the tier decision and the shuttle frame stride.
 * Pure policy, so every rung and every precedence rule is pinned here rather
 * than inferred from the Timeline's behaviour.
 */
import { describe, it, expect } from "vitest";

import {
  MAX_RENDER_STRIDE,
  qualityFor,
  renderStride,
  scalePctForTier,
  shouldRenderFrame,
  tierFor,
  type PreviewActivity,
} from "./previewQuality";

const idle: PreviewActivity = { rate: 0, interacting: false, exporting: false };

describe("tierFor", () => {
  it("parks at the idle rung with a stopped transport and no interaction", () => {
    expect(tierFor(idle)).toBe("idle");
  });

  it("drops to playback while the transport rolls, forward or reverse", () => {
    expect(tierFor({ ...idle, rate: 1 })).toBe("playback");
    expect(tierFor({ ...idle, rate: -4 })).toBe("playback");
    expect(tierFor({ ...idle, rate: 0.5 })).toBe("playback");
  });

  it("drops to interaction while scrubbing or dragging — even mid-playback", () => {
    expect(tierFor({ ...idle, interacting: true })).toBe("interaction");
    // A scrub during playback is the case that most needs to feel instant, so
    // interaction must outrank playback rather than the other way round.
    expect(tierFor({ rate: 2, interacting: true, exporting: false })).toBe(
      "interaction",
    );
  });

  it("pins the export rung above everything else", () => {
    expect(tierFor({ rate: 8, interacting: true, exporting: true })).toBe(
      "export",
    );
  });

  it("treats a nonsense rate as parked rather than rolling", () => {
    expect(tierFor({ ...idle, rate: Number.NaN })).toBe("idle");
    expect(tierFor({ ...idle, rate: Number.POSITIVE_INFINITY })).toBe("idle");
  });
});

describe("qualityFor", () => {
  it("resolves each rung to its scale — idle and export are un-degraded", () => {
    expect(qualityFor(idle)).toEqual({ tier: "idle", scalePct: 100 });
    expect(qualityFor({ ...idle, rate: 1 })).toEqual({
      tier: "playback",
      scalePct: 50,
    });
    expect(qualityFor({ ...idle, interacting: true })).toEqual({
      tier: "interaction",
      scalePct: 25,
    });
    expect(qualityFor({ ...idle, exporting: true })).toEqual({
      tier: "export",
      scalePct: 100,
    });
  });

  it("never asks for more than full resolution", () => {
    for (const activity of [
      idle,
      { ...idle, rate: 1 },
      { ...idle, interacting: true },
      { ...idle, exporting: true },
    ]) {
      const { scalePct } = qualityFor(activity);
      expect(scalePct).toBeGreaterThan(0);
      expect(scalePct).toBeLessThanOrEqual(100);
    }
  });

  it("agrees with scalePctForTier", () => {
    const q = qualityFor({ ...idle, rate: 1 });
    expect(scalePctForTier(q.tier)).toBe(q.scalePct);
  });
});

describe("renderStride", () => {
  it("renders every frame at realtime and below, in both directions", () => {
    expect(renderStride(0)).toBe(1);
    expect(renderStride(1)).toBe(1);
    expect(renderStride(-1)).toBe(1);
    expect(renderStride(0.5)).toBe(1);
  });

  it("strides by the shuttle magnitude so the work per second stays flat", () => {
    expect(renderStride(2)).toBe(2);
    expect(renderStride(-2)).toBe(2);
    expect(renderStride(4)).toBe(4);
    expect(renderStride(8)).toBe(8);
  });

  it("caps the stride so a run never goes blind for long", () => {
    expect(renderStride(64)).toBe(MAX_RENDER_STRIDE);
  });

  it("falls back to every frame for a nonsense rate", () => {
    expect(renderStride(Number.NaN)).toBe(1);
    expect(renderStride(Number.POSITIVE_INFINITY)).toBe(1);
  });
});

describe("shouldRenderFrame", () => {
  it("renders every frame at stride 1", () => {
    for (let i = 0; i < 5; i++) expect(shouldRenderFrame(i, 1)).toBe(true);
  });

  it("renders the first frame of a run, then every Nth", () => {
    const rendered = [0, 1, 2, 3, 4, 5, 6, 7].filter((i) =>
      shouldRenderFrame(i, 4),
    );
    expect(rendered).toEqual([0, 4]);
  });

  it("drops exactly (N-1)/N of the frames at stride N", () => {
    const total = 80;
    for (const stride of [2, 4, 8]) {
      const kept = Array.from({ length: total }, (_, i) => i).filter((i) =>
        shouldRenderFrame(i, stride),
      );
      expect(kept.length).toBe(total / stride);
    }
  });
});
