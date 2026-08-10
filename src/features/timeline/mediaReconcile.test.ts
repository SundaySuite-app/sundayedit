import { describe, it, expect } from "vitest";

import {
  reconcileMedia,
  defaultReconcileConfig,
  frameBudgetMs,
  type MediaAction,
  type MediaElementSnapshot,
  type TimelineSyncState,
} from "./mediaReconcile";

// One frame at 30 fps — the whole tolerance table hangs off this number.
const FRAME_MS = 1000 / 30; // 33.33
const HALF_FRAME_MS = FRAME_MS / 2; // 16.67

const PLAYING: TimelineSyncState = { playheadMs: 5000, rate: 1, fps: 30 };
const STOPPED: TimelineSyncState = { playheadMs: 5000, rate: 0, fps: 30 };

/** A healthy element sitting exactly on its target, playing in sync. */
function element(
  over: Partial<MediaElementSnapshot> = {},
): MediaElementSnapshot {
  return {
    itemId: "clip-1",
    targetTimeMs: 2000,
    currentTimeMs: 2000,
    paused: false,
    seeking: false,
    readyState: 4,
    playbackRate: 1,
    hasBeenSeeked: true,
    ...over,
  };
}

/** A pinned element (transport not playing) sitting on its target. */
function pinned(
  over: Partial<MediaElementSnapshot> = {},
): MediaElementSnapshot {
  return element({ paused: true, ...over });
}

const seeks = (actions: MediaAction[]) =>
  actions.filter((a) => a.type === "seek");

describe("frameBudgetMs / defaultReconcileConfig", () => {
  it("derives the budget from the project frame rate", () => {
    expect(frameBudgetMs(30)).toBeCloseTo(33.333, 3);
    expect(frameBudgetMs(60)).toBeCloseTo(16.667, 3);
    expect(frameBudgetMs(24000 / 1001)).toBeCloseTo(41.708, 3);
  });

  it("falls back to 30 fps for a nonsense frame rate", () => {
    expect(frameBudgetMs(0)).toBeCloseTo(FRAME_MS);
    expect(frameBudgetMs(-24)).toBeCloseTo(FRAME_MS);
  });

  it("keeps every drift budget inside one frame — playing AND paused", () => {
    for (const fps of [24, 25, 30, 50, 60]) {
      const config = defaultReconcileConfig(fps);
      const frame = frameBudgetMs(fps);
      expect(config.playingBudgetMs).toBeLessThanOrEqual(frame);
      expect(config.pausedBudgetMs).toBeLessThanOrEqual(frame);
      expect(config.deadBandMs).toBeLessThan(config.playingBudgetMs);
    }
  });

  it("nudges by ±2%, as upstream does", () => {
    const config = defaultReconcileConfig(30);
    expect(config.nudgeUp).toBeCloseTo(1.02);
    expect(config.nudgeDown).toBeCloseTo(0.98);
  });
});

describe("reconcileMedia — playing, element in sync", () => {
  it("issues nothing when the element tracks the playhead", () => {
    expect(reconcileMedia(PLAYING, [element()])).toEqual([]);
  });

  it("tolerates drift inside the dead band", () => {
    expect(
      reconcileMedia(PLAYING, [element({ currentTimeMs: 2000 + 5 })]),
    ).toEqual([]);
  });

  it("restores the nominal rate once a nudge is no longer needed", () => {
    const actions = reconcileMedia(PLAYING, [element({ playbackRate: 1.02 })]);
    expect(actions).toEqual([
      { type: "setRate", itemId: "clip-1", rate: 1, reason: "rate-restore" },
    ]);
  });

  it("leaves an already-correct rate alone", () => {
    expect(
      reconcileMedia(PLAYING, [element({ playbackRate: 1.0001 })]),
    ).toEqual([]);
  });
});

