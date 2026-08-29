/**
 * relink.ts — the auto-search → manual-pick → commit flow, in isolation from
 * any component. Mocks the typed `@/lib/ipc` wrappers directly (rather than
 * the raw `invoke`, as `MediaBin.test.tsx` does) since the point here is the
 * flow's BRANCHING — auto-found / falls back to the dialog / cancelled /
 * backend rejects — not the command names, which the invoke-level tests
 * already pin.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const isTauriMock = vi.fn(() => true);
vi.mock("@tauri-apps/api/core", () => ({
  isTauri: () => isTauriMock(),
}));

const openDialog = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (...args: unknown[]) => openDialog(...args),
}));

vi.mock("@tauri-apps/api/path", () => ({
  dirname: async (p: string) => p.split("/").slice(0, -1).join("/") || "/",
  videoDir: async () => "/Movies",
  desktopDir: async () => "/Desktop",
  downloadDir: async () => "/Downloads",
  homeDir: async () => "/Home",
}));

const relinkProbe = vi.fn();
const acceptedExtensions = vi.fn(async () => ["mp4", "mov"]);
const relinkOp = vi.fn();
vi.mock("@/lib/ipc", () => ({
  project: {
    relink: (...args: unknown[]) => relinkProbe(...args),
    acceptedExtensions: () => acceptedExtensions(),
  },
  ipc: {
    timeline: {
      relinkMedia: (...args: unknown[]) => relinkOp(...args),
    },
  },
}));

import { candidateSearchDirs, relinkMedia } from "./relink";
import type { MediaItem, Project } from "@/lib/bindings";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";

const MEDIA: MediaItem = {
  ...SAMPLE_PROJECT.media[0],
  path: "/old/gone.mp4",
};

function projectWithMedia(m: MediaItem): Project {
  return { ...SAMPLE_PROJECT, media: [m] };
}

beforeEach(() => {
  isTauriMock.mockReturnValue(true);
  openDialog.mockReset();
  relinkProbe.mockReset();
  acceptedExtensions.mockClear();
  relinkOp.mockReset();
});

describe("candidateSearchDirs", () => {
  it("includes the old file's parent dir plus the usual media homes, deduped", async () => {
    const dirs = await candidateSearchDirs("/old/footage/gone.mp4");
    expect(dirs).toContain("/old/footage");
    expect(dirs).toContain("/Movies");
    expect(dirs).toContain("/Desktop");
    expect(dirs).toContain("/Downloads");
    expect(dirs).toContain("/Home");
    expect(new Set(dirs).size).toBe(dirs.length); // no duplicates
  });
});

describe("relinkMedia", () => {
  function makeRun(project: Project) {
    return vi.fn(async (op: (p: Project) => Promise<Project>) => {
      await op(project);
    });
  }

  it("auto-search: commits the found path without ever opening a dialog", async () => {
    const project = projectWithMedia(MEDIA);
    relinkProbe.mockResolvedValue("/new/gone.mp4");
    const relinked = { ...MEDIA, path: "/new/gone.mp4" };
    relinkOp.mockResolvedValue({ ...project, media: [relinked] });
    const run = makeRun(project);
    const phases: string[] = [];

    const outcome = await relinkMedia({
      media: MEDIA,
      run,
      filterName: "Video & audio",
      onPhase: (p) => phases.push(p),
    });

    expect(outcome).toEqual({ kind: "auto", durationChanged: false });
    expect(openDialog).not.toHaveBeenCalled();
    expect(relinkOp).toHaveBeenCalledWith(project, MEDIA.id, "/new/gone.mp4");
    expect(phases).toEqual(["searching", "linking"]);
  });

  it("falls back to the file dialog when auto-search finds nothing", async () => {
    const project = projectWithMedia(MEDIA);
    relinkProbe.mockResolvedValue(null);
    openDialog.mockResolvedValue("/picked/gone.mp4");
    const relinked = { ...MEDIA, path: "/picked/gone.mp4" };
    relinkOp.mockResolvedValue({ ...project, media: [relinked] });
    const run = makeRun(project);
    const phases: string[] = [];

    const outcome = await relinkMedia({
      media: MEDIA,
      run,
      filterName: "Video & audio",
      onPhase: (p) => phases.push(p),
    });

    expect(outcome).toEqual({ kind: "manual", durationChanged: false });
    expect(openDialog).toHaveBeenCalledWith({
      multiple: false,
      filters: [{ name: "Video & audio", extensions: ["mp4", "mov"] }],
    });
    expect(relinkOp).toHaveBeenCalledWith(
      project,
      MEDIA.id,
      "/picked/gone.mp4",
    );
    expect(phases).toEqual(["searching", "picking", "linking"]);
  });

  it("reports a different duration on the picked file as durationChanged", async () => {
    const project = projectWithMedia(MEDIA);
    relinkProbe.mockResolvedValue("/new/gone.mp4");
    const relinked = { ...MEDIA, path: "/new/gone.mp4", duration_ms: 4_000 };
    relinkOp.mockResolvedValue({ ...project, media: [relinked] });
    const run = makeRun(project);

    const outcome = await relinkMedia({
      media: MEDIA, // duration_ms from SAMPLE_PROJECT (18 000)
      run,
      filterName: "Video & audio",
    });

    expect(outcome).toEqual({ kind: "auto", durationChanged: true });
  });

  it("cancelled dialog: never commits, never touches the store", async () => {
    const project = projectWithMedia(MEDIA);
    relinkProbe.mockResolvedValue(null);
    openDialog.mockResolvedValue(null); // user cancelled
    const run = makeRun(project);

    const outcome = await relinkMedia({
      media: MEDIA,
      run,
      filterName: "Video & audio",
    });

    expect(outcome).toEqual({ kind: "cancelled" });
    expect(run).not.toHaveBeenCalled();
    expect(relinkOp).not.toHaveBeenCalled();
  });

  it("off-Tauri: the dialog fallback is skipped (no native picker to open)", async () => {
    isTauriMock.mockReturnValue(false);
    const project = projectWithMedia(MEDIA);
    relinkProbe.mockResolvedValue(null);
    const run = makeRun(project);

    const outcome = await relinkMedia({
      media: MEDIA,
      run,
      filterName: "Video & audio",
    });

    expect(outcome).toEqual({ kind: "cancelled" });
    expect(openDialog).not.toHaveBeenCalled();
  });

  it("surfaces a backend rejection from the commit step as an error outcome", async () => {
    const project = projectWithMedia(MEDIA);
    relinkProbe.mockResolvedValue("/new/gone.mp4");
    relinkOp.mockRejectedValue(
      new Error("gone.mp4 reports a duration of 0 ms"),
    );
    const run = vi.fn(async (op: (p: Project) => Promise<Project>) => {
      await op(project); // propagates the rejection, same as the real store
    });

    const outcome = await relinkMedia({
      media: MEDIA,
      run,
      filterName: "Video & audio",
    });

    expect(outcome).toEqual({
      kind: "error",
      message: "gone.mp4 reports a duration of 0 ms",
    });
  });

  it("surfaces a rejection from the auto-search probe itself", async () => {
    const project = projectWithMedia(MEDIA);
    relinkProbe.mockRejectedValue(new Error("backend unreachable"));
    const run = makeRun(project);

    const outcome = await relinkMedia({
      media: MEDIA,
      run,
      filterName: "Video & audio",
    });

    expect(outcome).toEqual({ kind: "error", message: "backend unreachable" });
    expect(run).not.toHaveBeenCalled();
  });
});
