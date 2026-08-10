import { describe, it, expect, vi } from "vitest";

import {
  PlaybackClock,
  snapMsToFrame,
  MAX_CLOCK_RATE,
  type ClockTimeSource,
  type PlaybackClockOptions,
  type PlaybackSnapshot,
} from "./playbackClock";

/**
 * A fully controlled world for the clock: a fake audio source, a fake wall
 * clock and a manual frame scheduler. Audio and wall time advance separately
 * so we can reproduce the exact situations the lift exists for — rAF starved
 * while the audio clock keeps running, and an audio clock that has not started
 * yet.
 */
function harness(options: Partial<PlaybackClockOptions> = {}) {
  let audioSec = 0;
  let wallMs = 0;
  let running = true;
  let resumeCalls = 0;
  let closeCalls = 0;

  const frames = new Map<number, () => void>();
  let nextHandle = 0;

  const source: ClockTimeSource = {
    get currentTimeSec() {
      return audioSec;
    },
    get running() {
      return running;
    },
    resume() {
      resumeCalls += 1;
    },
    close() {
      closeCalls += 1;
    },
  };

  const clock = new PlaybackClock({
    createTimeSource: () => source,
    requestFrame: (cb) => {
      const handle = ++nextHandle;
      frames.set(handle, cb);
      return handle;
    },
    cancelFrame: (handle) => {
      frames.delete(handle);
    },
    nowMs: () => wallMs,
    durationMs: 10_000,
    fps: 30,
    ...options,
  });

  return {
    clock,
    /** Advance both clocks (audio only advances while the source runs). */
    advance(ms: number) {
      wallMs += ms;
      if (running) audioSec += ms / 1000;
    },
    /** Advance only the audio clock — rAF/main thread starved. */
    advanceAudioOnly(ms: number) {
      audioSec += ms / 1000;
    },
    /** Advance only the wall clock — audio hardware not running yet. */
    advanceWallOnly(ms: number) {
      wallMs += ms;
    },
    startAudio() {
      running = true;
    },
    stopAudio() {
      running = false;
    },
    /** Run every scheduled frame callback once. */
    runFrame() {
      const pending = [...frames.values()];
      frames.clear();
      for (const cb of pending) cb();
    },
    pendingFrames: () => [...frames.values()],
    resumeCalls: () => resumeCalls,
    closeCalls: () => closeCalls,
  };
}

describe("snapMsToFrame", () => {
  it("rounds to the nearest frame boundary at 30 fps", () => {
    expect(snapMsToFrame(100, 30)).toBeCloseTo(100);
    // 110ms is frame 3.3 → frame 3 → 100ms
    expect(snapMsToFrame(110, 30)).toBeCloseTo(100);
    // 120ms is frame 3.6 → frame 4 → 133.33ms
    expect(snapMsToFrame(120, 30)).toBeCloseTo(4000 / 30);
  });

  it("honours non-integer broadcast frame rates", () => {
    const ntsc = 30000 / 1001; // 29.97
    const frameMs = 1000 / ntsc;
    expect(snapMsToFrame(frameMs * 15.6, ntsc)).toBeCloseTo(frameMs * 16);
    const film = 24000 / 1001; // 23.976
    expect(snapMsToFrame((1000 / film) * 10.4, film)).toBeCloseTo(
      (1000 / film) * 10,
    );
  });

  it("is a no-op for a non-positive fps", () => {
    expect(snapMsToFrame(1234.5, 0)).toBe(1234.5);
    expect(snapMsToFrame(1234.5, -30)).toBe(1234.5);
  });
});

describe("PlaybackClock — construction and configuration", () => {
  it("starts stopped at zero", () => {
    const { clock } = harness();
    expect(clock.timeMs).toBe(0);
    expect(clock.status).toBe("stopped");
    expect(clock.rate).toBe(0);
    expect(clock.isSeeking).toBe(false);
  });

  it("exposes duration and fps through the snapshot", () => {
    const { clock } = harness();
    const snapshot: PlaybackSnapshot = clock.getSnapshot();
    expect(snapshot).toEqual({
      timeMs: 0,
      status: "stopped",
      rate: 0,
      durationMs: 10_000,
      fps: 30,
    });
  });

  it("rejects nonsense duration and fps", () => {
    const { clock } = harness();
    clock.setDurationMs(Number.NaN);
    expect(clock.durationMs).toBe(0);
    clock.setDurationMs(-500);
    expect(clock.durationMs).toBe(0);
    clock.setFps(Number.POSITIVE_INFINITY);
    expect(clock.fps).toBe(30);
    clock.setFps(0);
    expect(clock.fps).toBe(1);
  });

  it("pulls the playhead back when the duration shrinks under it", () => {
    const { clock } = harness();
    clock.seek(8000);
    clock.setDurationMs(5000);
    expect(clock.timeMs).toBe(5000);
  });
});