describe("reconcileMedia — playing, sub-frame drift becomes a rate nudge", () => {
  it("speeds the element up when it lags the playhead", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ currentTimeMs: 2000 - 20 }),
    ]);
    expect(actions).toHaveLength(1);
    expect(actions[0].type).toBe("setRate");
    expect(actions[0].rate).toBeCloseTo(1.02);
    expect(actions[0].reason).toBe("drift-recovery");
  });

  it("slows the element down when it runs ahead", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ currentTimeMs: 2000 + 20 }),
    ]);
    expect(actions[0].type).toBe("setRate");
    expect(actions[0].rate).toBeCloseTo(0.98);
  });

  it("bends time instead of cutting it — never a seek inside the budget", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ currentTimeMs: 2000 + FRAME_MS - 1 }),
    ]);
    expect(seeks(actions)).toEqual([]);
  });

  it("does not re-issue a nudge already applied", () => {
    expect(
      reconcileMedia(PLAYING, [
        element({ currentTimeMs: 2000 - 20, playbackRate: 1.02 }),
      ]),
    ).toEqual([]);
  });

  it("scales the nudge with the clip's own speed", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ currentTimeMs: 2000 - 20, speed: 2, playbackRate: 2 }),
    ]);
    expect(actions[0].rate).toBeCloseTo(2 * 1.02);
  });
});

describe("reconcileMedia — playing, drift past the budget becomes a seek", () => {
  it("seeks once drift exceeds one frame", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ currentTimeMs: 2000 + FRAME_MS + 1 }),
    ]);
    expect(actions).toEqual([
      {
        type: "seek",
        itemId: "clip-1",
        timeMs: 2000,
        reason: "drift-recovery",
      },
    ]);
  });

  it("reports a large jump as a scrub, not as drift", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ currentTimeMs: 200 }), // 1.8s adrift
    ]);
    expect(actions[0].reason).toBe("scrub");
    expect(actions[0].timeMs).toBe(2000);
  });

  it("uses the fps-derived budget, not a fixed one", () => {
    const drifted = element({ currentTimeMs: 2000 + 20 });
    // 20ms is sub-frame at 30fps (nudge) but over a frame at 60fps (seek).
    expect(reconcileMedia({ ...PLAYING, fps: 30 }, [drifted])[0].type).toBe(
      "setRate",
    );
    expect(reconcileMedia({ ...PLAYING, fps: 60 }, [drifted])[0].type).toBe(
      "seek",
    );
  });

  it("corrects on every frame — no time-based seek lockout", () => {
    // Upstream would refuse the second correction for 400ms (1500ms with
    // audio). We only defer while the element itself reports `seeking`.
    const drifted = element({ currentTimeMs: 3000 });
    expect(seeks(reconcileMedia(PLAYING, [drifted]))).toHaveLength(1);
    expect(seeks(reconcileMedia(PLAYING, [drifted]))).toHaveLength(1);
  });

  it("waits while the element is still seeking", () => {
    expect(
      reconcileMedia(PLAYING, [
        element({ currentTimeMs: 3000, seeking: true }),
      ]),
    ).toEqual([]);
  });
});

describe("reconcileMedia — playing, element not rolling", () => {
  it("starts a paused element that should be playing", () => {
    const actions = reconcileMedia(PLAYING, [element({ paused: true })]);
    expect(actions).toEqual([
      { type: "play", itemId: "clip-1", reason: "transport" },
    ]);
  });

  it("lands on the right frame before rolling", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ paused: true, currentTimeMs: 500 }),
    ]);
    expect(actions.map((a) => a.type)).toEqual(["seek", "play"]);
    expect(actions[0].timeMs).toBe(2000);
    expect(actions[0].reason).toBe("drift-recovery");
  });

  it("tags the very first seek into a clip as clip-enter", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ paused: true, currentTimeMs: 500, hasBeenSeeked: false }),
    ]);
    expect(actions[0].reason).toBe("clip-enter");
  });

  it("still asks for play while a seek is in flight", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ paused: true, currentTimeMs: 500, seeking: true }),
    ]);
    expect(actions).toEqual([
      { type: "play", itemId: "clip-1", reason: "transport" },
    ]);
  });

  it("does not seek an element that has no metadata yet", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ paused: true, currentTimeMs: 500, readyState: 0 }),
    ]);
    expect(seeks(actions)).toEqual([]);
    expect(actions.map((a) => a.type)).toEqual(["play"]);
  });
});

