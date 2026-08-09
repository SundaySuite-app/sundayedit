import { describe, it, expect } from "vitest";

import type { Project } from "@/lib/bindings/Project";
import type { TimelineItem } from "@/lib/bindings/TimelineItem";
import type { Track } from "@/lib/bindings/Track";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import {
  itemSpan,
  stackedTracks,
  trackAtY,
  timelineDurationMs,
} from "./laneLayout";

function track(id: string, index: number, extra?: Partial<Track>): Track {
  return {
    id,
    kind: "video",
    name: id,
    index,
    enabled: true,
    locked: false,
    muted: false,
    solo: false,
    ...extra,
  };
}

function item(id: string, extra?: Partial<TimelineItem>): TimelineItem {
  return {
    id,
    track_id: "tv",
    kind: "av",
    source_media_id: "m1",
    in_ms: 0,
    out_ms: 1000,
    timeline_start_ms: 0,
    speed: 1,
    transform: {
      x: 0,
      y: 0,
      scale: 1,
      rotation_deg: 0,
      opacity: 1,
      crop: null,
    },
    effects: [],
    transition_in: null,
    text: null,
    enabled: true,
    locked: false,
    ...extra,
  };
}

describe("itemSpan", () => {
  it("maps a clip to its timeline span (speed 1× = source length)", () => {
    expect(
      itemSpan(item("a", { timeline_start_ms: 500, out_ms: 2000 })),
    ).toEqual({ start_ms: 500, end_ms: 2500 });
  });

  it("compresses the on-timeline span at speed > 1×", () => {
    // 2000ms of source at 2× occupies 1000ms of timeline.
    expect(
      itemSpan(item("a", { timeline_start_ms: 0, out_ms: 2000, speed: 2 })),
    ).toEqual({ start_ms: 0, end_ms: 1000 });
  });
});

describe("stackedTracks", () => {
  it("orders top-lane-first by descending index (top-most composites over)", () => {
    const ordered = stackedTracks([
      track("a", 0),
      track("c", 2),
      track("b", 1),
    ]);
    expect(ordered.map((t) => t.id)).toEqual(["c", "b", "a"]);
  });
});

describe("trackAtY", () => {
  it("resolves a Y to the track via the stacking order", () => {
    const tracks = [track("a", 0), track("b", 1)]; // stacked: [b, a]
    expect(trackAtY(0, tracks, 48)?.id).toBe("b"); // lane 0 = top = b
    expect(trackAtY(47, tracks, 48)?.id).toBe("b"); // last px of lane 0
    expect(trackAtY(48, tracks, 48)?.id).toBe("a"); // lane boundary → lane 1
    expect(trackAtY(999, tracks, 48)).toBeNull();
  });

  it("returns null above the lanes area, with no tracks, or degenerate lane height", () => {
    const tracks = [track("a", 0), track("b", 1)];
    expect(trackAtY(-1, tracks, 48)).toBeNull();
    expect(trackAtY(2 * 48, tracks, 48)).toBeNull(); // just past the last lane
    expect(trackAtY(0, [], 48)).toBeNull();
    expect(trackAtY(0, tracks, 0)).toBeNull();
  });
});

describe("timelineDurationMs", () => {
  it("is the primary video duration when nothing outruns it", () => {
    expect(timelineDurationMs(SAMPLE_PROJECT)).toBe(
      SAMPLE_PROJECT.video_duration_ms,
    );
  });

  it("grows to the furthest clip end", () => {
    const project: Project = {
      ...SAMPLE_PROJECT,
      captions: [],
      video_duration_ms: 1000,
      timeline_items: [item("x", { timeline_start_ms: 5000, out_ms: 2000 })],
    };
    expect(timelineDurationMs(project)).toBe(7000);
  });

  it("grows to the furthest caption end", () => {
    const project: Project = {
      ...SAMPLE_PROJECT,
      video_duration_ms: 1000,
      timeline_items: [],
      captions: [{ ...SAMPLE_PROJECT.captions[0], start_ms: 0, end_ms: 9999 }],
    };
    expect(timelineDurationMs(project)).toBe(9999);
  });
});
