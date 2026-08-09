/**
 * useVideoImport — drag-drop subscription lifecycle.
 *
 * Regression (state-usevideoimport-unlisten-race-leak): the effect subscribes
 * via `webview.onDragDropEvent(handler).then((fn) => { unlisten = fn })`, and
 * the cleanup used to call only `unlisten?.()`. When cleanup ran before the
 * registration promise resolved (unmount / `enabled` flip within the IPC
 * round-trip; guaranteed on the StrictMode dev double-mount enabled in
 * main.tsx), the resolved unlisten was stored into a variable nobody read
 * again — the webview listener leaked with a stale closure that still called
 * importPath → onReady. A `disposed` flag (mirroring `subscribeComposeProgress`
 * in composeEngine.ts) now closes the race.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";

// --- controllable webview mock -------------------------------------------

type DragHandler = (event: {
  payload: { type: string; paths?: string[] };
}) => void;

const handlers: DragHandler[] = [];
const unlistenSpies: Array<ReturnType<typeof vi.fn>> = [];
const resolveRegistration: Array<() => void> = [];

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: (handler: DragHandler) => {
      handlers.push(handler);
      const unlisten = vi.fn();
      unlistenSpies.push(unlisten);
      // The real API resolves after an IPC round-trip; we resolve on demand
      // so the test controls whether cleanup wins the race.
      return new Promise<() => void>((resolve) => {
        resolveRegistration.push(() => resolve(unlisten));
      });
    },
  }),
}));

vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
}));

const createFromVideo = vi.fn(async (path: string) => ({ id: "p1", path }));

vi.mock("@/lib/ipc", () => ({
  ipc: {
    project: {
      createFromVideo: (path: string) => createFromVideo(path),
    },
  },
  IPCError: class IPCError extends Error {
    code = "unknown";
  },
  project: {
    acceptedExtensions: vi.fn(async () => ["mp4"]),
  },
}));

import { useVideoImport } from "@/lib/useVideoImport";

// --- helpers ---------------------------------------------------------------

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

beforeEach(() => {
  handlers.length = 0;
  unlistenSpies.length = 0;
  resolveRegistration.length = 0;
  createFromVideo.mockClear();
});

describe("useVideoImport — unlisten registration race", () => {
  it("tears down the drag-drop listener when unmount wins the registration round-trip", async () => {
    const onReady = vi.fn();
    const { unmount } = renderHook(() => useVideoImport(onReady));

    // The handler was handed to onDragDropEvent synchronously in the effect…
    expect(handlers).toHaveLength(1);

    // …but the component unmounts before the registration promise resolves
    // (real IPC round-trip; StrictMode's dev double-mount does this always).
    unmount();

    // Registration finally resolves — after cleanup already ran.
    resolveRegistration[0]();
    await flush();

    // The subscription must still be torn down.
    expect(unlistenSpies[0]).toHaveBeenCalled();
  });

  it("does not let a leaked listener import files after `enabled` flipped off", async () => {
    const onReady = vi.fn();
    const { rerender } = renderHook(
      ({ enabled }) => useVideoImport(onReady, { enabled }),
      { initialProps: { enabled: true } },
    );
    expect(handlers).toHaveLength(1);

    // Screen moves past the drop step within the registration round-trip.
    rerender({ enabled: false });
    resolveRegistration[0]();
    await flush();

    // A file dropped on the window later must NOT reach the dead screen.
    await act(async () => {
      handlers[0]({ payload: { type: "drop", paths: ["/user/other.mp4"] } });
      await Promise.resolve();
    });
    await flush();

    expect(createFromVideo).not.toHaveBeenCalled();
    expect(onReady).not.toHaveBeenCalled();
  });
});
