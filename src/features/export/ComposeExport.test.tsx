import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, cleanup, screen, fireEvent } from "@testing-library/react";

import { ComposeExport } from "./ComposeExport";
import { COMPOSE_PROGRESS_EVENT } from "@/lib/composeEngine";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import { useLocale } from "@/lib/i18n";

// Mock the lowest layer (Tauri invoke) so the real typed `ipc` wrappers run —
// this pins the compose command names. `isTauri` reports false so the progress
// subscription stays on the deterministic window-CustomEvent path.
const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
  isTauri: () => false,
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
