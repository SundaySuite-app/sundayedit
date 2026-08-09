/**
 * Timeline — interaction/state regressions that live in the component itself
 * (drag lifecycle, preview-proxy invalidation, keyboard shortcut scoping).
 * The pure geometry/drag/lane math has its own suites (geometry.test.ts,
 * clipDrag.test.ts, laneLayout.test.ts).
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  render,
  cleanup,
  screen,
  fireEvent,
  act,
  waitFor,
} from "@testing-library/react";

// Mock the lowest layer (Tauri invoke) so the real typed `ipc` wrappers run.
// `tauriEnv` flips what `isTauri()` reports per-test: the MediaPlayer +
// preview-render UI is gated on a Tauri runtime.
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
// The compose engine shells out to ffmpeg — pretend the proxy render succeeds.
vi.mock("@/lib/composeEngine", () => ({
  renderPreviewProxy: vi.fn(async () => true),
}));

import { Timeline } from "./Timeline";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import { useProjectStore } from "@/lib/useProjectStore";
import { useLocale } from "@/lib/i18n";
import type { Project } from "@/lib/bindings";

// ── jsdom shims (same shape as MediaPlayer.test.tsx) ────────────────────────

let mockState: { currentTime: number; paused: boolean; duration: number };

function installVideoMock() {
  mockState = { currentTime: 0, paused: true, duration: 60 };
  const proto = window.HTMLMediaElement.prototype;
  vi.spyOn(proto, "play").mockImplementation(() => {
    mockState.paused = false;
    return Promise.resolve();
  });
  vi.spyOn(proto, "pause").mockImplementation(() => {
    mockState.paused = true;
  });
  vi.spyOn(proto, "currentTime", "get").mockImplementation(
    () => mockState.currentTime,
  );
  vi.spyOn(proto, "currentTime", "set").mockImplementation((v: number) => {
    mockState.currentTime = v;
  });
  vi.spyOn(proto, "paused", "get").mockImplementation(() => mockState.paused);
  vi.spyOn(proto, "duration", "get").mockImplementation(
    () => mockState.duration,
  );
}

// Timeline + MediaPlayer run rAF loops — capture instead of free-running.
function installRaf() {
  vi.spyOn(window, "requestAnimationFrame").mockImplementation(() => 1);
  vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
}

// jsdom has no ResizeObserver (Timeline measures its viewport with one).
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

beforeEach(() => {
  invoke.mockReset();
  // Default: no backend under Vitest — the waveform fetch takes its documented
  // browser/demo catch path. Tests override per-command where needed.
  invoke.mockImplementation(() =>
    Promise.reject(new Error("no tauri runtime under vitest")),
  );
  tauriEnv = false;
  vi.stubGlobal("ResizeObserver", ResizeObserverStub);
  installVideoMock();
  installRaf();
  // Deterministic button labels (initialLang falls back to "no").
  act(() => {
    useLocale.setState({ lang: "en" });
  });
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
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

// ── clip drag lifecycle ─────────────────────────────────────────────────────
// Regression (state-clipdrag-no-pointercancel-handler): a `pointercancel`
// (touch/pen gesture takeover, OS interruption) releases the pointer capture
// WITHOUT a pointerup. The drag state used to survive it — the ghost followed
// plain hover moves, and leaving the viewport committed the unintended move.

/** The sample project's single clip box (title = media original_filename). */
function clipBox(): HTMLElement {
  return screen.getByTitle("sermon.mp4");
}

// Initial view is pxPerMs = 0.05, scrollMs = 0 → the clip (start 0ms) rests at
// left = 0px, and a pointer delta of Npx maps to N/0.05 ms → back to Npx.

describe("Timeline — clip drag aborts on pointercancel", () => {
  beforeEach(() => {
    // Echo the project back unchanged for any committed op — the assertions
    // only care WHETHER a commit happened, not what it produced.
    invoke.mockImplementation((cmd: unknown, rawArgs: unknown) =>
      cmd === "waveform_compute"
        ? Promise.reject(new Error("no waveform under vitest"))
        : Promise.resolve((rawArgs as { project: Project }).project),
    );
  });

  it("aborts the drag on pointercancel — the ghost must not follow later hover moves", () => {
    render(<Timeline project={SAMPLE_PROJECT} />);
    const clip = clipBox();
    expect(clip.style.left).toBe("0px");

    // Start dragging the clip and move 50px right — live ghost follows.
    fireEvent.pointerDown(clip, {
      pointerId: 1,
      button: 0,
      buttons: 1,
      clientX: 100,
      clientY: 10,
    });
    fireEvent.pointerMove(clip, {
      pointerId: 1,
      buttons: 1,
      clientX: 150,
      clientY: 10,
    });
    expect(clipBox().style.left).toBe("50px"); // sanity: drag is active

    // The system cancels the drag (gesture takeover / OS interruption).
    // pointercancel implicitly releases capture; NO pointerup will follow.
    fireEvent.pointerCancel(clip, { pointerId: 1 });

    // A plain hover move afterwards — no button held.
    fireEvent.pointerMove(clip, {
      pointerId: 1,
      buttons: 0,
      clientX: 300,
      clientY: 10,
    });

    // The cancelled drag was aborted: ghost back at rest.
    expect(clipBox().style.left).toBe("0px");
  });

  it("commits nothing when the pointer leaves the viewport after a cancelled drag", async () => {
    render(<Timeline project={SAMPLE_PROJECT} />);
    const clip = clipBox();

    fireEvent.pointerDown(clip, {
      pointerId: 1,
      button: 0,
      buttons: 1,
      clientX: 100,
      clientY: 10,
    });
    fireEvent.pointerCancel(clip, { pointerId: 1 });

    // Hover wanders 200px right (no button), then exits the viewport —
    // relatedTarget outside the viewport makes React fire onPointerLeave on
    // the viewport ancestor.
    fireEvent.pointerMove(clip, {
      pointerId: 1,
      buttons: 0,
      clientX: 300,
      clientY: 10,
    });
    await act(async () => {
      fireEvent.pointerOut(clip, {
        pointerId: 1,
        clientX: 300,
        clientY: 10,
        relatedTarget: document.body,
      });
    });

    // The cancelled drag commits nothing — no op, no undo entry.
    expect(invoke).not.toHaveBeenCalledWith(
      "op_move_timeline_item",
      expect.anything(),
    );
    expect(useProjectStore.getState().project).toBe(SAMPLE_PROJECT);
    expect(useProjectStore.getState().past).toHaveLength(0);
  });
});