describe("PlaybackClock — the clock rides the time source, not the frames", () => {
  it("advances without a single frame callback running", () => {
    const h = harness();
    h.clock.play();
    h.advance(250);
    // No runFrame() at all: an imperative read is still exact.
    expect(h.clock.timeMs).toBeCloseTo(250);
  });

  it("stays correct when frames are starved but audio keeps running", () => {
    const h = harness();
    h.clock.play();
    // The tab was backgrounded: one frame in a whole second of audio.
    h.advanceAudioOnly(1000);
    h.advanceWallOnly(16);
    expect(h.clock.timeMs).toBeCloseTo(1000);
  });

  it("does not freeze while the audio source is still suspended", () => {
    const h = harness();
    h.stopAudio();
    h.clock.play();
    expect(h.resumeCalls()).toBe(1);
    // Audio has not started; the wall clock carries the playhead meanwhile.
    h.advance(200);
    expect(h.clock.timeMs).toBeCloseTo(200);
  });

  it("adopts the audio clock without a jump once it starts running", () => {
    const h = harness();
    h.stopAudio();
    h.clock.play();
    h.advance(200); // wall only — audio frozen at 0
    h.startAudio();
    h.runFrame(); // the tick notices and re-anchors
    expect(h.clock.timeMs).toBeCloseTo(200);
    h.advanceAudioOnly(100);
    expect(h.clock.timeMs).toBeCloseTo(300);
  });

  it("keeps counting when the audio source suspends mid-playback", () => {
    const h = harness();
    h.clock.play();
    h.advance(100);
    h.runFrame();
    h.stopAudio(); // audio hardware went away
    h.advance(100); // wall only
    expect(h.clock.timeMs).toBeCloseTo(200);
  });

  it("plays at the transport rate", () => {
    const h = harness();
    h.clock.setRate(2);
    h.clock.play();
    h.advance(500);
    expect(h.clock.timeMs).toBeCloseTo(1000);
  });

  it("plays backwards at a negative rate", () => {
    const h = harness();
    h.clock.seek(2000);
    h.clock.setRate(-1);
    h.clock.play();
    h.advance(500);
    expect(h.clock.timeMs).toBeCloseTo(1500);
  });
});

describe("PlaybackClock — transport", () => {
  it("defaults to realtime forward when played from rest", () => {
    const { clock } = harness();
    clock.play();
    expect(clock.status).toBe("playing");
    expect(clock.rate).toBe(1);
  });

  it("ignores play() while already playing", () => {
    const h = harness();
    h.clock.play();
    h.advance(100);
    h.clock.play();
    expect(h.clock.timeMs).toBeCloseTo(100);
    expect(h.pendingFrames()).toHaveLength(1);
  });

  it("pauses on the live position, snapped to a frame boundary", () => {
    const h = harness();
    h.clock.play();
    h.advance(110); // frame 3.3 at 30fps
    h.clock.pause();
    expect(h.clock.status).toBe("paused");
    expect(h.clock.timeMs).toBeCloseTo(100); // frame 3
  });

  it("cancels the frame loop on pause", () => {
    const h = harness();
    h.clock.play();
    expect(h.pendingFrames()).toHaveLength(1);
    h.clock.pause();
    expect(h.pendingFrames()).toHaveLength(0);
  });

  it("holds the paused position while time passes", () => {
    const h = harness();
    h.clock.play();
    h.advance(1000);
    h.clock.pause();
    const paused = h.clock.timeMs;
    h.advance(5000);
    expect(h.clock.timeMs).toBe(paused);
  });

  it("resumes from where it paused", () => {
    const h = harness();
    h.clock.play();
    h.advance(1000);
    h.clock.pause();
    h.advance(5000); // dead time while paused
    h.clock.play();
    h.advance(500);
    expect(h.clock.timeMs).toBeCloseTo(1500);
  });

  it("stop() rewinds to zero and drops the rate", () => {
    const h = harness();
    h.clock.play();
    h.advance(1000);
    h.clock.stop();
    expect(h.clock.status).toBe("stopped");
    expect(h.clock.timeMs).toBe(0);
    expect(h.clock.rate).toBe(0);
    expect(h.pendingFrames()).toHaveLength(0);
  });

  it("pauses at the end of the timeline", () => {
    const h = harness({ durationMs: 1000 });
    h.clock.play();
    h.advance(1200);
    // The imperative read clamps immediately …
    expect(h.clock.timeMs).toBe(1000);
    // … and the tick parks the transport there.
    h.runFrame();
    expect(h.clock.status).toBe("paused");
    expect(h.clock.timeMs).toBe(1000);
    expect(h.pendingFrames()).toHaveLength(0);
  });

  it("pauses at zero when playing backwards off the head", () => {
    const h = harness();
    h.clock.seek(300);
    h.clock.setRate(-1);
    h.clock.play();
    h.advance(500);
    expect(h.clock.timeMs).toBe(0);
    h.runFrame();
    expect(h.clock.status).toBe("paused");
    expect(h.clock.timeMs).toBe(0);
  });
});

