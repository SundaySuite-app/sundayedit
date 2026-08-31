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
  within,
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
import { renderPreviewProxy } from "@/lib/composeEngine";
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

// ── preview quality ladder ──────────────────────────────────────────────────
// One rung serves the whole preview stack (previewQuality.ts): the live surface
// and any flatten asked for from the same state. Parked, nothing is degraded —
// the ladder must not quietly cost the user resolution on a still frame.

describe("Timeline — preview quality ladder", () => {
  it("renders the preview surface full-size while parked and smaller while rolling", () => {
    tauriEnv = true;
    const { container } = render(
      <Timeline project={structuredClone(SAMPLE_PROJECT)} />,
    );
    const surface = container.firstElementChild as HTMLElement;
    const stage = () =>
      container.querySelector('[data-testid="preview-stage"]') as HTMLElement;

    expect(stage().style.transform).toBe(""); // idle → 100 %

    fireEvent.keyDown(surface, { key: " " }); // play → 50 %
    expect(stage().style.width).toBe("50%");
    expect(stage().style.transform).toBe("scale(2)");

    fireEvent.keyDown(surface, { key: " " }); // stop → back to full
    expect(stage().style.transform).toBe("");
  });

  it("drops to the interaction rung while a clip is being dragged", () => {
    tauriEnv = true;
    const { container } = render(
      <Timeline project={structuredClone(SAMPLE_PROJECT)} />,
    );
    const stage = () =>
      container.querySelector('[data-testid="preview-stage"]') as HTMLElement;
    const clip = screen.getByTitle("sermon.mp4");

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
    expect(stage().style.width).toBe("25%");

    fireEvent.pointerCancel(clip, { pointerId: 1 });
    expect(stage().style.transform).toBe("");
  });

  it("asks for the proxy flatten at full geometry from a parked timeline", async () => {
    tauriEnv = true;
    const proxy = vi.mocked(renderPreviewProxy);
    proxy.mockClear();
    const { getByRole } = render(
      <Timeline project={structuredClone(SAMPLE_PROJECT)} />,
    );
    act(() => {
      getByRole("button", { name: "Render preview" }).click();
    });
    await waitFor(() => expect(proxy).toHaveBeenCalled());
    // The normal case, and byte-identical to the pre-ladder behaviour.
    expect(proxy.mock.calls[0][2]).toBe(100);
  });

  it("asks for a smaller proxy when the flatten is requested mid-shuttle", async () => {
    tauriEnv = true;
    const proxy = vi.mocked(renderPreviewProxy);
    proxy.mockClear();
    const { container, getByRole } = render(
      <Timeline project={structuredClone(SAMPLE_PROJECT)} />,
    );
    fireEvent.keyDown(container.firstElementChild as HTMLElement, { key: "l" });
    act(() => {
      getByRole("button", { name: "Render preview" }).click();
    });
    await waitFor(() => expect(proxy).toHaveBeenCalled());
    expect(proxy.mock.calls[0][2]).toBe(50);
  });
});

// ── blade + delete keyboard ops ─────────────────────────────────────────────
// B splits the selected clip at the playhead (S is taken by snap); Delete/
// Backspace ripple-deletes it. Both commit through the shared store.

