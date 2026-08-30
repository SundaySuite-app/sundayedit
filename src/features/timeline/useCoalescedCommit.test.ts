import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { renderHook, act } from "@testing-library/react";

import { useCoalescedCommit } from "./useCoalescedCommit";
import { useProjectStore } from "@/lib/useProjectStore";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import type { Project } from "@/lib/bindings";

function store() {
  return useProjectStore.getState();
}

beforeEach(() => {
  useProjectStore.setState({
    project: null,
    past: [],
    future: [],
    busy: false,
    inFlight: false,
    savedSnapshot: null,
    filePath: null,
    saving: false,
  });
  store().reset(SAMPLE_PROJECT);
});

afterEach(() => {
  useProjectStore.setState({ project: null, past: [], future: [] });
});

/** Flush the microtask queue so an already-settled promise's `.then` runs. */
async function flush() {
  await Promise.resolve();
  await Promise.resolve();
}

describe("useCoalescedCommit", () => {
  it("commits the round-tripped result and pushes ONE undo entry", async () => {
    const { result } = renderHook(() => useCoalescedCommit());
    const next: Project = { ...SAMPLE_PROJECT, updated_at: 1 };

    act(() => result.current("gain:i1", async () => next));
    await act(flush);

    expect(store().project).toBe(next);
    expect(store().past).toEqual([SAMPLE_PROJECT]);
  });

  it("folds a burst of same-key commits into ONE undo entry, not one per tick", async () => {
    const { result } = renderHook(() => useCoalescedCommit());
    const tick1: Project = { ...SAMPLE_PROJECT, updated_at: 1 };
    const tick2: Project = { ...SAMPLE_PROJECT, updated_at: 2 };
    const tick3: Project = { ...SAMPLE_PROJECT, updated_at: 3 };

    act(() => {
      result.current("gain:i1", async () => tick1);
      result.current("gain:i1", async () => tick2);
      result.current("gain:i1", async () => tick3);
    });
    await act(flush);

    // The release value lands — but as ONE undo step whose "before" is the
    // state from BEFORE the whole burst, same as a dragged panel slider.
    expect(store().project).toBe(tick3);
    expect(store().past).toEqual([SAMPLE_PROJECT]);
  });

  it("keeps distinct keys on separate undo entries (gain vs. fade-in)", async () => {
    const { result } = renderHook(() => useCoalescedCommit());
    const gained: Project = { ...SAMPLE_PROJECT, updated_at: 1 };
    const faded: Project = { ...SAMPLE_PROJECT, updated_at: 2 };

    act(() => result.current("gain:i1", async () => gained));
    await act(flush);
    act(() => result.current("fadeIn:i1", async () => faded));
    await act(flush);

    expect(store().project).toBe(faded);
    expect(store().past).toEqual([SAMPLE_PROJECT, gained]);
  });

  it("drops a stale response even when it resolves AFTER a later tick", async () => {
    const { result } = renderHook(() => useCoalescedCommit());
    let resolveFirst: ((p: Project) => void) | undefined;
    const stale: Project = { ...SAMPLE_PROJECT, updated_at: 1 };
    const released: Project = { ...SAMPLE_PROJECT, updated_at: 2 };

    act(() => {
      // Tick 1: held open (simulates an in-flight IPC round-trip).
      result.current(
        "gain:i1",
        () => new Promise<Project>((resolve) => (resolveFirst = resolve)),
      );
      // Tick 2 (pointer release): resolves immediately.
      result.current("gain:i1", async () => released);
    });
    await act(flush);
    expect(store().project).toBe(released);

    // Tick 1 finally resolves — its stale result must NOT clobber tick 2's.
    await act(async () => {
      resolveFirst?.(stale);
      await flush();
    });
    expect(store().project).toBe(released);
    expect(store().past).toEqual([SAMPLE_PROJECT]);
  });

  it("is a no-op without an open project", async () => {
    useProjectStore.setState({ project: null, past: [], future: [] });
    const { result } = renderHook(() => useCoalescedCommit());

    act(() => result.current("gain:i1", async () => SAMPLE_PROJECT));
    await act(flush);

    expect(store().project).toBeNull();
    expect(store().past).toEqual([]);
  });

  it("leaves the project untouched when the backend rejects (clamped op)", async () => {
    const { result } = renderHook(() => useCoalescedCommit());

    act(() =>
      result.current("gain:i1", () => Promise.reject(new Error("rejected"))),
    );
    await act(flush);

    expect(store().project).toBe(SAMPLE_PROJECT);
    expect(store().past).toEqual([]);
  });
});
