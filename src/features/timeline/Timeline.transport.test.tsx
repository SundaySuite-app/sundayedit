/**
 * Timeline transport — the `PlaybackClock` wiring (programme stage E2).
 *
 * The playhead used to be accumulated inside a rAF loop
 * (`setPlayheadMs(p => p + dt * rate)`). It is now READ from a monotonic clock
 * every frame, which is a different failure profile: dropped frames cost
 * nothing, and the position after N ms of wall clock is N × rate no matter how
 * many frames were actually delivered. These tests own that contract, plus the
 * outward one that must NOT have changed — J/K/L shuttle, Space, ruler seeks,
 * and `playhead.ts` still publishing to the external store.
 *
 * Time is owned by the test: `performance.now()` is the clock the jsdom
 * PlaybackClock rides (there is no AudioContext here), and rAF callbacks are
 * queued rather than free-running, so "frames" and "elapsed time" are two
 * independent dials the tests turn separately.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, cleanup, act, fireEvent } from "@testing-library/react";

const invoke = vi.fn();
let tauriEnv = false;
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
  isTauri: () => tauriEnv,
}));
vi.mock("@tauri-apps/api/path", () => ({
  appCacheDir: async () => "/cache",
  join: async (...parts: string[]) => parts.join("/"),
}));
vi.mock("@/lib/composeEngine", () => ({
  renderPreviewProxy: vi.fn(async () => true),
}));

import { Timeline } from "./Timeline";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import { getPlayheadMs } from "./playhead";
import { useProjectStore } from "@/lib/useProjectStore";
import { useLocale } from "@/lib/i18n";

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

let rafQueue: FrameRequestCallback[] = [];
let clockNowMs = 0;

beforeEach(() => {
  invoke.mockReset();
  invoke.mockImplementation(() =>
    Promise.reject(new Error("no tauri runtime under vitest")),
  );
  tauriEnv = false;
  rafQueue = [];
  clockNowMs = 0;
  vi.spyOn(performance, "now").mockImplementation(() => clockNowMs);
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb) => {
    rafQueue.push(cb);
    return rafQueue.length;
  });
  vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
  vi.stubGlobal("ResizeObserver", ResizeObserverStub);
  act(() => {
    useLocale.setState({ lang: "en" });
  });
  useProjectStore.setState({
    project: SAMPLE_PROJECT,
    past: [],
    future: [],
    busy: false,
    inFlight: false,
  });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

/** Advance wall-clock time to `now` and deliver one round of frames. */
function frameAt(now: number) {
  const cbs = rafQueue;
  rafQueue = [];
  clockNowMs = now;
  act(() => {
    for (const cb of cbs) cb(now);
  });
}

/** The initial view is 0.05 px/ms, so the playhead line's x IS ms × 0.05. */
const PX_PER_MS = 0.05;

function mount() {
  const utils = render(<Timeline project={SAMPLE_PROJECT} />);
  const surface = utils.container.firstElementChild as HTMLElement;
  const playheadX = () => {
    const line = utils.container.querySelector(
      '[class*="bg-white/90"]',
    ) as HTMLElement | null;
    return line ? parseFloat(line.style.left) : Number.NaN;
  };
  return {
    ...utils,
    surface,
    playheadX,
    playheadMs: () => playheadX() / PX_PER_MS,
  };
}

describe("Timeline transport — the clock is read, not accumulated", () => {
  it("advances the playhead by elapsed time × rate, not by frame count", () => {
    const { surface, playheadMs } = mount();
    fireEvent.keyDown(surface, { key: " " }); // play at 1×
    frameAt(500);
    expect(playheadMs()).toBeCloseTo(500, 5);
    frameAt(1500);
    expect(playheadMs()).toBeCloseTo(1500, 5);
  });

  it("loses no time across a stall that swallows frames", () => {
    // ONE frame delivered after a 2 s gap (a decode stall, a throttled tab, a
    // long synchronous layout). The old rAF accumulator could only add what it
    // was told about; a clock that is read knows where it really is.
    const { surface, playheadMs } = mount();
    fireEvent.keyDown(surface, { key: " " });
    frameAt(2000);
    expect(playheadMs()).toBeCloseTo(2000, 5);

    // …and 60 frames covering the SAME wall-clock span land in the same place.
    for (let i = 1; i <= 60; i++) frameAt(2000 + i * 16);
    expect(playheadMs()).toBeCloseTo(2960, 5);
  });

  it("publishes the playhead to the shared external store", () => {
    const { surface } = mount();
    fireEvent.keyDown(surface, { key: " " });
    frameAt(1000);
    expect(getPlayheadMs()).toBeCloseTo(1000, 5);
  });

  it("stops cleanly at the end of the timeline", () => {
    const { surface, queryByText } = mount();
    fireEvent.keyDown(surface, { key: "l" });
    fireEvent.keyDown(surface, { key: "l" }); // 2× forward
    expect(queryByText("2× ▸")).not.toBeNull();
    // SAMPLE_PROJECT is 18 s long; 2× for 10 s overshoots it. (18 s is off the
    // right edge of the 800 px viewport, so read the published playhead rather
    // than the line's x.)
    frameAt(10_000);
    expect(getPlayheadMs()).toBeCloseTo(18_000, 5);
    // The transport reads "stopped" again — no shuttle badge, no rate.
    expect(queryByText("2× ▸")).toBeNull();
  });

  it("plays in reverse and stops at zero", () => {
    const { surface, playheadMs, container, queryByText } = mount();
    // Park at 4000 ms first (ruler click: x=200 px at 0.05 px/ms).
    fireEvent.pointerDown(container.querySelector("canvas")!, { clientX: 200 });
    expect(playheadMs()).toBeCloseTo(4000, 5);

    fireEvent.keyDown(surface, { key: "j" }); // −1×
    expect(queryByText("◂ 1×")).not.toBeNull();
    frameAt(1000);
    expect(playheadMs()).toBeCloseTo(3000, 5);
    frameAt(5000);
    expect(playheadMs()).toBeCloseTo(0, 5);
    expect(queryByText("◂ 1×")).toBeNull();
  });
});

