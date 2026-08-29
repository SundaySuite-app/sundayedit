/**
 * MissingMediaBanner — the app-wide "some source moved" strip. Mocks the
 * lowest layer (Tauri invoke + dialog) like MediaBin.test.tsx, so the real
 * `useMediaAvailability` + `useRelinkMedia` hooks run underneath it.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, cleanup, screen, fireEvent } from "@testing-library/react";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
  isTauri: () => true,
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async () => null),
}));
vi.mock("@tauri-apps/api/path", () => ({
  dirname: async (p: string) => p.split("/").slice(0, -1).join("/") || "/",
  videoDir: async () => "/Movies",
  desktopDir: async () => "/Desktop",
  downloadDir: async () => "/Downloads",
  homeDir: async () => "/Home",
}));

import { MissingMediaBanner } from "./MissingMediaBanner";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import { useProjectStore } from "@/lib/useProjectStore";
import { useLocale } from "@/lib/i18n";
import type { MediaItem, Project } from "@/lib/bindings";

const MEDIA: MediaItem = SAMPLE_PROJECT.media[0];

function projectWith(...media: MediaItem[]): Project {
  return { ...SAMPLE_PROJECT, media };
}

beforeEach(() => {
  invoke.mockReset();
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

describe("MissingMediaBanner", () => {
  it("renders nothing while every pooled file is present", async () => {
    invoke.mockImplementation(async (cmd: unknown) =>
      cmd === "check_media_paths"
        ? [{ media_id: MEDIA.id, path: MEDIA.path, exists: true }]
        : null,
    );
    render(<MissingMediaBanner project={projectWith(MEDIA)} />);

    await new Promise((r) => setTimeout(r, 0));
    expect(screen.queryByTestId("missing-media-banner")).toBeNull();
  });

  it("counts the missing files and offers the relink action", async () => {
    invoke.mockImplementation(async (cmd: unknown) =>
      cmd === "check_media_paths"
        ? [{ media_id: MEDIA.id, path: MEDIA.path, exists: false }]
        : null,
    );
    render(<MissingMediaBanner project={projectWith(MEDIA)} />);

    const banner = await screen.findByTestId("missing-media-banner");
    expect(banner.textContent).toContain("1 file(s) are missing.");
    expect(screen.getByTestId("missing-media-relink-all")).toBeTruthy();
  });

  it("dismisses on click and stays hidden while the missing set is unchanged", async () => {
    invoke.mockImplementation(async (cmd: unknown) =>
      cmd === "check_media_paths"
        ? [{ media_id: MEDIA.id, path: MEDIA.path, exists: false }]
        : null,
    );
    render(<MissingMediaBanner project={projectWith(MEDIA)} />);

    await screen.findByTestId("missing-media-banner");
    fireEvent.click(screen.getByTitle("Close"));
    expect(screen.queryByTestId("missing-media-banner")).toBeNull();
  });

  it("relink-all drives the same auto-search → commit flow as a bin row", async () => {
    const relinked: Project = {
      ...SAMPLE_PROJECT,
      media: [{ ...MEDIA, path: "/new/gone.mp4" }],
    };
    invoke.mockImplementation(async (cmd: unknown, args: unknown) => {
      if (cmd === "check_media_paths")
        return [{ media_id: MEDIA.id, path: MEDIA.path, exists: false }];
      if (cmd === "project_relink") return "/new/gone.mp4";
      if (cmd === "op_relink_media") {
        const a = args as { mediaId: string; newPath: string };
        expect(a.mediaId).toBe(MEDIA.id);
        return relinked;
      }
      return null;
    });
    const project = projectWith(MEDIA);
    useProjectStore.setState({
      project,
      past: [],
      future: [],
      busy: false,
      inFlight: false,
    });
    render(<MissingMediaBanner project={project} />);

    await screen.findByTestId("missing-media-banner");
    fireEvent.click(screen.getByTestId("missing-media-relink-all"));

    await vi.waitFor(() =>
      expect(useProjectStore.getState().project).toEqual(relinked),
    );
  });
});