describe("Timeline — blade (B) and Delete clip ops", () => {
  beforeEach(() => {
    invoke.mockImplementation((cmd: unknown, rawArgs: unknown) =>
      cmd === "waveform_compute"
        ? Promise.reject(new Error("no waveform under vitest"))
        : Promise.resolve((rawArgs as { project: Project }).project),
    );
  });

  function surfaceOf(container: HTMLElement): HTMLElement {
    return container.firstElementChild as HTMLElement;
  }

  it("B is a no-op while the playhead rests on the clip's edge, splits once inside", async () => {
    const { container } = render(<Timeline project={SAMPLE_PROJECT} />);
    const surface = surfaceOf(container);

    // Select the clip; onSelect parks the playhead at the clip start (0) —
    // NOT strictly inside the 0..18000 span, so B must do nothing.
    fireEvent.click(clipBox());
    fireEvent.keyDown(surface, { key: "b" });
    expect(invoke).not.toHaveBeenCalledWith(
      "op_split_timeline_item",
      expect.anything(),
    );

    // Seek to 2000 ms (canvas x=100 at pxPerMs 0.05, rect left 0 in jsdom)
    // and blade again — the op commits at the playhead.
    const canvas = container.querySelector("canvas")!;
    fireEvent.pointerDown(canvas, { clientX: 100 });
    fireEvent.keyDown(surface, { key: "b" });
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "op_split_timeline_item",
        expect.objectContaining({ itemId: "ti1", atTimelineMs: 2000 }),
      ),
    );
  });

  it("Delete ripple-deletes the selected clip through the store", async () => {
    const { container } = render(<Timeline project={SAMPLE_PROJECT} />);
    fireEvent.click(clipBox());
    fireEvent.keyDown(surfaceOf(container), { key: "Delete" });
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "op_ripple_delete_item",
        expect.objectContaining({ itemId: "ti1" }),
      ),
    );
  });

  it("Delete without a selected clip commits nothing", () => {
    const { container } = render(<Timeline project={SAMPLE_PROJECT} />);
    fireEvent.keyDown(surfaceOf(container), { key: "Delete" });
    expect(invoke).not.toHaveBeenCalledWith(
      "op_ripple_delete_item",
      expect.anything(),
    );
  });
});

// ── ⌘D duplicate, ⌘C/⌘V copy-paste (R3-B) ────────────────────────────────────
// The typing-guard + "only these three modified chords" scoping is exercised
// in "Timeline — shuttle/snap keys ignore modified chords" below; this block
// covers the ops themselves.

