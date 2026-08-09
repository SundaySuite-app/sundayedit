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

describe("clipDragToOp — move", () => {
  it("shifts timeline_start by the delta at speed 1×", () => {
    expect(clipDragToOp(item(), "move", 300)).toEqual({
      op: "move",
      itemId: "i1",
      trackId: "tv",
      timelineStartMs: 1300,
    });
  });

  it("is speed-independent: a move never touches the source domain", () => {
    // Same timeline displacement at 2× — in/out are untouched by a move, so
    // the committed start must not scale with speed.
    expect(clipDragToOp(item({ speed: 2 }), "move", 300)).toEqual({
      op: "move",
      itemId: "i1",
      trackId: "tv",
      timelineStartMs: 1300,
    });
  });

  it("carries the cross-track target, even at zero delta", () => {
    expect(clipDragToOp(item(), "move", 0, "tv2")).toEqual({
      op: "move",
      itemId: "i1",
      trackId: "tv2",
      timelineStartMs: 1000,
    });
  });

  it("clamps the leading edge to timeline 0 on an overshoot left", () => {
    expect(clipDragToOp(item(), "move", -5000)).toEqual({
      op: "move",
      itemId: "i1",
      trackId: "tv",
      timelineStartMs: 0,
    });
  });

  it("is a no-op at zero delta on the same track", () => {
    expect(clipDragToOp(item(), "move", 0)).toEqual({ op: "none" });
  });
});

describe("clipDragToOp — resize-start (left edge)", () => {
  it("moves in by Δ and start by Δ at speed 1×", () => {
    expect(clipDragToOp(item(), "resize-start", -200)).toEqual({
      op: "trim-start",
      itemId: "i1",
      newInMs: 300,
      newTimelineStartMs: 800,
    });
  });

  it("moves in by Δ×speed but start by Δ at speed 2×", () => {
    // A 200ms timeline reveal consumes 400ms of source at 2×.
    expect(clipDragToOp(item({ speed: 2 }), "resize-start", -200)).toEqual({
      op: "trim-start",
      itemId: "i1",
      newInMs: 100,
      newTimelineStartMs: 800,
    });
  });

  it("clamps an overshoot left to timeline 0, keeping in/start coupled", () => {
    // start=300 binds before in=1000: effective Δ is −300, not −1000.
    expect(
      clipDragToOp(
        item({ in_ms: 1000, timeline_start_ms: 300 }),
        "resize-start",
        -1000,
      ),
    ).toEqual({
      op: "trim-start",
      itemId: "i1",
      newInMs: 700,
      newTimelineStartMs: 0,
    });
  });

  it("clamps an overshoot left to source in=0 when that binds first", () => {
    // in=100 binds before start=5000: effective Δ is −100.
    expect(
      clipDragToOp(
        item({ in_ms: 100, timeline_start_ms: 5000 }),
        "resize-start",
        -1000,
      ),
    ).toEqual({
      op: "trim-start",
      itemId: "i1",
      newInMs: 0,
      newTimelineStartMs: 4900,
    });
  });

  it("is a no-op at zero delta", () => {
    expect(clipDragToOp(item(), "resize-start", 0)).toEqual({ op: "none" });
  });
});

describe("clipDragToOp — resize-end (right edge)", () => {
  it("moves out by Δ at speed 1×, leaving start alone", () => {
    expect(clipDragToOp(item(), "resize-end", 300)).toEqual({
      op: "trim-end",
      itemId: "i1",
      newOutMs: 2800,
    });
  });

  it("moves out by Δ×speed at speed 2× — the asymmetry-suspect case", () => {
    // Extending the timeline tail by 300ms plays 600ms more source at 2×.
    expect(clipDragToOp(item({ speed: 2 }), "resize-end", 300)).toEqual({
      op: "trim-end",
      itemId: "i1",
      newOutMs: 3100,
    });
  });

  it("rounds fractional source deltas to whole ms (backend fields are i64)", () => {
    expect(clipDragToOp(item({ speed: 1.5 }), "resize-end", 333)).toEqual({
      op: "trim-end",
      itemId: "i1",
      newOutMs: 3000, // 2500 + 499.5 → rounded
    });
  });

  it("is a no-op at zero delta", () => {
    expect(clipDragToOp(item(), "resize-end", 0)).toEqual({ op: "none" });
  });
});