describe("PlaybackClock — rate changes", () => {
  it("keeps the exact position across a shuttle step", () => {
    const h = harness();
    h.clock.play();
    h.advance(1010); // deliberately off a frame boundary
    h.clock.setRate(2);
    expect(h.clock.timeMs).toBeCloseTo(1010); // no frame snap, no nudge
    h.advance(500);
    expect(h.clock.timeMs).toBeCloseTo(2010);
  });

  it("keeps the loop alive across a rate change", () => {
    const h = harness();
    h.clock.play();
    h.clock.setRate(4);
    expect(h.clock.status).toBe("playing");
    expect(h.pendingFrames()).toHaveLength(1);
  });

  it("clamps the rate magnitude to the shuttle bound", () => {
    const { clock } = harness();
    clock.setRate(99);
    expect(clock.rate).toBe(MAX_CLOCK_RATE);
    clock.setRate(-99);
    expect(clock.rate).toBe(-MAX_CLOCK_RATE);
    clock.setRate(Number.NaN);
    expect(clock.rate).toBe(0);
  });

  it("rate 0 pauses on the live position", () => {
    const h = harness();
    h.clock.play();
    h.advance(1000);
    h.clock.setRate(0);
    expect(h.clock.status).toBe("paused");
    expect(h.clock.rate).toBe(0);
    // The elapsed second must not be swallowed by the rate going to zero.
    expect(h.clock.timeMs).toBeCloseTo(1000);
  });

  it("remembers a rate set while paused", () => {
    const h = harness();
    h.clock.setRate(-2);
    expect(h.clock.status).toBe("stopped");
    h.clock.seek(1000);
    h.clock.play();
    h.advance(250);
    expect(h.clock.timeMs).toBeCloseTo(500);
  });
});

describe("PlaybackClock — seeking", () => {
  it("clamps and frame-snaps the target", () => {
    const { clock } = harness();
    clock.seek(1010); // frame 30.3 at 30fps → frame 30
    expect(clock.timeMs).toBeCloseTo(1000);
    clock.seek(-500);
    expect(clock.timeMs).toBe(0);
    clock.seek(99_000);
    expect(clock.timeMs).toBe(10_000);
    clock.seek(Number.NaN);
    expect(clock.timeMs).toBe(0);
  });

  it("snaps against the project frame rate, not a fixed one", () => {
    const { clock } = harness({ fps: 25 });
    clock.seek(105); // frame 2.625 at 25fps → frame 3 → 120ms
    expect(clock.timeMs).toBeCloseTo(120);
  });

  it("keeps playing through a seek", () => {
    const h = harness();
    h.clock.play();
    h.advance(200);
    h.clock.seek(5000);
    expect(h.clock.status).toBe("playing");
    h.advance(500);
    expect(h.clock.timeMs).toBeCloseTo(5500);
  });

  it("marks a stopped clock paused once the playhead leaves the head", () => {
    const { clock } = harness();
    clock.seek(3000);
    expect(clock.status).toBe("paused");
    clock.stop();
    clock.seek(0);
    expect(clock.status).toBe("stopped");
  });

  it("holds the clock at the target for an opt-in held seek", () => {
    const h = harness();
    h.clock.play();
    h.clock.seek(2000, { hold: true });
    expect(h.clock.isSeeking).toBe(true);
    h.advance(300);
    h.runFrame();
    h.advance(300);
    expect(h.clock.timeMs).toBeCloseTo(2000); // frozen while held
    h.clock.completeSeek();
    expect(h.clock.isSeeking).toBe(false);
    h.advance(400);
    expect(h.clock.timeMs).toBeCloseTo(2400); // resumes without a jump
  });

  it("does not hold by default", () => {
    const h = harness();
    h.clock.play();
    h.clock.seek(2000);
    expect(h.clock.isSeeking).toBe(false);
    h.advance(100);
    expect(h.clock.timeMs).toBeCloseTo(2100);
  });

  it("keeps the loop alive while a seek is held", () => {
    const h = harness();
    h.clock.play();
    h.clock.seek(2000, { hold: true });
    h.advance(300);
    h.runFrame();
    expect(h.pendingFrames()).toHaveLength(1);
  });

  it("ignores completeSeek() when no seek is held", () => {
    const h = harness();
    h.clock.play();
    h.advance(100);
    h.clock.completeSeek();
    expect(h.clock.timeMs).toBeCloseTo(100);
  });

  it("clears a held seek on pause", () => {
    const h = harness();
    h.clock.play();
    h.clock.seek(2000, { hold: true });
    h.clock.pause();
    expect(h.clock.isSeeking).toBe(false);
  });
});

