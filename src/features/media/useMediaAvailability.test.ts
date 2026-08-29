/**
 * useMediaAvailability — the pool-fingerprint dependency (re-check on
 * import/relink, NOT on every unrelated project edit) and the off-Tauri /
 * no-media / failure degradations. Mocks the lowest layer (Tauri invoke) so
 * the real `ipc.project.checkMediaPaths` wrapper runs.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";

const invoke = vi.fn();
let tauriEnv = true;
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
  isTauri: () => tauriEnv,
}));

import { useMediaAvailability } from "./useMediaAvailability";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import type { MediaAvailability, Project } from "@/lib/bindings";

const MEDIA = SAMPLE_PROJECT.media[0];

beforeEach(() => {
  invoke.mockReset();
  tauriEnv = true;
});

describe("useMediaAvailability", () => {
  it("skips the check and reports nothing missing off-Tauri", async () => {
    tauriEnv = false;
    const { result } = renderHook(() => useMediaAvailability(SAMPLE_PROJECT));

    await new Promise((r) => setTimeout(r, 0));
    expect(invoke).not.toHaveBeenCalled();
    expect(result.current.missingIds.size).toBe(0);
  });

  it("skips the check for a project with no media", async () => {
    const empty: Project = { ...SAMPLE_PROJECT, media: [] };
    renderHook(() => useMediaAvailability(empty));

    await new Promise((r) => setTimeout(r, 0));
    expect(invoke).not.toHaveBeenCalled();
  });

  it("reports the ids check_media_paths flags as missing", async () => {
    const rows: MediaAvailability[] = [
      { media_id: MEDIA.id, path: MEDIA.path, exists: false },
    ];
    invoke.mockResolvedValue(rows);
    const { result } = renderHook(() => useMediaAvailability(SAMPLE_PROJECT));

    await waitFor(() =>
      expect(result.current.missingIds.has(MEDIA.id)).toBe(true),
    );
    expect(result.current.availability).toEqual(rows);
  });

  it("degrades to nothing-missing when the backend call rejects", async () => {
    invoke.mockRejectedValue(new Error("no tauri runtime"));
    const { result } = renderHook(() => useMediaAvailability(SAMPLE_PROJECT));

    await new Promise((r) => setTimeout(r, 0));
    expect(result.current.missingIds.size).toBe(0);
  });

  it("re-checks when the pool's paths change but NOT for an unrelated edit", async () => {
    invoke.mockResolvedValue([]);
    let project = SAMPLE_PROJECT;
    const { result, rerender } = renderHook(() =>
      useMediaAvailability(project),
    );

    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));

    // An unrelated edit (every Rust op returns a brand-new Project object,
    // even for a caption-only change) must NOT re-trigger the check.
    project = { ...project, name: "renamed.mp4" };
    act(() => rerender());
    await new Promise((r) => setTimeout(r, 0));
    expect(invoke).toHaveBeenCalledTimes(1);

    // A relink (the pool's path changes) DOES re-trigger it.
    project = {
      ...project,
      media: [{ ...MEDIA, path: "/new/path.mp4" }],
    };
    act(() => rerender());
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
    expect(result.current).toBeTruthy();
  });
});
