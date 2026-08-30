import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, cleanup, screen, fireEvent } from "@testing-library/react";

import { ComposeExport } from "./ComposeExport";
import { COMPOSE_PROGRESS_EVENT } from "@/lib/composeEngine";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import { useLocale } from "@/lib/i18n";

// Mock the lowest layer (Tauri invoke) so the real typed `ipc` wrappers run —
// this pins the compose command names. `tauriEnv` flips what `isTauri()`
// reports per-test; off (the default) keeps the progress subscription on the
// deterministic window-CustomEvent path used by most of this file's tests.
const invoke = vi.fn();
let tauriEnv = false;
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
  isTauri: () => tauriEnv,
}));

// The save dialog resolves a fixed output path so the export flow runs.
const saveDialog = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: (...args: unknown[]) => saveDialog(...args),
}));

/** Wire compose_render to a deferred promise the test settles by hand. */
function deferRender() {
  let resolveRender!: () => void;
  let rejectRender!: (e: unknown) => void;
  invoke.mockImplementation((cmd: unknown) => {
    if (cmd === "compose_render")
      return new Promise<void>((res, rej) => {
        resolveRender = res;
        rejectRender = rej;
      });
    return Promise.resolve(undefined); // compose_cancel etc.
  });
  return {
    resolve: () => resolveRender(),
    reject: (e: unknown) => rejectRender(e),
  };
}

/** Click the export action and wait for the progress overlay to open. */
async function startExport() {
  render(<ComposeExport project={SAMPLE_PROJECT} />);
  fireEvent.click(
    screen.getByRole("button", { name: /Export composed video/ }),
  );
  await screen.findByTestId("compose-progress");
}

beforeEach(() => {
  invoke.mockReset();
  saveDialog.mockReset();
  saveDialog.mockResolvedValue("/demo/out.mp4");
  useLocale.setState({ lang: "en" });
  tauriEnv = false;
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("ComposeExport — cancel vs error", () => {
  it("shows the calm cancelled state (not the error banner) after a user cancel", async () => {
    const render_ = deferRender();
    await startExport();

    // Cancel → the flag flips locally and compose_cancel is invoked.
    fireEvent.click(screen.getByRole("button", { name: /Cancel/ }));
    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("compose_cancel", undefined),
    );
    expect(screen.getByText("Cancelling…")).toBeTruthy();

    // The Rust side rejects the render future. Use a message WITHOUT "cancel"
    // to prove the local cancelRequested flag alone carries the decision.
    render_.reject(new Error("ffmpeg exited early"));

    expect(await screen.findByTestId("compose-cancelled")).toBeTruthy();
    expect(screen.getByText("Render cancelled.")).toBeTruthy();
    expect(screen.queryByTestId("compose-error")).toBeNull();
  });

  it("maps a backend 'cancelled' rejection to the calm state even without a local cancel", async () => {
    const render_ = deferRender();
    await startExport();

    // Exact Rust error text from services/compose.rs (AppError shape → IPCError).
    render_.reject({ code: "Internal", message: "compose render cancelled" });

    expect(await screen.findByTestId("compose-cancelled")).toBeTruthy();
    expect(screen.queryByTestId("compose-error")).toBeNull();
  });

  it("still shows the error banner for a real failure", async () => {
    const render_ = deferRender();
    await startExport();

    render_.reject({ code: "Internal", message: "ffmpeg exploded" });

    const error = await screen.findByTestId("compose-error");
    expect(error.textContent).toContain("ffmpeg exploded");
    expect(screen.queryByTestId("compose-cancelled")).toBeNull();
  });

  it("streams progress and lands in the done state on success", async () => {
    const render_ = deferRender();
    await startExport();

    // Progress arrives via the window CustomEvent path off-Tauri.
    window.dispatchEvent(
      new CustomEvent(COMPOSE_PROGRESS_EVENT, {
        detail: {
          out_ms: 6_000,
          total_ms: 12_000,
          fraction: 0.5,
          frame: 180,
          done: false,
        },
      }),
    );
    expect(await screen.findByText("50%")).toBeTruthy();

    render_.resolve();
    const done = await screen.findByTestId("compose-done");
    expect(done.textContent).toContain("/demo/out.mp4");

    // Close returns to idle — the overlay unmounts.
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(screen.queryByTestId("compose-progress")).toBeNull();
  });

  it("does nothing when the save dialog is dismissed", async () => {
    saveDialog.mockResolvedValue(null);
    render(<ComposeExport project={SAMPLE_PROJECT} />);
    fireEvent.click(
      screen.getByRole("button", { name: /Export composed video/ }),
    );
    await vi.waitFor(() => expect(saveDialog).toHaveBeenCalled());
    expect(screen.queryByTestId("compose-progress")).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });
});

// ── encoder/codec picker (R3-B) ──────────────────────────────────────────────
// Regression: the multi-track export used to hardcode `encoder: "cpu"`
// unconditionally — minutes vs. close to an hour on a 60-minute timeline.

describe("ComposeExport — encoder/codec picker", () => {
  function encoderSelect(): HTMLSelectElement {
    return screen.getByTestId("compose-encoder-select") as HTMLSelectElement;
  }

  it("stays on cpu off-Tauri, without ever calling compose_default_encoder", () => {
    invoke.mockResolvedValue(undefined);
    render(<ComposeExport project={SAMPLE_PROJECT} />);
    expect(encoderSelect().value).toBe("cpu");
    expect(invoke).not.toHaveBeenCalledWith(
      "compose_default_encoder",
      undefined,
    );
  });

  it("adopts the platform's hardware-aware default under Tauri — no longer unconditionally cpu", async () => {
    tauriEnv = true;
    invoke.mockImplementation((cmd: unknown) =>
      cmd === "compose_default_encoder"
        ? Promise.resolve("video-toolbox")
        : Promise.resolve(undefined),
    );
    render(<ComposeExport project={SAMPLE_PROJECT} />);
    await vi.waitFor(() => expect(encoderSelect().value).toBe("video-toolbox"));
  });

  it("always keeps CPU (most compatible) selectable regardless of the detected default", async () => {
    tauriEnv = true;
    invoke.mockImplementation((cmd: unknown) =>
      cmd === "compose_default_encoder"
        ? Promise.resolve("nvenc")
        : Promise.resolve(undefined),
    );
    render(<ComposeExport project={SAMPLE_PROJECT} />);
    await vi.waitFor(() => expect(encoderSelect().value).toBe("nvenc"));
    const options = Array.from(encoderSelect().options).map((o) => o.value);
    expect(options).toContain("cpu");
    expect(screen.getByText("CPU (most compatible)")).toBeTruthy();
  });

  it("sends the user's picked codec + encoder to compose_render, not the hardcoded baseline", async () => {
    invoke.mockResolvedValue(undefined);
    render(<ComposeExport project={SAMPLE_PROJECT} />);

    fireEvent.click(screen.getByTestId("compose-codec-h265"));
    fireEvent.change(encoderSelect(), { target: { value: "nvenc" } });
    fireEvent.click(
      screen.getByRole("button", { name: /Export composed video/ }),
    );

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "compose_render",
        expect.objectContaining({
          settings: expect.objectContaining({
            codec: "h265",
            encoder: "nvenc",
          }),
        }),
      ),
    );
  });
});