describe("Timeline — duplicate (⌘D) and copy/paste (⌘C/⌘V) clip ops", () => {
  function surfaceOf(container: HTMLElement): HTMLElement {
    return container.firstElementChild as HTMLElement;
  }

  beforeEach(() => {
    invoke.mockImplementation((cmd: unknown, rawArgs: unknown) =>
      cmd === "waveform_compute"
        ? Promise.reject(new Error("no waveform under vitest"))
        : Promise.resolve((rawArgs as { project: Project }).project),
    );
  });

  it("⌘D duplicates the selected clip", async () => {
    const { container } = render(<Timeline project={SAMPLE_PROJECT} />);
    fireEvent.click(clipBox());
    fireEvent.keyDown(surfaceOf(container), { key: "d", metaKey: true });
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "op_duplicate_timeline_item",
        expect.objectContaining({ itemId: "ti1" }),
      ),
    );
  });

  it("Ctrl+D duplicates too (Windows)", async () => {
    const { container } = render(<Timeline project={SAMPLE_PROJECT} />);
    fireEvent.click(clipBox());
    fireEvent.keyDown(surfaceOf(container), { key: "d", ctrlKey: true });
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "op_duplicate_timeline_item",
        expect.objectContaining({ itemId: "ti1" }),
      ),
    );
  });

  it("⌘D without a selected clip commits nothing", () => {
    const { container } = render(<Timeline project={SAMPLE_PROJECT} />);
    fireEvent.keyDown(surfaceOf(container), { key: "d", metaKey: true });
    expect(invoke).not.toHaveBeenCalledWith(
      "op_duplicate_timeline_item",
      expect.anything(),
    );
  });

  it("⌘D on a locked clip commits nothing", () => {
    const locked: Project = {
      ...SAMPLE_PROJECT,
      timeline_items: SAMPLE_PROJECT.timeline_items.map((it) =>
        it.id === "ti1" ? { ...it, locked: true } : it,
      ),
    };
    const { container } = render(<Timeline project={locked} />);
    fireEvent.click(clipBox());
    fireEvent.keyDown(surfaceOf(container), { key: "d", metaKey: true });
    expect(invoke).not.toHaveBeenCalledWith(
      "op_duplicate_timeline_item",
      expect.anything(),
    );
  });

  it("Cmd+Alt+D is left alone (not a timeline shortcut)", () => {
    const { container } = render(<Timeline project={SAMPLE_PROJECT} />);
    fireEvent.click(clipBox());
    fireEvent.keyDown(surfaceOf(container), {
      key: "d",
      metaKey: true,
      altKey: true,
    });
    expect(invoke).not.toHaveBeenCalledWith(
      "op_duplicate_timeline_item",
      expect.anything(),
    );
  });

  it("⌘C then ⌘V duplicates the copied clip and moves it to the playhead", async () => {
    invoke.mockImplementation((cmd: unknown, rawArgs: unknown) => {
      if (cmd === "waveform_compute") {
        return Promise.reject(new Error("no waveform under vitest"));
      }
      if (cmd === "op_duplicate_timeline_item") {
        const { project, itemId } = rawArgs as {
          project: Project;
          itemId: string;
        };
        const orig = project.timeline_items.find((i) => i.id === itemId)!;
        const clone = {
          ...orig,
          id: "ti1-dup",
          timeline_start_ms: orig.timeline_start_ms + 18_000,
        };
        return Promise.resolve({
          ...project,
          timeline_items: [...project.timeline_items, clone],
        });
      }
      if (cmd === "op_move_timeline_item") {
        const { project, itemId, newTrackId, newTimelineStartMs } = rawArgs as {
          project: Project;
          itemId: string;
          newTrackId: string;
          newTimelineStartMs: number;
        };
        return Promise.resolve({
          ...project,
          timeline_items: project.timeline_items.map((it) =>
            it.id === itemId
              ? {
                  ...it,
                  track_id: newTrackId,
                  timeline_start_ms: newTimelineStartMs,
                }
              : it,
          ),
        });
      }
      return Promise.resolve((rawArgs as { project: Project }).project);
    });

    const { container } = render(<Timeline project={SAMPLE_PROJECT} />);
    const surface = surfaceOf(container);
    fireEvent.click(clipBox());
    fireEvent.keyDown(surface, { key: "c", metaKey: true });

    // Seek the playhead to 2000ms (same geometry as the blade test above).
    const canvas = container.querySelector("canvas")!;
    fireEvent.pointerDown(canvas, { clientX: 100 });

    fireEvent.keyDown(surface, { key: "v", metaKey: true });

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "op_duplicate_timeline_item",
        expect.objectContaining({ itemId: "ti1" }),
      ),
    );
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "op_move_timeline_item",
        expect.objectContaining({
          itemId: "ti1-dup",
          newTrackId: "tv",
          newTimelineStartMs: 2000,
        }),
      ),
    );
  });

  it("⌘V with nothing copied commits nothing", () => {
    const { container } = render(<Timeline project={SAMPLE_PROJECT} />);
    fireEvent.keyDown(surfaceOf(container), { key: "v", metaKey: true });
    expect(invoke).not.toHaveBeenCalledWith(
      "op_duplicate_timeline_item",
      expect.anything(),
    );
    expect(invoke).not.toHaveBeenCalledWith(
      "op_move_timeline_item",
      expect.anything(),
    );
  });

  it("⌘V is a no-op when the SELECTED track's kind doesn't match the copied clip's", async () => {
    // A second, audio, track+clip — mirrors the pointer drag/drop rule
    // (`clipDrag.trackKind`): a clip can only land on a track of its own kind.
    const withAudioTrack: Project = {
      ...SAMPLE_PROJECT,
      media: [
        ...SAMPLE_PROJECT.media,
        {
          id: "m2",
          path: "/demo/music.mp3",
          content_hash: "demo2",
          kind: "audio_only",
          duration_ms: 5000,
          width: 0,
          height: 0,
          fps: 0,
          has_audio: true,
          audio_wav_path: null,
          original_filename: "music.mp3",
          added_at: 0,
        },
      ],
      tracks: [
        ...SAMPLE_PROJECT.tracks,
        {
          id: "ta",
          kind: "audio",
          name: "Audio",
          index: 2,
          enabled: true,
          locked: false,
          muted: false,
          solo: false,
          volume_db: 0,
        },
      ],
      timeline_items: [
        ...SAMPLE_PROJECT.timeline_items,
        {
          id: "ti2",
          track_id: "ta",
          kind: "av",
          source_media_id: "m2",
          in_ms: 0,
          out_ms: 5000,
          timeline_start_ms: 0,
          speed: 1,
          gain_db: 0,
          fade_in_ms: 0,
          fade_out_ms: 0,
          transform: {
            x: 0,
            y: 0,
            scale: 1,
            rotation_deg: 0,
            opacity: 1,
            crop: null,
          },
          effects: [],
          transition_in: null,
          text: null,
          enabled: true,
          locked: false,
        },
      ],
    };
    const { container } = render(<Timeline project={withAudioTrack} />);
    const surface = surfaceOf(container);

    fireEvent.click(clipBox()); // ti1, video, on "tv"
    fireEvent.keyDown(surface, { key: "c", metaKey: true });

    fireEvent.click(screen.getByTitle("music.mp3")); // ti2, audio, on "ta"
    fireEvent.keyDown(surface, { key: "v", metaKey: true });

    expect(invoke).not.toHaveBeenCalledWith(
      "op_duplicate_timeline_item",
      expect.anything(),
    );
  });
});

