/**
 * MediaBin — import flow, drag payload, remove-media error surfacing, and the
 * thumbnail upgrade path. Follows the ClipInspector.test.tsx pattern: mock the
 * lowest layer (Tauri invoke + dialog) so the real typed `ipc` wrappers run —
 * this pins the timeline-op command names + argument shapes the bin relies on.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, cleanup, screen, fireEvent } from "@testing-library/react";

// `tauriEnv` flips what `isTauri()` reports per-test: thumbnails only exist in
// a Tauri runtime (browser mode keeps the kind icon).
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

const openDialog = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (...args: unknown[]) => openDialog(...args),
}));

import { MediaBin, MEDIA_DND_MIME } from "./MediaBin";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import { useProjectStore } from "@/lib/useProjectStore";
import { useLocale } from "@/lib/i18n";
import type { MediaItem, Project } from "@/lib/bindings";

// The sample project's only media item (video, 18 s, 1920×1080).
const MEDIA = SAMPLE_PROJECT.media[0];

/** A media item with a fresh id — the thumbnail memo is keyed on media id and
 *  survives across tests (module-level cache), so cache-sensitive tests must
 *  use ids no other test has touched. */
let uniqueN = 0;
function freshMedia(extra?: Partial<MediaItem>): MediaItem {
  uniqueN += 1;
  return { ...MEDIA, id: `m-fresh-${uniqueN}`, ...extra };
}

function projectWith(...media: MediaItem[]): Project {
  return { ...SAMPLE_PROJECT, media };
}

beforeEach(() => {
  invoke.mockReset();
  openDialog.mockReset();
  tauriEnv = false;
  useLocale.setState({ lang: "en" });
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
  vi.restoreAllMocks();
});

function renderBin(project: Project = SAMPLE_PROJECT) {
  return render(<MediaBin project={project} />);
}

describe("MediaBin — empty state", () => {
  it("shows the empty hint when the pool has no media", () => {
    renderBin(projectWith());
    expect(
      screen.getByText("No media yet. Import a clip to place it on a track."),
    ).toBeTruthy();
    expect(screen.queryByTestId("remove-media")).toBeNull();
  });
});

describe("MediaBin — import flow", () => {
  it("imports the picked file via op_import_media onto the shared undo stack", async () => {
    const next: Project = { ...SAMPLE_PROJECT, updated_at: 1 };
    invoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "accepted_media_extensions") return ["mp4", "mov"];
      if (cmd === "op_import_media") return next;
      throw new Error(`unexpected command ${String(cmd)}`);
    });
    openDialog.mockResolvedValueOnce("/footage/take1.mp4");
    renderBin();

    fireEvent.click(screen.getByRole("button", { name: /Import media/ }));

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("op_import_media", {
        project: SAMPLE_PROJECT,
        path: "/footage/take1.mp4",
      }),
    );
    // The dialog filter used the backend's accepted extensions.
    expect(openDialog).toHaveBeenCalledWith({
      multiple: false,
      filters: [{ name: "Video & audio", extensions: ["mp4", "mov"] }],
    });
    // Committed through store.run: new project + undo snapshot.
    await vi.waitFor(() =>
      expect(useProjectStore.getState().project).toEqual(next),
    );
    expect(useProjectStore.getState().past).toEqual([SAMPLE_PROJECT]);
  });

  it("falls back to the built-in extension list when the backend probe fails", async () => {
    invoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "accepted_media_extensions")
        throw new Error("no backend in browser mode");
      return SAMPLE_PROJECT;
    });
    openDialog.mockResolvedValueOnce(null); // cancel — filters are the point
    renderBin();

    fireEvent.click(screen.getByRole("button", { name: /Import media/ }));

    await vi.waitFor(() => expect(openDialog).toHaveBeenCalledTimes(1));
    const args = openDialog.mock.calls[0][0] as {
      filters: Array<{ extensions: string[] }>;
    };
    // The fallback covers the common video + audio containers.
    expect(args.filters[0].extensions).toContain("mp4");
    expect(args.filters[0].extensions).toContain("wav");
    expect(args.filters[0].extensions).toContain("mkv");
  });

  it("does nothing when the dialog is cancelled", async () => {
    invoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "accepted_media_extensions") return ["mp4"];
      throw new Error(`unexpected command ${String(cmd)}`);
    });
    openDialog.mockResolvedValueOnce(null);
    renderBin();

    fireEvent.click(screen.getByRole("button", { name: /Import media/ }));

    await vi.waitFor(() => expect(openDialog).toHaveBeenCalledTimes(1));
    expect(invoke).not.toHaveBeenCalledWith(
      "op_import_media",
      expect.anything(),
    );
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("surfaces an import failure in the alert strip", async () => {
    invoke.mockImplementation(async (cmd: unknown) => {
      if (cmd === "accepted_media_extensions") return ["mp4"];
      if (cmd === "op_import_media")
        throw { code: "ffmpeg_failed", message: "could not probe file" };
      throw new Error(`unexpected command ${String(cmd)}`);
    });
    openDialog.mockResolvedValueOnce("/footage/broken.mp4");
    renderBin();

    fireEvent.click(screen.getByRole("button", { name: /Import media/ }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("Import failed: could not probe file");
    // The failed op left the store untouched (run rethrows without committing).
    expect(useProjectStore.getState().project).toEqual(SAMPLE_PROJECT);
    expect(useProjectStore.getState().past).toEqual([]);
  });
});