describe("Timeline transport — J/K/L and Space", () => {
  it("doubles on repeated taps, reverses on J, and stops on K", () => {
    const { surface, queryByText } = mount();
    fireEvent.keyDown(surface, { key: "l" });
    expect(queryByText("2× ▸")).toBeNull(); // 1× shows no badge
    fireEvent.keyDown(surface, { key: "l" });
    expect(queryByText("2× ▸")).not.toBeNull();
    fireEvent.keyDown(surface, { key: "l" });
    expect(queryByText("4× ▸")).not.toBeNull();
    fireEvent.keyDown(surface, { key: "j" });
    expect(queryByText("◂ 1×")).not.toBeNull();
    fireEvent.keyDown(surface, { key: "k" });
    expect(queryByText("◂ 1×")).toBeNull();
  });

  it("Space stops a shuttle run and restarts it at 1× forward", () => {
    const { surface, queryByText, playheadMs } = mount();
    fireEvent.keyDown(surface, { key: "l" });
    fireEvent.keyDown(surface, { key: "l" }); // 2×
    fireEvent.keyDown(surface, { key: " " }); // stop
    expect(queryByText("2× ▸")).toBeNull();

    fireEvent.keyDown(surface, { key: " " }); // roll again — forward realtime
    frameAt(1000);
    expect(playheadMs()).toBeCloseTo(1000, 5);
    expect(queryByText("2× ▸")).toBeNull(); // 1×, not the old 2×
  });

  it("parks the playhead on a frame boundary when playback pauses", () => {
    // 1010 ms is mid-frame at 30 fps (frame 30 starts at 1000 ms). A paused
    // preview must show a frame that exists, so the clock snaps on pause —
    // onto the same integer-ms grid `geometry.snapToFrame` uses everywhere else.
    const { surface, playheadMs } = mount();
    fireEvent.keyDown(surface, { key: " " });
    frameAt(1010);
    expect(playheadMs()).toBeCloseTo(1010, 5);
    fireEvent.keyDown(surface, { key: " " }); // pause
    expect(playheadMs()).toBe(1000);
  });

  it("seeks to a frame-snapped position on a ruler click", () => {
    const { container, playheadMs } = mount();
    // x = 101 px → 2020 ms → frame 61 (2033⅓ ms) → 2033 on the integer grid.
    fireEvent.pointerDown(container.querySelector("canvas")!, { clientX: 101 });
    expect(playheadMs()).toBe(2033);
  });
});

describe("Timeline transport — frame skipping above 1×", () => {
  it("publishes only every Nth frame while shuttling, but never delays a stop", () => {
    const { surface, playheadX, playheadMs, queryByText } = mount();
    fireEvent.keyDown(surface, { key: "l" });
    fireEvent.keyDown(surface, { key: "l" });
    fireEvent.keyDown(surface, { key: "l" }); // 4× → stride 4
    expect(playheadX()).toBe(0);

    // Frames 1–3 of the run are skipped: the clock advanced, the UI did not.
    frameAt(16);
    frameAt(32);
    frameAt(48);
    expect(playheadX()).toBe(0);

    // Frame 4 publishes the clock's true position — 48+16 ms at 4×.
    frameAt(64);
    expect(playheadMs()).toBeCloseTo(256, 5);

    // A transport change is never skipped, whatever the stride counter stands
    // at: K stops here and the playhead lands immediately.
    frameAt(80);
    fireEvent.keyDown(surface, { key: "k" });
    expect(queryByText("4× ▸")).toBeNull();
    // 80 ms × 4 = 320 ms, snapped to frame 10 (333⅓ ms → 333 on the grid).
    expect(playheadMs()).toBe(333);
  });

  it("shows a seek immediately even mid-stride", () => {
    // A seek is the one transport change that leaves the rate alone, so it has
    // to clear the skip counter itself — a ruler click that appears to do
    // nothing for three frames reads as a dropped input.
    const { surface, container, playheadMs } = mount();
    fireEvent.keyDown(surface, { key: "l" });
    fireEvent.keyDown(surface, { key: "l" });
    fireEvent.keyDown(surface, { key: "l" }); // 4× → stride 4
    frameAt(16); // one frame into the stride, and skipped

    // x = 100 px at 0.05 px/ms → 2000 ms.
    fireEvent.pointerDown(container.querySelector("canvas")!, { clientX: 100 });
    expect(playheadMs()).toBe(2000);
  });

  it("renders every frame at realtime — no skipping below the shuttle band", () => {
    const { surface, playheadMs } = mount();
    fireEvent.keyDown(surface, { key: " " });
    for (const t of [16, 32, 48]) {
      frameAt(t);
      expect(playheadMs()).toBeCloseTo(t, 5);
    }
  });
});