// ── remove track ────────────────────────────────────────────────────────────
// The ✕ in a track header commits op_remove_track; the backend rejects a
// non-empty track and the message surfaces as an alert strip instead of
// vanishing into a silent catch.

describe("Timeline — remove track surfaces backend rejections", () => {
  it("shows the rejection message as an alert", async () => {
    invoke.mockImplementation((cmd: unknown) =>
      cmd === "op_remove_track"
        ? Promise.reject(
            Object.assign(new Error("track tv is not empty"), {
              code: "validation",
            }),
          )
        : Promise.reject(new Error("no tauri runtime under vitest")),
    );
    render(<Timeline project={SAMPLE_PROJECT} />);
    const header = screen.getByTestId("track-header-tv");
    fireEvent.click(within(header).getByTestId("remove-track"));
    await waitFor(() =>
      expect(screen.getByRole("alert").textContent).toContain("is not empty"),
    );
  });
});

// ── gap engine UI (E3-UI) ────────────────────────────────────────────────────
// The track header's "close gaps" button commits op_pack_track through the
// shared undo stack. Hidden on caption tracks (gaps there aren't TimelineItems
// — see Timeline.tsx's `canPackGaps`) and on locked tracks.

describe("Timeline — close-gaps track action", () => {
  beforeEach(() => {
    invoke.mockImplementation((cmd: unknown, rawArgs: unknown) =>
      cmd === "waveform_compute"
        ? Promise.reject(new Error("no waveform under vitest"))
        : Promise.resolve((rawArgs as { project: Project }).project),
    );
  });

  it("commits op_pack_track for the clicked track", async () => {
    render(<Timeline project={SAMPLE_PROJECT} />);
    const header = screen.getByTestId("track-header-tv");
    fireEvent.click(within(header).getByTestId("pack-track-tv"));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "op_pack_track",
        expect.objectContaining({ trackId: "tv" }),
      ),
    );
  });

  it("is not offered on a caption track (gaps live in TimelineItems, not captions)", () => {
    render(<Timeline project={SAMPLE_PROJECT} />);
    const header = screen.getByTestId("track-header-tc");
    expect(within(header).queryByTestId("pack-track-tc")).toBeNull();
  });

  it("is not offered on a locked track", () => {
    const locked: Project = {
      ...SAMPLE_PROJECT,
      tracks: SAMPLE_PROJECT.tracks.map((t) =>
        t.id === "tv" ? { ...t, locked: true } : t,
      ),
    };
    render(<Timeline project={locked} />);
    const header = screen.getByTestId("track-header-tv");
    expect(within(header).queryByTestId("pack-track-tv")).toBeNull();
  });
});

// ── track enabled toggle (R3-B) ──────────────────────────────────────────────
// `Track.enabled` was already honoured by preview (previewMap.ts) and export
// (compose_track_flags.rs) and settable through `op_set_track_flags`, but
// nothing in the UI could flip it — so the smoke-test row asking to verify
// "disabled track excluded from export" wasn't even reachable by clicking.

