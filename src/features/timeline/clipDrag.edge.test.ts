/**
 * clipDragToOp — edge branches on top of clipDrag.test.ts: the speed floor
 * (degenerate speed ≤ 0 must not divide by zero or explode the source delta),
 * fully-clamped drags collapsing to no-ops, and rounding on the left edge.
 */
import { describe, it, expect } from "vitest";

import type { ClipDragItem } from "./clipDrag";
import { clipDragToOp } from "./clipDrag";

function item(extra?: Partial<ClipDragItem>): ClipDragItem {
  return {
    id: "i1",
    track_id: "tv",
    in_ms: 500,
    out_ms: 2500,
    timeline_start_ms: 1000,
    speed: 1,
    ...extra,
  };
}

describe("clipDragToOp — speed floor (degenerate speed)", () => {
  it("treats speed 0 as 0.01 on resize-end instead of a zero source delta", () => {
    // 300ms timeline × floored 0.01 = 3ms source.
    expect(clipDragToOp(item({ speed: 0 }), "resize-end", 300)).toEqual({
      op: "trim-end",
      itemId: "i1",
      newOutMs: 2503,
    });
  });

  it("treats a negative speed as 0.01 so the in-bound never flips sign", () => {
    // At floored 0.01, revealing 500ms of source spans 50 000 timeline ms —
    // in=500 binds at Δ=−50 000, well after start=1000 binds at Δ=−1000.
    expect(clipDragToOp(item({ speed: -2 }), "resize-start", -99_999)).toEqual({
      op: "trim-start",
      itemId: "i1",
      newInMs: 490, // 500 + (−1000 × 0.01)
      newTimelineStartMs: 0,
    });
  });
});

describe("clipDragToOp — fully-clamped drags", () => {
  it("a leftward move of a clip already at timeline 0 is a no-op", () => {
    expect(clipDragToOp(item({ timeline_start_ms: 0 }), "move", -500)).toEqual({
      op: "none",
    });
  });

  it("a fully-clamped move still commits when it changes track", () => {
    // Δ collapses to 0 but the drop target differs → the move must land.
    expect(
      clipDragToOp(item({ timeline_start_ms: 0 }), "move", -500, "tv2"),
    ).toEqual({
      op: "move",
      itemId: "i1",
      trackId: "tv2",
      timelineStartMs: 0,
    });
  });

  it("a leftward left-edge trim with nothing to reveal (in=0, start=0) is a no-op", () => {
    expect(
      clipDragToOp(
        item({ in_ms: 0, timeline_start_ms: 0 }),
        "resize-start",
        -500,
      ),
    ).toEqual({ op: "none" });
  });
});

describe("clipDragToOp — rounding on the left edge", () => {
  it("rounds both in and start to whole ms at fractional speed", () => {
    // Δ=−333 at 1.5×: in moves by −499.5 → 500−499.5=0.5 → rounds to 1 (in is
    // clamped at −in/speed=−333.33… so −333 survives unclamped).
    expect(clipDragToOp(item({ speed: 1.5 }), "resize-start", -333)).toEqual({
      op: "trim-start",
      itemId: "i1",
      newInMs: 1, // 500 − 499.5, rounded
      newTimelineStartMs: 667,
    });
  });

  it("rounds a fractional move delta to a whole timeline start", () => {
    expect(clipDragToOp(item(), "move", 100.4)).toEqual({
      op: "move",
      itemId: "i1",
      trackId: "tv",
      timelineStartMs: 1100,
    });
  });
});
