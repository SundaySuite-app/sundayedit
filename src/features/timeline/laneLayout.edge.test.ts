/**
 * laneLayout — edge branches on top of laneLayout.test.ts: the stacking-order
 * tie-break, negative lane height, slow-speed span expansion, and the
 * never-negative duration floor.
 */
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
    ...SAMPLE_PROJECT.timeline_items[0],
    id,
    ...extra,
  };
}

describe("itemSpan — slow speed", () => {
  it("expands the on-timeline span at speed < 1×", () => {
    // 1000ms of source at 0.5× occupies 2000ms of timeline.
    expect(
      itemSpan(
        item("a", {
          timeline_start_ms: 100,
          in_ms: 0,
          out_ms: 1000,
          speed: 0.5,
        }),
      ),
    ).toEqual({ start_ms: 100, end_ms: 2100 });
  });
});

describe("stackedTracks — tie-break", () => {
  it("breaks equal indices on id so the order is stable across renders", () => {
    // Duplicate indices can exist transiently (e.g. mid-reorder snapshots).
    const a = [track("b", 1), track("a", 1), track("c", 0)];
    const b = [track("a", 1), track("c", 0), track("b", 1)];
    expect(stackedTracks(a).map((t) => t.id)).toEqual(["a", "b", "c"]);
    expect(stackedTracks(b).map((t) => t.id)).toEqual(["a", "b", "c"]);
  });

  it("does not mutate the input array", () => {
    const tracks = [track("a", 0), track("b", 1)];
    stackedTracks(tracks);
    expect(tracks.map((t) => t.id)).toEqual(["a", "b"]);
  });
});

describe("trackAtY — degenerate lane height", () => {
  it("returns null for a negative lane height instead of hit-testing garbage", () => {
    expect(trackAtY(0, [track("a", 0)], -48)).toBeNull();
  });
});

describe("timelineDurationMs — floor", () => {
  it("never goes negative even for a corrupt negative video duration", () => {
    const project: Project = {
      ...SAMPLE_PROJECT,
      video_duration_ms: -5000,
      timeline_items: [],
      captions: [],
    };
    expect(timelineDurationMs(project)).toBe(0);
  });

  it("clips still win over a corrupt negative video duration", () => {
    const project: Project = {
      ...SAMPLE_PROJECT,
      video_duration_ms: -5000,
      captions: [],
      timeline_items: [
        item("x", { timeline_start_ms: 0, in_ms: 0, out_ms: 750 }),
      ],
    };
    expect(timelineDurationMs(project)).toBe(750);
  });
});
