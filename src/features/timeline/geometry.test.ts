import { describe, it, expect } from "vitest";
import {
  clampZoom,
  msToX,
  xToMs,
  visibleRange,
  zoomAround,
  snapToFrame,
  shuttleRate,
  snap,
  visibleCaptions,
  tickIntervalMs,
  rulerTicks,
  formatTimecode,
  edgeHitWidthPx,
  CLIP_EDGE_HIT_PX,
  MIN_PX_PER_MS,
  MAX_PX_PER_MS,
  MAX_SHUTTLE,
  type TimelineView,
} from "./geometry";

const view: TimelineView = { pxPerMs: 0.1, scrollMs: 1000, widthPx: 800 };

describe("time ↔ pixel mapping", () => {
  it("round-trips ms → x → ms", () => {
    expect(msToX(1000, view)).toBe(0); // scrollMs at x=0
    expect(msToX(2000, view)).toBeCloseTo(100);
    expect(xToMs(msToX(3456, view), view)).toBeCloseTo(3456);
  });

  it("computes the visible range from width and zoom", () => {
    // 800px / 0.1 px/ms = 8000ms span, starting at scrollMs.
    expect(visibleRange(view)).toEqual([1000, 9000]);
  });
});

describe("zoom", () => {
  it("clamps to bounds", () => {
    expect(clampZoom(99)).toBe(MAX_PX_PER_MS);
    expect(clampZoom(0)).toBe(MIN_PX_PER_MS);
  });

  it("keeps the time under the anchor pixel fixed", () => {
    const anchorX = 300;
    const anchorMsBefore = xToMs(anchorX, view);
    const zoomed = zoomAround(view, 2, anchorX);
    expect(zoomed.pxPerMs).toBeCloseTo(0.2);
    // Same time still sits under the same pixel.
    expect(xToMs(anchorX, zoomed)).toBeCloseTo(anchorMsBefore);
  });

  it("never scrolls before zero", () => {
    const z = zoomAround({ ...view, scrollMs: 0 }, 0.5, 0);
    expect(z.scrollMs).toBeGreaterThanOrEqual(0);
  });
});

describe("edgeHitWidthPx", () => {
  it("is the fixed constant on a comfortably wide clip", () => {
    expect(edgeHitWidthPx(400)).toBe(CLIP_EDGE_HIT_PX);
  });

  it("stays fixed regardless of the clip's own width, above the halfway point", () => {
    // 8px is unaffected all the way down to width=16 (half=8).
    expect(edgeHitWidthPx(16)).toBe(8);
    expect(edgeHitWidthPx(20)).toBe(8);
  });

  it("shrinks to exactly half the clip below 2×edgePx, so zones tile with no gap", () => {
    expect(edgeHitWidthPx(10)).toBe(5);
    expect(edgeHitWidthPx(2)).toBe(1);
  });

  it("two edge zones never exceed the clip's own width", () => {
    for (const w of [0, 1, 2, 5, 8, 10, 16, 100]) {
      expect(edgeHitWidthPx(w) * 2).toBeLessThanOrEqual(w + 1e-9);
    }
  });

  it("never goes negative on a degenerate zero/negative width", () => {
    expect(edgeHitWidthPx(0)).toBe(0);
    expect(edgeHitWidthPx(-5)).toBe(0);
  });

  it("honours a custom edgePx", () => {
    expect(edgeHitWidthPx(100, 20)).toBe(20);
    expect(edgeHitWidthPx(10, 20)).toBe(5);
  });
});

describe("snapToFrame", () => {
  it("rounds to the nearest 30fps frame (~33.33ms)", () => {
    expect(snapToFrame(40, 30)).toBe(33); // frame 1 = 33.33 → 33
    expect(snapToFrame(20, 30)).toBe(33); // nearer frame 1 than frame 0
    expect(snapToFrame(10, 30)).toBe(0);
  });
  it("passes through when fps is unknown", () => {
    expect(snapToFrame(1234, 0)).toBe(1234);
  });
});