describe("reconcileMedia — paused transport pins the frame", () => {
  it("pauses an element that is still rolling", () => {
    const actions = reconcileMedia(STOPPED, [element()]);
    expect(actions).toEqual([
      { type: "pause", itemId: "clip-1", reason: "transport" },
    ]);
  });

  it("leaves a pinned element on target alone", () => {
    expect(reconcileMedia(STOPPED, [pinned()])).toEqual([]);
  });

  it("pins tighter than one frame — half a frame moves the frame shown", () => {
    expect(
      reconcileMedia(STOPPED, [
        pinned({ currentTimeMs: 2000 + HALF_FRAME_MS - 1 }),
      ]),
    ).toEqual([]);
    const actions = reconcileMedia(STOPPED, [
      pinned({ currentTimeMs: 2000 + HALF_FRAME_MS + 1 }),
    ]);
    expect(actions).toEqual([
      { type: "seek", itemId: "clip-1", timeMs: 2000, reason: "transport" },
    ]);
  });

  it("forces the first seek even at zero drift, so a frame gets decoded", () => {
    const actions = reconcileMedia(STOPPED, [pinned({ hasBeenSeeked: false })]);
    expect(actions).toEqual([
      { type: "seek", itemId: "clip-1", timeMs: 2000, reason: "clip-enter" },
    ]);
  });

  it("does not pile a second seek onto a seeking element", () => {
    expect(
      reconcileMedia(STOPPED, [
        pinned({ currentTimeMs: 500, seeking: true, hasBeenSeeked: false }),
      ]),
    ).toEqual([]);
  });

  it("waits for metadata before pinning", () => {
    expect(
      reconcileMedia(STOPPED, [
        pinned({ currentTimeMs: 500, readyState: 0, hasBeenSeeked: false }),
      ]),
    ).toEqual([]);
  });

  it("tags a big paused jump as a scrub", () => {
    const actions = reconcileMedia(STOPPED, [pinned({ currentTimeMs: 100 })]);
    expect(actions[0].reason).toBe("scrub");
  });
});

describe("reconcileMedia — reverse and shuttle scrub instead of playing", () => {
  it("pauses and pins during reverse playback", () => {
    const actions = reconcileMedia({ ...PLAYING, rate: -1 }, [
      element({ currentTimeMs: 1000 }),
    ]);
    expect(actions.map((a) => a.type)).toEqual(["pause", "seek"]);
    expect(actions[1].timeMs).toBe(2000);
    expect(actions[1].reason).toBe("scrub");
  });

  it("pauses and pins during fast shuttle", () => {
    const actions = reconcileMedia({ ...PLAYING, rate: 4 }, [
      element({ currentTimeMs: 1000 }),
    ]);
    expect(actions.map((a) => a.type)).toEqual(["pause", "seek"]);
  });

  it("never issues play for a non-realtime transport", () => {
    for (const rate of [-8, -1, 0, 2, 8]) {
      const actions = reconcileMedia({ ...PLAYING, rate }, [
        element({ paused: true, currentTimeMs: 1000 }),
      ]);
      expect(actions.some((a) => a.type === "play")).toBe(false);
    }
  });
});

describe("reconcileMedia — clip speed", () => {
  it("plays a half-speed clip natively at 0.5", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ speed: 0.5, playbackRate: 1 }),
    ]);
    expect(actions).toEqual([
      { type: "setRate", itemId: "clip-1", rate: 0.5, reason: "rate-restore" },
    ]);
  });

  it("plays a double-speed clip natively at 2", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ speed: 2, playbackRate: 1 }),
    ]);
    expect(actions[0].rate).toBeCloseTo(2);
  });

  it("scrubs a clip whose speed is beyond clean native playback", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ speed: 8, currentTimeMs: 1000 }),
    ]);
    expect(actions.map((a) => a.type)).toEqual(["pause", "seek"]);
  });

  it("scrubs a clip slowed below clean native playback", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ speed: 0.1, currentTimeMs: 1000 }),
    ]);
    expect(actions.map((a) => a.type)).toEqual(["pause", "seek"]);
  });
});

