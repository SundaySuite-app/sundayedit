/**
 * Shared playhead store — the bridge between the Timeline's playback clock and
 * the clip inspector's "split at playhead" (rendered outside the Timeline).
 */
import { describe, it, expect } from "vitest";
import { renderHook, act } from "@testing-library/react";

import { publishPlayheadMs, usePlayheadMs } from "./playhead";

describe("playhead store", () => {
  it("publishes the timeline playhead to hook subscribers", () => {
    const { result } = renderHook(() => usePlayheadMs());
    act(() => publishPlayheadMs(1234));
    expect(result.current).toBe(1234);
    act(() => publishPlayheadMs(0));
    expect(result.current).toBe(0);
  });

  it("late subscribers read the last published position", () => {
    act(() => publishPlayheadMs(5000));
    const { result } = renderHook(() => usePlayheadMs());
    expect(result.current).toBe(5000);
  });

  it("unsubscribes on unmount — no further re-renders, no leaked listeners", () => {
    const { result, unmount } = renderHook(() => usePlayheadMs());
    act(() => publishPlayheadMs(100));
    expect(result.current).toBe(100);
    unmount();
    // Publishing after unmount must not throw (dead listeners are dropped).
    act(() => publishPlayheadMs(200));
  });
});
