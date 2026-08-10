/**
 * Timeline — render-efficiency guard.
 *
 * The playback clock advances `playheadMs` (Timeline state) on every animation
 * frame, so the Timeline function re-runs ~60×/s while playing. The lane
 * subtrees (LaneStack/LaneHeaders/RulerBar) are React.memo'd on playhead-
 * independent props precisely so that per-tick churn reconciles ZERO lane
 * nodes — only the toolbar timecode, the playhead line and the player update.
 *
 * Probe: `useThumbnail` is called by every ClipBox render, so its call count
 * is a direct render counter for the clip-lane subtree. If someone re-inlines
 * the lanes into Timeline's own JSX (or breaks a useCallback/useMemo identity
 * this memoization depends on), the count climbs with the frames and this
 * test fails.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, cleanup, act, fireEvent } from "@testing-library/react";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
  isTauri: () => false,
}));
vi.mock("@tauri-apps/api/path", () => ({
  appCacheDir: async () => "/cache",
  join: async (...parts: string[]) => parts.join("/"),
}));
vi.mock("@/lib/composeEngine", () => ({
  renderPreviewProxy: vi.fn(async () => true),
}));
// The render-count probe: ClipBox calls useThumbnail on every render.
const useThumbnail = vi.fn(() => null);
vi.mock("@/features/media/thumbnails", () => ({
  useThumbnail: (...args: unknown[]) =>
    (useThumbnail as (...a: unknown[]) => null)(...args),
}));

import { Timeline } from "./Timeline";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import { useProjectStore } from "@/lib/useProjectStore";
import { useLocale } from "@/lib/i18n";

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

// Capture rAF callbacks so the test drives the playback clock frame by frame.
let rafQueue: FrameRequestCallback[] = [];
// The PlaybackClock reads its position from a monotonic clock (an AudioContext
// in the app, `performance.now()` in jsdom) rather than from rAF deltas — so
// driving frames alone would not move it. Owning `performance.now()` here makes
// "10 frames, 16 ms apart" mean exactly 160 ms of playback, deterministically.
let clockNowMs = 0;

beforeEach(() => {
  invoke.mockReset();
  invoke.mockImplementation(() =>
    Promise.reject(new Error("no tauri runtime under vitest")),
  );
  useThumbnail.mockClear();
  rafQueue = [];
  clockNowMs = 0;
  vi.spyOn(performance, "now").mockImplementation(() => clockNowMs);
  vi.stubGlobal("ResizeObserver", ResizeObserverStub);
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb) => {
    rafQueue.push(cb);
    return rafQueue.length;
  });
  vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
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

/** Advance the clock to `now` and run every queued animation frame once. */
function runFrames(now: number) {
  const cbs = rafQueue;
  rafQueue = [];
  clockNowMs = now;
  act(() => {
    for (const cb of cbs) cb(now);
  });
}

describe("Timeline — lanes do not re-render on playback ticks", () => {
  it("advances the playhead across frames without re-rendering a single clip box", () => {
    const { container } = render(<Timeline project={SAMPLE_PROJECT} />);
    const surface = container.firstElementChild as HTMLElement;
    const playheadLine = () =>
      container.querySelector('[class*="bg-white/90"]') as HTMLElement;
    expect(playheadLine().style.left).toBe("0px");
    expect(useThumbnail).toHaveBeenCalled(); // the clip lane rendered at all

    // Start forward playback (L) — the clock effect registers its first frame.
    act(() => {
      fireEvent.keyDown(surface, { key: "l" });
    });
    const rendersBeforePlayback = useThumbnail.mock.calls.length;

    // Drive 10 frames of the playback clock, 16 ms apart. The clock is read
    // (not accumulated) each frame, so after 160 ms the playhead stands at
    // 160 ms → 160 × 0.05 px/ms = 8 px.
    for (let frame = 1; frame <= 10; frame++) runFrames(frame * 16);

    // The playhead genuinely moved (so Timeline DID re-render per tick)…
    expect(parseFloat(playheadLine().style.left)).toBeCloseTo(8, 5);
    // …but the memoized lane subtree bailed out on every one of those renders:
    // not a single additional ClipBox render across 10 playback frames.
    expect(useThumbnail.mock.calls.length).toBe(rendersBeforePlayback);
  });
});