describe("visibleCaptions virtualization", () => {
  const caps = Array.from({ length: 1000 }, (_, i) => ({
    id: `c${i}`,
    start_ms: i * 1000,
    end_ms: i * 1000 + 800,
  }));

  it("returns only the window around [start,end] plus buffer", () => {
    const vis = visibleCaptions(caps, 500_000, 503_000, 5);
    const indices = vis.map((v) => v.index);
    // Window is captions 500..503; buffer 5 each side.
    expect(Math.min(...indices)).toBe(495);
    expect(Math.max(...indices)).toBe(508);
    expect(vis.length).toBeLessThan(20); // nowhere near all 1000
  });

  it("clamps the buffer at the start of the list", () => {
    const vis = visibleCaptions(caps, 0, 1500, 5);
    expect(vis[0].index).toBe(0);
  });

  it("handles a window past the end", () => {
    const vis = visibleCaptions(caps, 2_000_000, 2_001_000, 5);
    expect(vis.every((v) => v.index < caps.length)).toBe(true);
  });
});

describe("ruler", () => {
  it("picks a coarser interval as you zoom out", () => {
    const zoomedIn = tickIntervalMs({ ...view, pxPerMs: 1 }, 80);
    const zoomedOut = tickIntervalMs({ ...view, pxPerMs: 0.01 }, 80);
    expect(zoomedOut).toBeGreaterThan(zoomedIn);
  });

  it("spaces ticks at least minPxBetween apart", () => {
    const v = { ...view, pxPerMs: 0.1 };
    const interval = tickIntervalMs(v, 80);
    expect(interval * v.pxPerMs).toBeGreaterThanOrEqual(80);
  });

  it("generates ticks across the visible range", () => {
    const ticks = rulerTicks(view, 80);
    const [start, end] = visibleRange(view);
    expect(ticks.every((t) => t >= start && t <= end)).toBe(true);
    expect(ticks.length).toBeGreaterThan(0);
  });
});

describe("formatTimecode", () => {
  it("formats MM:SS:FF under an hour", () => {
    expect(formatTimecode(0, 30)).toBe("00:00:00");
    expect(formatTimecode(65_500, 30)).toBe("01:05:15"); // 0.5s @30 = frame 15
  });
  it("adds hours past 60 minutes", () => {
    expect(formatTimecode(3_661_000, 30)).toBe("1:01:01:00");
  });
  it("never emits a frame >= fps", () => {
    expect(formatTimecode(999, 30)).toBe("00:00:29"); // clamped to frame 29
  });
});

describe("shuttleRate (J/K/L transport)", () => {
  it("l ramps forward by doubling, capped", () => {
    expect(shuttleRate(0, "l")).toBe(1);
    expect(shuttleRate(1, "l")).toBe(2);
    expect(shuttleRate(2, "l")).toBe(4);
    expect(shuttleRate(8, "l")).toBe(MAX_SHUTTLE);
  });
  it("j mirrors l in reverse", () => {
    expect(shuttleRate(0, "j")).toBe(-1);
    expect(shuttleRate(-1, "j")).toBe(-2);
    expect(shuttleRate(-8, "j")).toBe(-MAX_SHUTTLE);
  });
  it("k stops, and the opposite key reverses direction at 1×", () => {
    expect(shuttleRate(4, "k")).toBe(0);
    expect(shuttleRate(4, "j")).toBe(-1);
    expect(shuttleRate(-4, "l")).toBe(1);
  });
});

describe("snap", () => {
  const targets = [1000, 2000, 5000];
  it("snaps to a target within the pixel tolerance", () => {
    expect(snap(1010, targets, 0.1, 6)).toBe(1000); // 1px away
  });
  it("leaves the value when no target is close enough", () => {
    expect(snap(1200, targets, 0.1, 6)).toBe(1200); // 20px away
  });
  it("picks the nearest of competing targets", () => {
    expect(snap(1600, [1000, 2000], 0.01, 100)).toBe(2000);
  });
  it("is a no-op with no targets", () => {
    expect(snap(1234, [], 0.1)).toBe(1234);
  });
});