describe("PlaybackClock — generation guard against stale frames", () => {
  it("ignores a frame scheduled before a seek", () => {
    const h = harness();
    h.clock.play();
    h.advance(100);
    const [stale] = h.pendingFrames();
    h.clock.seek(5000);
    stale(); // the pre-seek frame finally fires
    expect(h.clock.timeMs).toBeCloseTo(5000);
    expect(h.pendingFrames()).toHaveLength(1); // no duplicate loop either
  });

  it("ignores a frame scheduled before a pause", () => {
    const h = harness();
    h.clock.play();
    h.advance(100);
    const [stale] = h.pendingFrames();
    h.clock.pause();
    h.advance(5000);
    stale();
    expect(h.clock.status).toBe("paused");
    expect(h.clock.timeMs).toBeCloseTo(100);
    expect(h.pendingFrames()).toHaveLength(0);
  });

  it("ignores a frame scheduled before a stop", () => {
    const h = harness();
    h.clock.play();
    h.advance(100);
    const [stale] = h.pendingFrames();
    h.clock.stop();
    stale();
    expect(h.clock.timeMs).toBe(0);
    expect(h.clock.status).toBe("stopped");
  });

  it("ignores a frame scheduled before a rate change", () => {
    const h = harness();
    h.clock.play();
    const [stale] = h.pendingFrames();
    h.clock.setRate(2);
    stale();
    // Exactly one live loop remains — the stale frame did not re-arm a second.
    expect(h.pendingFrames()).toHaveLength(1);
  });

  it("survives a burst of seeks during playback", () => {
    const h = harness();
    h.clock.play();
    const stale = h.pendingFrames();
    h.clock.seek(1000);
    h.clock.seek(2000);
    h.clock.seek(3000);
    for (const frame of stale) frame();
    expect(h.clock.timeMs).toBeCloseTo(3000);
    expect(h.pendingFrames()).toHaveLength(1);
  });

  it("lets the current frame keep the loop running", () => {
    const h = harness();
    h.clock.play();
    h.advance(16);
    h.runFrame();
    expect(h.pendingFrames()).toHaveLength(1);
    h.advance(16);
    h.runFrame();
    expect(h.clock.timeMs).toBeCloseTo(32);
  });
});

