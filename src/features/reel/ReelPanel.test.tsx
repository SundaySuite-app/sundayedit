import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, cleanup, screen, fireEvent } from "@testing-library/react";

import { ReelPanel } from "./ReelPanel";
import { REEL_RENDER_PROGRESS_EVENT } from "@/lib/ipc";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import { useLocale } from "@/lib/i18n";
import type {
  ExportPreset,
  ReelRenderProgress,
  ReelRenderResult,
  ReelStoryboard,
  RenderPlan,
} from "@/lib/bindings";

// Mock the lowest layer (Tauri invoke) so the real typed `ipc.reel.*` wrappers
// run — this pins the four command names AND their argument keys, which is the
// seam this panel exists to cross. `isTauri` reports true so the native-guarded
// surface renders; the progress subscription is then exercised on the window
// CustomEvent channel (the Tauri event module is stubbed out below), the same
// deterministic path ComposeExport's test uses.
const invoke = vi.fn();
let tauriHost = true;
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
  isTauri: () => tauriHost,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: () => Promise.resolve(() => {}),
}));

const openDialog = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (...args: unknown[]) => openDialog(...args),
}));

const PRESETS: ExportPreset[] = [
  {
    id: "export:reels",
    name: "Reels",
    description: "Vertical 9:16",
    aspect: "portrait",
    width: 1080,
    height: 1920,
    max_duration_sec: 90n,
    codec: "h264",
    bitrate_kbps: 6000,
    also_srt_sidecar: true,
  },
  {
    id: "export:youtube",
    name: "YouTube",
    description: "Landscape 16:9",
    aspect: "landscape",
    width: 1920,
    height: 1080,
    max_duration_sec: null,
    codec: "h264",
    bitrate_kbps: 8000,
    also_srt_sidecar: false,
  },
];

const CLIP = {
  id: "clip:0",
  title: "Velkommen til gudstjenesten",
  hook: "",
  caption_ids: ["c1"],
  start_ms: 0,
  end_ms: 4200,
};

const HEURISTIC: ReelStoryboard = {
  plan: { talk_summary: "", clips: [CLIP] },
  used_ai: false,
  ai_error: null,
};

const PLAN: RenderPlan = {
  items: [
    {
      id: "clip:0__export:reels",
      clip: CLIP,
      preset: PRESETS[0],
      output_path: "/out/01-velkommen-til-gudstjenesten__reels.mp4",
    },
  ],
  total: 1,
};

/**
 * Wire the four reel commands. `renderAll` is deferred so a test can hold the
 * batch open, push progress, and settle it by hand.
 */
function wire(storyboard: ReelStoryboard = HEURISTIC) {
  let settleRender!: (r: ReelRenderResult) => void;
  let failRender!: (e: unknown) => void;
  invoke.mockImplementation((cmd: unknown) => {
    switch (cmd) {
      case "export_list_presets":
        return Promise.resolve(PRESETS);
      case "reel_storyboard":
        return Promise.resolve(storyboard);
      case "reel_build_plan":
        return Promise.resolve(PLAN);
      case "reel_render_all":
        return new Promise<ReelRenderResult>((res, rej) => {
          settleRender = res;
          failRender = rej;
        });
      default:
        return Promise.resolve(undefined); // reel_cancel_render
    }
  });
  return {
    settle: (r: ReelRenderResult) => settleRender(r),
    fail: (e: unknown) => failRender(e),
  };
}

function emitProgress(p: ReelRenderProgress) {
  window.dispatchEvent(
    new CustomEvent(REEL_RENDER_PROGRESS_EVENT, { detail: p }),
  );
}

/** Storyboard → folder → the "Render all" button, ready to click. */
async function upToPlan() {
  render(<ReelPanel project={SAMPLE_PROJECT} />);
  fireEvent.click(screen.getByRole("button", { name: "Propose storyboard" }));
  await screen.findByTestId("reel-mode");
  fireEvent.click(screen.getByRole("button", { name: /No folder chosen/ }));
  // The button exists as soon as there are clips, but stays disabled until the
  // fan-out has resolved — wait for the plan, not just the button.
  await screen.findByTestId("reel-plan");
  return screen.getByTestId("reel-render-all");
}