// ── preview-proxy staleness ─────────────────────────────────────────────────
// Regression (state-proxy-preview-not-invalidated-on-edit): after "Render
// preview" completed, `proxySrc`/`previewState` were keyed to nothing — any
// subsequent edit kept the PRE-edit composite playing while the toolbar still
// said "Preview rendered". An edit must invalidate the proxy.

describe("Timeline — rendered preview proxy is invalidated on edit", () => {
  it("drops the proxy and the 'Preview rendered' state when the project changes", async () => {
    tauriEnv = true;
    const before: Project = structuredClone(SAMPLE_PROJECT);

    const { container, getByRole, queryByRole, rerender } = render(
      <Timeline project={before} />,
    );
    const video = container.querySelector("video")!;
    // NLE mapping mode: the React src attribute is unbound (rAF drives it).
    expect(video.getAttribute("src")).toBeNull();

    // User renders the preview proxy (compose engine mocked to succeed).
    act(() => {
      getByRole("button", { name: "Render preview" }).click();
    });
    await waitFor(() => {
      expect(video.getAttribute("src") ?? "").toContain(
        "sundayedit-preview.mp4",
      );
    });
    expect(getByRole("button", { name: "Preview rendered" })).toBeTruthy();

    // The user then drags the clip 5 s later on the timeline. The commit lands
    // as a new Project from the store — Timeline re-renders with the edited
    // project (same thing run/undo/redo do).
    const after: Project = structuredClone(before);
    after.timeline_items[0].timeline_start_ms += 5000;
    after.updated_at = before.updated_at + 1;
    rerender(<Timeline project={after} />);

    // The rendered proxy composites the OLD clip positions — the player must
    // return to the live per-clip mapping…
    expect(video.getAttribute("src") ?? "").not.toContain(
      "sundayedit-preview.mp4",
    );
    // …and the toolbar must stop asserting the preview is current.
    expect(queryByRole("button", { name: "Preview rendered" })).toBeNull();
  });
});

// ── keyboard shortcut scoping ───────────────────────────────────────────────
// Regression (diff-shuttle-keys-ignore-modifiers): j/k/l/s used to fire on
// modified keypresses too — Cmd+K (command palette) stopped playback, Cmd+S
// toggled snapping, and preventDefault swallowed app-level chords.

describe("Timeline — shuttle/snap keys ignore modified chords", () => {
  it("ignores Cmd+J / Ctrl+S — app-level chords must not drive shuttle or snap", () => {
    tauriEnv = true;
    const { container, getByTitle, queryByText } = render(
      <Timeline project={structuredClone(SAMPLE_PROJECT)} />,
    );
    const surface = container.firstElementChild as HTMLElement; // tabIndex=0 keyboard surface

    // Cmd+J is an app-level chord, not the J shuttle key. It must not start
    // reverse playback (the "◂ 1×" shuttle badge must not appear) and must be
    // left unprevented for window-level handlers/menu accelerators.
    const notPrevented = fireEvent.keyDown(surface, {
      key: "j",
      metaKey: true,
    });
    expect(queryByText("◂ 1×")).toBeNull();
    expect(notPrevented).toBe(true); // false ⇔ preventDefault was called

    // Ctrl+S / Cmd+S is the save chord — it must not silently toggle snapping.
    const snapBtn = getByTitle("Snap"); // t("timelineSnap"), aria-pressed
    expect(snapBtn.getAttribute("aria-pressed")).toBe("true");
    fireEvent.keyDown(surface, { key: "s", ctrlKey: true });
    expect(snapBtn.getAttribute("aria-pressed")).toBe("true");
  });

  it("does not stop playback when Cmd+K (command palette chord) is pressed", () => {
    tauriEnv = true;
    const { container, getByText, queryByText } = render(
      <Timeline project={structuredClone(SAMPLE_PROJECT)} />,
    );
    const surface = container.firstElementChild as HTMLElement;

    // Legitimate unmodified shuttle: L L → 2× forward (badge visible).
    fireEvent.keyDown(surface, { key: "l" });
    fireEvent.keyDown(surface, { key: "l" });
    expect(getByText("2× ▸")).toBeTruthy();

    // User opens the command palette with Cmd+K while the timeline has focus.
    // The palette chord must not double as the K (stop) shuttle key.
    fireEvent.keyDown(surface, { key: "k", metaKey: true });
    expect(queryByText("2× ▸")).not.toBeNull(); // playback must keep running
  });
});