describe("Timeline — track enabled/visibility toggle", () => {
  beforeEach(() => {
    invoke.mockImplementation((cmd: unknown, rawArgs: unknown) =>
      cmd === "waveform_compute"
        ? Promise.reject(new Error("no waveform under vitest"))
        : Promise.resolve((rawArgs as { project: Project }).project),
    );
  });

  it("commits op_set_track_flags {enabled:false} for an enabled track", async () => {
    render(<Timeline project={SAMPLE_PROJECT} />);
    const header = screen.getByTestId("track-header-tv");
    fireEvent.click(within(header).getByTestId("track-enabled-tv"));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "op_set_track_flags",
        expect.objectContaining({ trackId: "tv", enabled: false }),
      ),
    );
  });

  it("commits {enabled:true} to re-enable an already-disabled track", async () => {
    const disabled: Project = {
      ...SAMPLE_PROJECT,
      tracks: SAMPLE_PROJECT.tracks.map((t) =>
        t.id === "tv" ? { ...t, enabled: false } : t,
      ),
    };
    render(<Timeline project={disabled} />);
    const header = screen.getByTestId("track-header-tv");
    fireEvent.click(within(header).getByTestId("track-enabled-tv"));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith(
        "op_set_track_flags",
        expect.objectContaining({ trackId: "tv", enabled: true }),
      ),
    );
  });

  it("is offered on a caption track too (not just audible video/audio ones)", () => {
    render(<Timeline project={SAMPLE_PROJECT} />);
    const header = screen.getByTestId("track-header-tc");
    expect(within(header).getByTestId("track-enabled-tc")).toBeTruthy();
  });
});

// ── anchored zoom ────────────────────────────────────────────────────────────
// zoomAround must pin the PLAYHEAD (not the viewport centre) for the +/-
// buttons — the standard "zoom toward what I'm looking at" affordance when
// there's no pointer to anchor on.

