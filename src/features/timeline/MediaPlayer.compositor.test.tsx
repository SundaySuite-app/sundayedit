/**
 * The E6 acceptance bar: **with the GPU compositor flag OFF, the preview must
 * be exactly what it was before E6.**
 *
 * That is asserted structurally (the stage's rendered markup, pinned literally)
 * rather than by spot-checking a few properties, because the failure mode we
 * are guarding against is a stray wrapper, an inline style or a hidden canvas
 * creeping into the default path — none of which any behavioural test would
 * notice.
 *
 * The second half of the file exercises the automatic off-switch: jsdom has no
 * WebGL2, so turning the flag ON here drives the real fallback end to end and
 * must leave the same DOM behind.
 */

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, cleanup, screen, act } from "@testing-library/react";

import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import { MediaPlayer } from "./MediaPlayer";
import { useCompositorFlag } from "./compositor";

vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));

beforeEach(() => {
  useCompositorFlag.setState({ enabled: false, unavailableReason: null });
  // Keep the rAF reconcile loop from running during these DOM assertions.
  vi.spyOn(window, "requestAnimationFrame").mockImplementation(() => 1);
  vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

/** The markup the preview stage produced before E6 — the acceptance baseline. */
const PRE_E6_STAGE_HTML =
  '<div data-testid="preview-stage" class="grid h-full w-full place-items-center overflow-hidden">' +
  '<video src="asset://x.mp4" class="max-h-full max-w-full" playsinline="" preload="auto"></video>' +
  "</div>";

/**
 * Let `React.lazy` resolve the compositor chunk and run its mount effect.
 * Awaiting the module itself (rather than a fixed number of microtasks) keeps
 * the test honest whatever the loader does under the hood.
 */
async function settleLazyMount() {
  await act(async () => {
    await import("./compositor/PixiCompositor");
  });
  await act(async () => {
    await Promise.resolve();
  });
}

function renderLegacy() {
  return render(
    <MediaPlayer
      src="asset://x.mp4"
      playheadMs={0}
      rate={0}
      durationMs={60_000}
      fps={30}
    />,
  );
}

describe("MediaPlayer — compositor flag OFF (the acceptance bar)", () => {
  it("renders the pre-E6 preview stage, byte for byte", () => {
    renderLegacy();
    expect(screen.getByTestId("preview-stage").outerHTML).toBe(
      PRE_E6_STAGE_HTML,
    );
  });

  it("mounts no compositor and touches no element style", () => {
    renderLegacy();
    expect(screen.queryByTestId("gpu-compositor")).toBeNull();
    const video = document.querySelector("video")!;
    expect(video.getAttribute("style")).toBeNull();
  });

  it("is still the pre-E6 stage in NLE multi-track mode", () => {
    // The other preview branch — same bar.
    render(
      <MediaPlayer
        project={SAMPLE_PROJECT}
        playheadMs={1000}
        rate={0}
        durationMs={18_000}
        fps={30}
      />,
    );
    expect(screen.queryByTestId("gpu-compositor")).toBeNull();
    expect(document.querySelector("video")!.getAttribute("style")).toBeNull();
  });

  it("keeps the flag off by default, so this is what a fresh install gets", () => {
    expect(useCompositorFlag.getState().enabled).toBe(false);
  });
});

describe("MediaPlayer — compositor flag ON without a GPU", () => {
  it("falls back to the identical DOM and records why", async () => {
    // jsdom cannot give a WebGL2 context, which is exactly the shape of the
    // machines the gate exists for.
    useCompositorFlag.setState({ enabled: true, unavailableReason: null });
    renderLegacy();

    await settleLazyMount();

    expect(useCompositorFlag.getState().unavailableReason).toBe("no-webgl2");
    // The host div may exist for a tick, but nothing is drawn into it and the
    // element is never hidden — the picture never disappears.
    const host = screen.queryByTestId("gpu-compositor");
    if (host) {
      expect(host.getAttribute("data-mounted")).toBe("0");
      expect(host.querySelector("canvas")).toBeNull();
    }
    expect(document.querySelector("video")!.getAttribute("style")).toBeNull();
  });

  it("does not rewrite the user's setting when it falls back", async () => {
    useCompositorFlag.setState({ enabled: true, unavailableReason: null });
    renderLegacy();
    await settleLazyMount();
    expect(useCompositorFlag.getState().enabled).toBe(true);
  });

  it("returns to the exact pre-E6 stage once the flag goes back off", async () => {
    useCompositorFlag.setState({ enabled: true, unavailableReason: null });
    renderLegacy();
    await settleLazyMount();
    act(() => {
      useCompositorFlag.setState({ enabled: false, unavailableReason: null });
    });
    expect(screen.getByTestId("preview-stage").outerHTML).toBe(
      PRE_E6_STAGE_HTML,
    );
  });
});