describe("reconcileMedia — inactive clips", () => {
  it("pauses an element whose clip left the playhead", () => {
    const actions = reconcileMedia(PLAYING, [element({ targetTimeMs: null })]);
    expect(actions).toEqual([
      { type: "pause", itemId: "clip-1", reason: "clip-exit" },
    ]);
  });

  it("parks an upcoming clip on its prewarm frame", () => {
    const actions = reconcileMedia(PLAYING, [
      element({
        targetTimeMs: null,
        paused: true,
        currentTimeMs: 0,
        prewarmTimeMs: 4000,
      }),
    ]);
    expect(actions).toEqual([
      { type: "seek", itemId: "clip-1", timeMs: 4000, reason: "prewarm" },
    ]);
  });

  it("leaves an already-parked element alone", () => {
    expect(
      reconcileMedia(PLAYING, [
        element({
          targetTimeMs: null,
          paused: true,
          currentTimeMs: 4000,
          prewarmTimeMs: 4000,
        }),
      ]),
    ).toEqual([]);
  });

  it("does not prewarm when the caller asks for none", () => {
    expect(
      reconcileMedia(PLAYING, [
        element({ targetTimeMs: null, paused: true, prewarmTimeMs: null }),
      ]),
    ).toEqual([]);
  });

  it("pauses before parking, so an exiting clip goes quiet first", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ targetTimeMs: null, currentTimeMs: 0, prewarmTimeMs: 4000 }),
    ]);
    expect(actions.map((a) => a.type)).toEqual(["pause", "seek"]);
  });
});

describe("reconcileMedia — multiple elements", () => {
  it("decides each element independently, in order", () => {
    const actions = reconcileMedia(PLAYING, [
      element({ itemId: "a" }), // in sync → nothing
      element({ itemId: "b", currentTimeMs: 2000 - 20 }), // nudge
      element({ itemId: "c", currentTimeMs: 100 }), // hard seek
      element({ itemId: "d", targetTimeMs: null }), // gone → pause
    ]);
    expect(actions).toEqual([
      { type: "setRate", itemId: "b", rate: 1.02, reason: "drift-recovery" },
      { type: "seek", itemId: "c", timeMs: 2000, reason: "scrub" },
      { type: "pause", itemId: "d", reason: "clip-exit" },
    ]);
  });

  it("handles a cut: outgoing clip parks, incoming clip enters", () => {
    const actions = reconcileMedia(PLAYING, [
      element({
        itemId: "outgoing",
        targetTimeMs: null,
        prewarmTimeMs: 0,
        currentTimeMs: 9000,
      }),
      element({
        itemId: "incoming",
        paused: true,
        currentTimeMs: 0,
        hasBeenSeeked: false,
      }),
    ]);
    expect(actions).toEqual([
      { type: "pause", itemId: "outgoing", reason: "clip-exit" },
      { type: "seek", itemId: "outgoing", timeMs: 0, reason: "prewarm" },
      {
        type: "seek",
        itemId: "incoming",
        timeMs: 2000,
        reason: "clip-enter",
      },
      { type: "play", itemId: "incoming", reason: "transport" },
    ]);
  });

  it("returns nothing for an empty pool", () => {
    expect(reconcileMedia(PLAYING, [])).toEqual([]);
  });
});

describe("reconcileMedia — purity and configuration", () => {
  it("does not mutate its inputs", () => {
    const snapshot = Object.freeze(element({ currentTimeMs: 100 }));
    const timeline = Object.freeze({ ...PLAYING });
    expect(() => reconcileMedia(timeline, [snapshot])).not.toThrow();
    expect(snapshot.currentTimeMs).toBe(100);
  });

  it("is deterministic across repeated calls", () => {
    const snapshots = [
      element({ itemId: "a", currentTimeMs: 2000 - 20 }),
      element({ itemId: "b", currentTimeMs: 100 }),
    ];
    expect(reconcileMedia(PLAYING, snapshots)).toEqual(
      reconcileMedia(PLAYING, snapshots),
    );
  });

  it("honours a caller override of the budget", () => {
    const drifted = element({ currentTimeMs: 2000 + 100 });
    // With a 1s budget the 100ms drift is inside the dead band → no action.
    expect(
      reconcileMedia(PLAYING, [drifted], {
        playingBudgetMs: 1000,
        deadBandMs: 500,
      }),
    ).toEqual([]);
    expect(seeks(reconcileMedia(PLAYING, [drifted]))).toHaveLength(1);
  });
});