describe("MediaBin — drag payload", () => {
  it("puts the media id under MEDIA_DND_MIME on dragstart", () => {
    renderBin();
    const row = screen.getByTitle(MEDIA.path);

    const setData = vi.fn();
    const dataTransfer = { setData, effectAllowed: "" };
    fireEvent.dragStart(row, { dataTransfer });

    expect(setData).toHaveBeenCalledWith(MEDIA_DND_MIME, MEDIA.id);
    expect(dataTransfer.effectAllowed).toBe("copy");
  });
});

describe("MediaBin — remove media", () => {
  it("commits op_remove_media for the row's media id", async () => {
    const next: Project = { ...SAMPLE_PROJECT, media: [], updated_at: 2 };
    invoke.mockResolvedValueOnce(next);
    renderBin();

    fireEvent.click(screen.getByTestId("remove-media"));

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("op_remove_media", {
        project: SAMPLE_PROJECT,
        mediaId: MEDIA.id,
      }),
    );
    await vi.waitFor(() =>
      expect(useProjectStore.getState().project).toEqual(next),
    );
  });

  it("surfaces the backend rejection when the media is still referenced", async () => {
    invoke.mockRejectedValueOnce({
      code: "invalid_input",
      message: "media is referenced by 1 timeline item",
    });
    renderBin();

    fireEvent.click(screen.getByTestId("remove-media"));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("media is referenced by 1 timeline item");
    expect(useProjectStore.getState().project).toEqual(SAMPLE_PROJECT);
  });
});

describe("MediaBin — add track", () => {
  it.each([
    ["Video track", "video"],
    ["Audio track", "audio"],
    ["Overlay track", "overlay"],
  ] as const)("'%s' commits op_add_track kind=%s", async (label, kind) => {
    invoke.mockResolvedValueOnce({ ...SAMPLE_PROJECT, updated_at: 3 });
    renderBin();

    fireEvent.click(screen.getByRole("button", { name: label }));

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("op_add_track", {
        project: SAMPLE_PROJECT,
        kind,
        name: label,
      }),
    );
  });
});

describe("MediaBin — thumbnails", () => {
  it("upgrades the kind icon to a thumbnail img when extract_thumbnail resolves (Tauri)", async () => {
    tauriEnv = true;
    const media = freshMedia();
    invoke.mockImplementation(async (cmd: unknown, args: unknown) => {
      expect(cmd).toBe("extract_thumbnail");
      const a = args as { mediaPath: string; atMs: number; outPath: string };
      expect(a.mediaPath).toBe(media.path);
      // 10% into an 18 s clip, capped at 5 s.
      expect(a.atMs).toBe(1800);
      expect(a.outPath).toBe(`/cache/thumbnails/${media.id}.jpg`);
      return a.outPath;
    });
    renderBin(projectWith(media));

    const img = await vi.waitFor(() => {
      const el = screen
        .getByTitle(media.path)
        .querySelector("img") as HTMLImageElement | null;
      expect(el).not.toBeNull();
      return el as HTMLImageElement;
    });
    expect(img.src).toBe(`asset://localhost//cache/thumbnails/${media.id}.jpg`);
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("keeps the icon (never calls extract_thumbnail) for audio-only media", async () => {
    tauriEnv = true;
    const media = freshMedia({ kind: "audio_only", width: 0, height: 0 });
    renderBin(projectWith(media));

    // Give any stray thumbnail promise a chance to run.
    await new Promise((r) => setTimeout(r, 0));
    expect(invoke).not.toHaveBeenCalled();
    expect(screen.getByTitle(media.path).querySelector("img")).toBeNull();
    expect(screen.getByTitle(media.path).querySelector("svg")).not.toBeNull();
  });

  it("keeps the icon in browser mode (isTauri false) without touching IPC", async () => {
    const media = freshMedia();
    renderBin(projectWith(media));

    await new Promise((r) => setTimeout(r, 0));
    expect(invoke).not.toHaveBeenCalled();
    expect(screen.getByTitle(media.path).querySelector("img")).toBeNull();
  });

  it("falls back to the icon when the extraction fails, without re-spawning ffmpeg", async () => {
    tauriEnv = true;
    const media = freshMedia();
    invoke.mockRejectedValue(new Error("ffmpeg exploded"));
    const { unmount } = renderBin(projectWith(media));

    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    expect(screen.getByTitle(media.path).querySelector("img")).toBeNull();

    // A remount reuses the memoized failure — no second ffmpeg run.
    unmount();
    renderBin(projectWith(media));
    await new Promise((r) => setTimeout(r, 0));
    expect(invoke).toHaveBeenCalledTimes(1);
  });
});