describe("PlaybackClock — stall compensation", () => {
  it("gives back the time a synchronous stall consumed", () => {
    const h = harness();
    h.clock.play();
    h.advance(100);
    h.clock.recordStallStart();
    h.advance(500); // 500ms blocked compiling/decoding
    h.clock.compensateStall();
    expect(h.clock.timeMs).toBeCloseTo(100);
  });

  it("keeps running normally after the compensation", () => {
    const h = harness();
    h.clock.play();
    h.advance(100);
    h.clock.recordStallStart();
    h.advance(500);
    h.clock.compensateStall();
    h.advance(50);
    expect(h.clock.timeMs).toBeCloseTo(150);
  });

  it("compensates on the wall basis too (audio not yet running)", () => {
    const h = harness();
    h.stopAudio();
    h.clock.play();
    h.advance(100);
    h.clock.recordStallStart();
    h.advance(500);
    h.clock.compensateStall();
    expect(h.clock.timeMs).toBeCloseTo(100);
  });

  it("survives the anchor switching basis across the stall", () => {
    const h = harness();
    h.stopAudio();
    h.clock.play();
    h.advance(100);
    h.clock.recordStallStart();
    h.advance(500);
    h.clock.compensateStall();
    // Audio comes up after the stall; both bases were pushed forward, so the
    // switch must not reintroduce the stall.
    h.startAudio();
    h.runFrame();
    expect(h.clock.timeMs).toBeCloseTo(100);
  });

  it("is a no-op without a recorded start", () => {
    const h = harness();
    h.clock.play();
    h.advance(300);
    h.clock.compensateStall();
    expect(h.clock.timeMs).toBeCloseTo(300);
  });

  it("does not record a stall while paused", () => {
    const h = harness();
    h.clock.recordStallStart();
    h.clock.play();
    h.advance(300);
    h.clock.compensateStall();
    expect(h.clock.timeMs).toBeCloseTo(300);
  });

  it("ignores a negative stall", () => {
    const h = harness();
    h.clock.play();
    h.advance(300);
    h.clock.recordStallStart();
    h.clock.compensateStall(); // zero-length stall
    expect(h.clock.timeMs).toBeCloseTo(300);
  });
});

describe("PlaybackClock — subscriptions", () => {
  it("notifies immediately on every transport change", () => {
    const h = harness();
    const listener = vi.fn();
    h.clock.subscribe(listener);
    h.clock.play();
    h.clock.pause();
    h.clock.seek(1000);
    h.clock.stop();
    expect(listener).toHaveBeenCalledTimes(4);
    expect(listener.mock.calls[0][0].status).toBe("playing");
  });

  it("throttles frame notifications to roughly 10 Hz", () => {
    const h = harness({ notifyIntervalMs: 100 });
    const listener = vi.fn();
    h.clock.play();
    h.clock.subscribe(listener);
    // Ten 16ms frames ≈ 160ms of playback → at most two notifications.
    for (let i = 0; i < 10; i += 1) {
      h.advance(16);
      h.runFrame();
    }
    expect(listener.mock.calls.length).toBeGreaterThan(0);
    expect(listener.mock.calls.length).toBeLessThanOrEqual(2);
  });

  it("hands subscribers a live snapshot", () => {
    const h = harness();
    let seen: PlaybackSnapshot | null = null;
    h.clock.subscribe((snapshot) => {
      seen = snapshot;
    });
    h.clock.play();
    h.advance(500);
    h.clock.pause();
    expect(seen).not.toBeNull();
    const snapshot = seen as unknown as PlaybackSnapshot;
    expect(snapshot.status).toBe("paused");
    expect(snapshot.timeMs).toBeCloseTo(500);
    expect(snapshot.fps).toBe(30);
  });

  it("stops notifying after unsubscribe", () => {
    const h = harness();
    const listener = vi.fn();
    const off = h.clock.subscribe(listener);
    h.clock.play();
    off();
    h.clock.pause();
    expect(listener).toHaveBeenCalledTimes(1);
  });

  it("survives a listener unsubscribing during notification", () => {
    const h = harness();
    const calls: string[] = [];
    const offA = h.clock.subscribe(() => {
      calls.push("a");
      offA();
    });
    h.clock.subscribe(() => calls.push("b"));
    expect(() => h.clock.play()).not.toThrow();
    expect(calls).toEqual(["a", "b"]);
  });
});

describe("PlaybackClock — disposal", () => {
  it("cancels the loop, closes the source and drops listeners", () => {
    const h = harness();
    const listener = vi.fn();
    h.clock.subscribe(listener);
    h.clock.play();
    listener.mockClear();

    h.clock.dispose();
    expect(h.pendingFrames()).toHaveLength(0);
    expect(h.closeCalls()).toBe(1);
    expect(h.clock.status).toBe("stopped");

    h.clock.play();
    expect(listener).not.toHaveBeenCalled();
    expect(h.clock.status).toBe("stopped");
  });

  it("is safe to dispose a clock that never played", () => {
    const h = harness();
    expect(() => h.clock.dispose()).not.toThrow();
    expect(h.closeCalls()).toBe(0); // no source was ever created
  });
});