beforeEach(() => {
  invoke.mockReset();
  openDialog.mockReset();
  openDialog.mockResolvedValue("/out");
  tauriHost = true;
  useLocale.setState({ lang: "en" });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("ReelPanel — environment", () => {
  it("says the reel needs the desktop app instead of offering a render it cannot do", () => {
    tauriHost = false;
    wire();
    render(<ReelPanel project={SAMPLE_PROJECT} />);

    expect(screen.getByTestId("reel-unavailable")).toBeTruthy();
    expect(
      screen.queryByRole("button", { name: "Propose storyboard" }),
    ).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe("ReelPanel — storyboard", () => {
  it("labels the keyless heuristic mode instead of passing it off as AI", async () => {
    wire();
    render(<ReelPanel project={SAMPLE_PROJECT} />);

    fireEvent.click(screen.getByRole("button", { name: "Propose storyboard" }));

    // The command name + argument keys the Rust side actually declares.
    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("reel_storyboard", {
        project: SAMPLE_PROJECT,
        model: "haiku45",
        apiKey: null,
      }),
    );
    expect((await screen.findByTestId("reel-mode")).textContent).toContain(
      "No API key — clips proposed from pauses in the talk.",
    );
    expect(screen.getAllByTestId("reel-clip")).toHaveLength(1);
  });

  it("says the AI produced the clips when it did", async () => {
    wire({
      plan: { talk_summary: "", clips: [CLIP] },
      used_ai: true,
      ai_error: null,
    });
    render(<ReelPanel project={SAMPLE_PROJECT} />);

    fireEvent.click(screen.getByRole("button", { name: "Propose storyboard" }));

    expect((await screen.findByTestId("reel-mode")).textContent).toContain(
      "AI storyboard — Claude picked these clips.",
    );
  });

  it("surfaces an attempted-but-failed AI call while still showing the fallback clips", async () => {
    wire({
      plan: { talk_summary: "", clips: [CLIP] },
      used_ai: false,
      ai_error: "401 unauthorized",
    });
    render(<ReelPanel project={SAMPLE_PROJECT} />);

    fireEvent.click(screen.getByRole("button", { name: "Propose storyboard" }));

    expect((await screen.findByTestId("reel-mode")).textContent).toContain(
      "401 unauthorized",
    );
    expect(screen.getAllByTestId("reel-clip")).toHaveLength(1);
  });

  it("shows the empty state (not a false plan) when the backend proposes nothing", async () => {
    wire({
      plan: { talk_summary: "", clips: [] },
      used_ai: false,
      ai_error: null,
    });
    render(<ReelPanel project={SAMPLE_PROJECT} />);

    fireEvent.click(screen.getByRole("button", { name: "Propose storyboard" }));

    expect(await screen.findByTestId("reel-empty")).toBeTruthy();
    // No clips → no fan-out, so nothing offers to render.
    expect(screen.queryByTestId("reel-render-all")).toBeNull();
  });

  it("reports a hard storyboard failure without pretending it has clips", async () => {
    invoke.mockImplementation((cmd: unknown) =>
      cmd === "export_list_presets"
        ? Promise.resolve(PRESETS)
        : Promise.reject({ code: "internal", message: "backend is gone" }),
    );
    render(<ReelPanel project={SAMPLE_PROJECT} />);

    fireEvent.click(screen.getByRole("button", { name: "Propose storyboard" }));

    expect((await screen.findByTestId("reel-error")).textContent).toContain(
      "backend is gone",
    );
    expect(screen.queryByTestId("reel-clip")).toBeNull();
  });
});

describe("ReelPanel — fan-out", () => {
  it("builds the plan from the chosen folder and the ticked platforms only", async () => {
    wire();
    render(<ReelPanel project={SAMPLE_PROJECT} />);
    fireEvent.click(screen.getByRole("button", { name: "Propose storyboard" }));
    await screen.findByTestId("reel-mode");

    // Portrait presets are preselected — the same default the backend applies
    // for an empty selection — and the landscape one is not.
    expect(
      screen.getByTestId<HTMLInputElement>("reel-preset-export:reels").checked,
    ).toBe(true);
    expect(
      screen.getByTestId<HTMLInputElement>("reel-preset-export:youtube")
        .checked,
    ).toBe(false);

    // No folder yet → no plan promised.
    expect(screen.queryByTestId("reel-plan")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /No folder chosen/ }));
    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("reel_build_plan", {
        plan: { talk_summary: "", clips: [CLIP] },
        presetIds: ["export:reels"],
        outputDir: "/out",
      }),
    );
    expect((await screen.findByTestId("reel-plan")).textContent).toContain(
      "1 file(s) — 1 clip(s) × 1 platform(s)",
    );
  });
});

describe("ReelPanel — batch render", () => {
  it("streams per-item progress from the emitted events", async () => {
    wire();
    fireEvent.click(await upToPlan());

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("reel_render_all", {
        project: SAMPLE_PROJECT,
        plan: PLAN,
      }),
    );
    expect(await screen.findByTestId("reel-progress")).toBeTruthy();

    emitProgress({
      completed: 0,
      total: 1,
      fraction: 0,
      current_item_id: "clip:0__export:reels",
      failed: 0,
    });
    await vi.waitFor(() =>
      expect(screen.getByTestId("reel-item").dataset.itemState).toBe(
        "rendering",
      ),
    );
    expect(screen.getByTestId("reel-progress-count").textContent).toContain(
      "0 of 1",
    );
  });

  it("keeps a cancelled batch calm and still reports what landed", async () => {
    const batch = wire();
    fireEvent.click(await upToPlan());
    await screen.findByTestId("reel-progress");

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("reel_cancel_render", undefined),
    );
    expect(screen.getByText("Stopping after this clip…")).toBeTruthy();

    // Rust RESOLVES a cancelled batch (it never rejects) — the partial result
    // is the point, so this must not land in the error state.
    batch.settle({ rendered: [], failed: [], cancelled: true });

    expect(await screen.findByTestId("reel-cancelled")).toBeTruthy();
    expect(screen.queryByTestId("reel-render-error")).toBeNull();
  });

  it("shows the written files, and a failed clip, once the batch settles", async () => {
    const batch = wire();
    fireEvent.click(await upToPlan());
    await screen.findByTestId("reel-progress");

    batch.settle({
      rendered: [],
      failed: [["clip:0__export:reels", "ffmpeg not found"]],
      cancelled: false,
    });

    expect(await screen.findByTestId("reel-done")).toBeTruthy();
    expect(screen.getByTestId("reel-failed").textContent).toContain(
      "1 clip(s) failed.",
    );
    expect(screen.getByTestId("reel-item").dataset.itemState).toBe("failed");
    expect(screen.getByText("ffmpeg not found")).toBeTruthy();
  });

  it("shows the error state only when the render itself rejects", async () => {
    const batch = wire();
    fireEvent.click(await upToPlan());
    await screen.findByTestId("reel-progress");

    batch.fail({ code: "internal", message: "render task join failed" });

    expect(
      (await screen.findByTestId("reel-render-error")).textContent,
    ).toContain("render task join failed");
    expect(screen.queryByTestId("reel-cancelled")).toBeNull();
  });
});