describe("Timeline — zoom buttons anchor on the playhead", () => {
  it("keeps the playhead's on-screen pixel fixed across a zoom-in click", () => {
    const { container } = render(<Timeline project={SAMPLE_PROJECT} />);
    const canvas = container.querySelector("canvas")!;

    // Seek the playhead to 3000ms (pxPerMs 0.05 → x=150 at scrollMs 0).
    fireEvent.pointerDown(canvas, { clientX: 150 });

    const playheadLineBefore = container.querySelector(
      ".bg-white\\/90",
    ) as HTMLElement;
    const xBefore = parseFloat(playheadLineBefore.style.left);
    expect(xBefore).toBeCloseTo(150, 0);

    fireEvent.click(screen.getByLabelText("Zoom in"));

    const playheadLineAfter = container.querySelector(
      ".bg-white\\/90",
    ) as HTMLElement;
    const xAfter = parseFloat(playheadLineAfter.style.left);
    // The playhead's SCREEN position must not have moved — only pxPerMs did.
    expect(xAfter).toBeCloseTo(xBefore, 0);
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

// ── missing-media indicator (Round: relink media) ────────────────────────────

describe("Timeline — missing media indicator", () => {
  it("marks the clip box when check_media_paths reports its source missing", async () => {
    tauriEnv = true;
    invoke.mockImplementation((cmd: unknown) =>
      cmd === "check_media_paths"
        ? Promise.resolve([
            { media_id: "m1", path: "/demo/sermon.mp4", exists: false },
          ])
        : Promise.reject(new Error("not mocked")),
    );
    render(<Timeline project={SAMPLE_PROJECT} />);

    await screen.findByTestId("clip-missing-badge");
    expect(screen.getByTitle(/Source file missing/)).toBeTruthy();
  });

  it("leaves the clip box alone when the source is present", async () => {
    tauriEnv = true;
    invoke.mockImplementation((cmd: unknown) =>
      cmd === "check_media_paths"
        ? Promise.resolve([
            { media_id: "m1", path: "/demo/sermon.mp4", exists: true },
          ])
        : Promise.reject(new Error("not mocked")),
    );
    render(<Timeline project={SAMPLE_PROJECT} />);

    // Give the availability check a tick to (not) land.
    await new Promise((r) => setTimeout(r, 0));
    expect(screen.queryByTestId("clip-missing-badge")).toBeNull();
    expect(screen.getByTitle("sermon.mp4")).toBeTruthy();
  });
});

// ── text overlay (R5-C) ──────────────────────────────────────────────────────
// The toolbar's "Add text overlay" button is the only way into the feature.
// Everything it does lives inside ONE `run` callback, so the whole gesture is
// a SINGLE undo entry — and the new item is nudged off the frame corner,
// because `Transform::default()` is (0, 0) and the export anchors a text
// overlay's TOP-LEFT there.

describe("Timeline — add text overlay", () => {
  /** A minimal stand-in backend for the three ops the gesture chains. */
  function stubOps() {
    invoke.mockImplementation((cmd: unknown, rawArgs: unknown) => {
      const args = rawArgs as { project: Project; [k: string]: unknown };
      const p = args.project;
      switch (cmd) {
        case "op_add_track":
          return Promise.resolve({
            ...p,
            tracks: [
              ...p.tracks,
              {
                id: "to1",
                kind: "overlay",
                name: "Overlay",
                index: 2,
                enabled: true,
                locked: false,
                muted: false,
                solo: false,
                volume_db: 0,
              },
            ],
          });
        case "op_add_text_item":
          return Promise.resolve({
            ...p,
            timeline_items: [
              ...p.timeline_items,
              {
                ...p.timeline_items[0],
                id: "tx-new",
                track_id: args.trackId as string,
                kind: "text",
                source_media_id: null,
                in_ms: 0,
                out_ms: args.durationMs as number,
                timeline_start_ms: args.timelineStartMs as number,
                text: { text: args.text as string, style_id: null },
              },
            ],
          });
        case "op_set_transform":
          return Promise.resolve(p);
        default:
          return Promise.reject(new Error("no tauri runtime under vitest"));
      }
    });
  }

  it("creates an overlay track, places the item and nudges it off the corner — as ONE undo entry", async () => {
    stubOps();
    render(<Timeline project={SAMPLE_PROJECT} />);

    await act(async () => {
      fireEvent.click(screen.getByTestId("timeline-add-text"));
    });

    // The waveform fetch fires on its own schedule — only the ops matter.
    const ops = invoke.mock.calls
      .map((c) => c[0] as string)
      .filter((c) => c.startsWith("op_"));
    expect(ops).toEqual([
      "op_add_track",
      "op_add_text_item",
      "op_set_transform",
    ]);
    // The overlay lands on the freshly created track, at the playhead.
    const opCalls = invoke.mock.calls.filter((c) =>
      (c[0] as string).startsWith("op_"),
    );
    expect(opCalls[1][1]).toMatchObject({
      trackId: "to1",
      timelineStartMs: 0,
    });
    // …and NOT at the frame's top-left corner, which is what the identity
    // transform would mean once the export anchors `\pos` there.
    const placed = (opCalls[2][1] as { transform: { x: number; y: number } })
      .transform;
    expect(placed.x).toBeGreaterThan(0);
    expect(placed.y).toBeGreaterThan(0);

    // One gesture, one undo step.
    expect(useProjectStore.getState().past).toHaveLength(1);
  });

  it("reuses an existing overlay track instead of creating another", async () => {
    stubOps();
    const withOverlay: Project = {
      ...SAMPLE_PROJECT,
      tracks: [
        ...SAMPLE_PROJECT.tracks,
        {
          id: "to-existing",
          kind: "overlay",
          name: "Overlay",
          index: 2,
          enabled: true,
          locked: false,
          muted: false,
          solo: false,
          volume_db: 0,
        },
      ],
    };
    useProjectStore.setState({ project: withOverlay, past: [], future: [] });
    render(<Timeline project={withOverlay} />);

    await act(async () => {
      fireEvent.click(screen.getByTestId("timeline-add-text"));
    });

    const ops = invoke.mock.calls.filter((c) =>
      (c[0] as string).startsWith("op_"),
    );
    expect(ops.map((c) => c[0])).toEqual([
      "op_add_text_item",
      "op_set_transform",
    ]);
    expect(ops[0][1]).toMatchObject({ trackId: "to-existing" });
  });

  it("surfaces a backend rejection instead of failing silently", async () => {
    invoke.mockImplementation((cmd: unknown) =>
      cmd === "op_add_track"
        ? Promise.reject(new Error("too many tracks"))
        : Promise.reject(new Error("no tauri runtime under vitest")),
    );
    render(<Timeline project={SAMPLE_PROJECT} />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("timeline-add-text"));
    });
    await waitFor(() =>
      expect(screen.getByRole("alert").textContent).toContain(
        "too many tracks",
      ),
    );
  });
});
