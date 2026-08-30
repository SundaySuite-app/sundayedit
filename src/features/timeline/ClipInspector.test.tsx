import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  render,
  cleanup,
  screen,
  fireEvent,
  act,
} from "@testing-library/react";

import { ClipInspector } from "./ClipInspector";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import { useProjectStore } from "@/lib/useProjectStore";
import { useLocale } from "@/lib/i18n";
import type { Project, TimelineItem, Transform } from "@/lib/bindings";

// Mock the lowest layer (Tauri invoke) so the real typed `ipc` wrappers run —
// this pins the timeline-op command names + argument shapes the panel relies on.
const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));

// The sample project's only timeline item: in 0 → out 18000 on track tv.
const ITEM = SAMPLE_PROJECT.timeline_items[0];

/** A second clip with different trim points, for selection-switch tests. */
const OTHER_ITEM: TimelineItem = {
  ...ITEM,
  id: "ti2",
  in_ms: 500,
  out_ms: 9_000,
};

/** ITEM with a leading fade so the duration field renders. */
const FADED_ITEM: TimelineItem = {
  ...ITEM,
  transition_in: { kind: "fade", duration_ms: 500 },
};

beforeEach(() => {
  invoke.mockReset();
  useLocale.setState({ lang: "en" });
  // Every edit commits through the shared store; seed a clean project.
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

function renderInspector(item: TimelineItem = ITEM) {
  return render(<ClipInspector item={item} onClose={vi.fn()} />);
}

const trimIn = () => screen.getByLabelText("In (ms)") as HTMLInputElement;
const trimOut = () => screen.getByLabelText("Out (ms)") as HTMLInputElement;

describe("ClipInspector — trim buffer semantics", () => {
  it("resets the trim fields when the selected clip changes, discarding unsaved typing", () => {
    const { rerender } = renderInspector();
    expect(trimIn().value).toBe("0");
    expect(trimOut().value).toBe("18000");

    // Type into the buffer WITHOUT committing (no blur) …
    fireEvent.change(trimIn(), { target: { value: "999" } });
    expect(trimIn().value).toBe("999");

    // … then switch clips: the buffers must reset to the new clip's values.
    rerender(<ClipInspector item={OTHER_ITEM} onClose={vi.fn()} />);
    expect(trimIn().value).toBe("500");
    expect(trimOut().value).toBe("9000");
    // Nothing was committed along the way.
    expect(invoke).not.toHaveBeenCalled();
  });

  it("commits a typed value exactly once via op_trim_timeline_item on blur", async () => {
    const next: Project = { ...SAMPLE_PROJECT, updated_at: 1 };
    invoke.mockResolvedValueOnce(next);
    renderInspector();

    fireEvent.change(trimIn(), { target: { value: "250" } });
    fireEvent.blur(trimIn());

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("op_trim_timeline_item", {
        project: SAMPLE_PROJECT,
        itemId: "ti1",
        newInMs: 250,
        newOutMs: null,
        newTimelineStartMs: null,
      }),
    );
    expect(invoke).toHaveBeenCalledTimes(1);
    // The op lands on the shared undo stack via store.run.
    await vi.waitFor(() =>
      expect(useProjectStore.getState().project).toEqual(next),
    );
    expect(useProjectStore.getState().past).toEqual([SAMPLE_PROJECT]);
  });

  it("commits the out edge independently", async () => {
    invoke.mockResolvedValueOnce({ ...SAMPLE_PROJECT, updated_at: 2 });
    renderInspector();

    fireEvent.change(trimOut(), { target: { value: "12000" } });
    fireEvent.blur(trimOut());

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("op_trim_timeline_item", {
        project: SAMPLE_PROJECT,
        itemId: "ti1",
        newInMs: null,
        newOutMs: 12_000,
        newTimelineStartMs: null,
      }),
    );
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("does NOT round-trip when the value is unchanged on blur", () => {
    renderInspector();

    // Untouched blur.
    fireEvent.blur(trimIn());
    // Re-typing the committed value is also a no-op.
    fireEvent.change(trimOut(), { target: { value: "18000" } });
    fireEvent.blur(trimOut());

    expect(invoke).not.toHaveBeenCalled();
  });
});

describe("ClipInspector — delete (ripple)", () => {
  it("commits op_ripple_delete_item for the inspected clip and closes the panel", async () => {
    invoke.mockResolvedValueOnce({ ...SAMPLE_PROJECT, updated_at: 3 });
    const onClose = vi.fn();
    render(<ClipInspector item={ITEM} onClose={onClose} />);

    fireEvent.click(screen.getByTestId("inspector-delete"));

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("op_ripple_delete_item", {
        project: SAMPLE_PROJECT,
        itemId: ITEM.id,
      }),
    );
    await vi.waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    // The op lands on the shared undo stack via store.run, same as trim/split.
    expect(useProjectStore.getState().past).toEqual([SAMPLE_PROJECT]);
  });

  it("leaves the project untouched and does not close when the backend rejects", async () => {
    invoke.mockRejectedValueOnce(
      Object.assign(new Error("nope"), { code: "not_found" }),
    );
    const onClose = vi.fn();
    render(<ClipInspector item={ITEM} onClose={onClose} />);

    fireEvent.click(screen.getByTestId("inspector-delete"));

    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    expect(onClose).not.toHaveBeenCalled();
    expect(useProjectStore.getState().project).toBe(SAMPLE_PROJECT);
  });
});

// ── Audio (R2) ────────────────────────────────────────────────────────────
describe("ClipInspector — audio section visibility", () => {
  it("shows gain + fades for an audio-bearing av clip", () => {
    renderInspector();
    expect(screen.getByTestId("inspector-gain")).not.toBeNull();
    expect(screen.getByLabelText("Fade in (ms)")).not.toBeNull();
    expect(screen.getByLabelText("Fade out (ms)")).not.toBeNull();
  });

  it("hides the audio section when the clip's own media has no audio stream", () => {
    const silentProject: Project = {
      ...SAMPLE_PROJECT,
      media: [{ ...SAMPLE_PROJECT.media[0], id: "m-silent", has_audio: false }],
    };
    useProjectStore.setState({ project: silentProject });
    const silentItem: TimelineItem = {
      ...ITEM,
      id: "ti-silent",
      source_media_id: "m-silent",
    };
    renderInspector(silentItem);
    expect(screen.queryByTestId("inspector-gain")).toBeNull();
    expect(screen.queryByLabelText("Fade in (ms)")).toBeNull();
  });

  it("hides the audio section for a text overlay clip (no source media at all)", () => {
    const textItem: TimelineItem = {
      ...ITEM,
      id: "ti-text",
      kind: "text",
      source_media_id: null,
      text: { text: "Hello", style_id: null },
    };
    renderInspector(textItem);
    expect(screen.queryByTestId("inspector-gain")).toBeNull();
  });
});

describe("ClipInspector — gain slider", () => {
  /** Apply an op_set_item_audio payload the way the backend would. */
  function applySetItemAudio(rawArgs: unknown): Project {
    const args = rawArgs as {
      project: Project;
      itemId: string;
      gainDb: number | null;
      fadeInMs: number | null;
      fadeOutMs: number | null;
    };
    return {
      ...args.project,
      updated_at: args.project.updated_at + 1,
      timeline_items: args.project.timeline_items.map((ti) =>
        ti.id === args.itemId
          ? {
              ...ti,
              gain_db: args.gainDb ?? ti.gain_db,
              fade_in_ms: args.fadeInMs ?? ti.fade_in_ms,
              fade_out_ms: args.fadeOutMs ?? ti.fade_out_ms,
            }
          : ti,
      ),
    };
  }

  const gainSlider = () =>
    screen.getByTestId("inspector-gain") as HTMLInputElement;

  it("drags through several ticks but lands ONE undo entry at the release value", async () => {
    invoke.mockImplementation((cmd: unknown, rawArgs: unknown) => {
      expect(cmd).toBe("op_set_item_audio");
      return Promise.resolve(applySetItemAudio(rawArgs));
    });
    renderInspector();

    fireEvent.change(gainSlider(), { target: { value: "-6" } });
    fireEvent.change(gainSlider(), { target: { value: "-3" } });
    fireEvent.change(gainSlider(), { target: { value: "2" } });

    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(3));
    await vi.waitFor(() =>
      expect(
        useProjectStore.getState().project?.timeline_items[0].gain_db,
      ).toBe(2),
    );
    // ONE undo step for the whole drag, not one per tick.
    expect(useProjectStore.getState().past).toEqual([SAMPLE_PROJECT]);
  });

  it("snaps a drag near 0 dB to exactly unity (the detent)", async () => {
    invoke.mockImplementation((_cmd: unknown, rawArgs: unknown) =>
      Promise.resolve(applySetItemAudio(rawArgs)),
    );
    renderInspector();

    fireEvent.change(gainSlider(), { target: { value: "0.2" } });

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("op_set_item_audio", {
        project: SAMPLE_PROJECT,
        itemId: "ti1",
        gainDb: 0,
        fadeInMs: null,
        fadeOutMs: null,
      }),
    );
  });

  it("hides the reset button at 0 dB and shows it once dragged away", () => {
    const { unmount } = renderInspector();
    expect(screen.queryByTestId("inspector-gain-reset")).toBeNull();
    unmount();

    renderInspector({ ...ITEM, gain_db: -6 });
    expect(screen.queryByTestId("inspector-gain-reset")).not.toBeNull();
  });

  it("the reset button commits exactly 0 dB", async () => {
    invoke.mockImplementation((_cmd: unknown, rawArgs: unknown) =>
      Promise.resolve(applySetItemAudio(rawArgs)),
    );
    renderInspector({ ...ITEM, gain_db: -6 });

    fireEvent.click(screen.getByTestId("inspector-gain-reset"));

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("op_set_item_audio", {
        project: SAMPLE_PROJECT,
        itemId: "ti1",
        gainDb: 0,
        fadeInMs: null,
        fadeOutMs: null,
      }),
    );
  });
});

describe("ClipInspector — combined-gain honesty (preview can't boost past unity)", () => {
  it("shows no warning when the combined level is unity or quieter", () => {
    const { unmount } = renderInspector({ ...ITEM, gain_db: 0 });
    expect(screen.queryByTestId("inspector-gain-clipped")).toBeNull();
    unmount();

    renderInspector({ ...ITEM, gain_db: -6 });
    expect(screen.queryByTestId("inspector-gain-clipped")).toBeNull();
  });

  it("warns with the combined dB figure when the clip's OWN gain is positive", () => {
    renderInspector({ ...ITEM, gain_db: 6 });
    const note = screen.getByTestId("inspector-gain-clipped");
    expect(note.textContent).toContain("+6.0 dB");
  });

  it("counts the track's fader too, not just the clip's own gain", () => {
    // Clip gain alone is unity, but the TRACK ("tv") is boosted +3 dB — the
    // combined level the export will render is what must be judged.
    useProjectStore.setState({
      project: {
        ...SAMPLE_PROJECT,
        tracks: SAMPLE_PROJECT.tracks.map((tr) =>
          tr.id === "tv" ? { ...tr, volume_db: 3 } : tr,
        ),
      },
    });
    renderInspector({ ...ITEM, gain_db: 0 });
    const note = screen.getByTestId("inspector-gain-clipped");
    expect(note.textContent).toContain("+3.0 dB");
  });
});

describe("ClipInspector — fades", () => {
  const fadeIn = () =>
    screen.getByLabelText("Fade in (ms)") as HTMLInputElement;
  const fadeOut = () =>
    screen.getByLabelText("Fade out (ms)") as HTMLInputElement;

  it("always states fades are applied at export, not previewed", () => {
    renderInspector();
    expect(screen.getByText(/render(ed)? at export/i)).not.toBeNull();
  });

  it("commits fade-in via op_set_item_audio on blur, leaving fade-out alone", async () => {
    const next: Project = { ...SAMPLE_PROJECT, updated_at: 1 };
    invoke.mockResolvedValueOnce(next);
    renderInspector();

    fireEvent.change(fadeIn(), { target: { value: "500" } });
    fireEvent.blur(fadeIn());

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("op_set_item_audio", {
        project: SAMPLE_PROJECT,
        itemId: "ti1",
        gainDb: null,
        fadeInMs: 500,
        fadeOutMs: null,
      }),
    );
    expect(invoke).toHaveBeenCalledTimes(1);
    // The op lands on the shared undo stack via store.run, same as trim.
    await vi.waitFor(() =>
      expect(useProjectStore.getState().project).toEqual(next),
    );
    expect(useProjectStore.getState().past).toEqual([SAMPLE_PROJECT]);
  });

  it("commits fade-out independently", async () => {
    invoke.mockResolvedValueOnce({ ...SAMPLE_PROJECT, updated_at: 1 });
    renderInspector();

    fireEvent.change(fadeOut(), { target: { value: "300" } });
    fireEvent.blur(fadeOut());

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("op_set_item_audio", {
        project: SAMPLE_PROJECT,
        itemId: "ti1",
        gainDb: null,
        fadeInMs: null,
        fadeOutMs: 300,
      }),
    );
  });

  it("does not round-trip when a fade value is unchanged on blur", () => {
    renderInspector();
    fireEvent.blur(fadeIn());
    fireEvent.change(fadeOut(), { target: { value: "0" } });
    fireEvent.blur(fadeOut());
    expect(invoke).not.toHaveBeenCalled();
  });

  it("resets the fade buffers when the selected clip changes", () => {
    const { rerender } = renderInspector();
    fireEvent.change(fadeIn(), { target: { value: "999" } });
    expect(fadeIn().value).toBe("999");

    rerender(<ClipInspector item={OTHER_ITEM} onClose={vi.fn()} />);
    expect(fadeIn().value).toBe(String(OTHER_ITEM.fade_in_ms));
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe("ClipInspector — transition", () => {
  it("hides the duration field when no transition is set (no op possible)", () => {
    renderInspector();
    expect(screen.queryByLabelText("Duration (ms)")).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("picking a kind commits op_set_transition with the default duration", async () => {
    invoke.mockResolvedValueOnce({ ...SAMPLE_PROJECT, updated_at: 3 });
    renderInspector();

    fireEvent.change(screen.getByLabelText("Type"), {
      target: { value: "fade" },
    });

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("op_set_transition", {
        project: SAMPLE_PROJECT,
        itemId: "ti1",
        kind: "fade",
        durationMs: 500,
      }),
    );
  });

  it("editing the duration commits op_set_transition with the existing kind", async () => {
    invoke.mockResolvedValueOnce({ ...SAMPLE_PROJECT, updated_at: 4 });
    renderInspector(FADED_ITEM);

    const duration = screen.getByLabelText("Duration (ms)") as HTMLInputElement;
    expect(duration.value).toBe("500");
    fireEvent.change(duration, { target: { value: "750" } });
    fireEvent.blur(duration);

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("op_set_transition", {
        project: SAMPLE_PROJECT,
        itemId: "ti1",
        kind: "fade",
        durationMs: 750,
      }),
    );
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("selecting 'None' clears the transition via op_clear_transition", async () => {
    invoke.mockResolvedValueOnce({ ...SAMPLE_PROJECT, updated_at: 5 });
    renderInspector(FADED_ITEM);

    fireEvent.change(screen.getByLabelText("Type"), { target: { value: "" } });

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("op_clear_transition", {
        project: SAMPLE_PROJECT,
        itemId: "ti1",
      }),
    );
  });
});

// Regression (seam-xfade-transition-vocabulary): the picker used to offer
// "crossfade" and "dip", which are NOT ffmpeg `xfade` transition names — the
// kind string is stored verbatim and emitted as `xfade=transition={kind}`, so
// 2 of the 3 options aborted the export at render time. Every option value
// must be a real xfade name; friendly wording lives in the LABEL only. The
// authoritative picker↔ffmpeg check (against the bundled ffmpeg's own enum)
// is src-tauri/tests/compose_xfade_vocabulary.rs — this pins the frontend end.
describe("ClipInspector — transition vocabulary is real xfade names", () => {
  // ffmpeg 6.0 `xfade` enum (subset is fine — every option must be in it).
  const XFADE_NAMES = new Set([
    "fade",
    "wipeleft",
    "wiperight",
    "wipeup",
    "wipedown",
    "slideleft",
    "slideright",
    "slideup",
    "slidedown",
    "circlecrop",
    "rectcrop",
    "distance",
    "fadeblack",
    "fadewhite",
    "radial",
    "smoothleft",
    "smoothright",
    "smoothup",
    "smoothdown",
    "circleopen",
    "circleclose",
    "vertopen",
    "vertclose",
    "horzopen",
    "horzclose",
    "dissolve",
    "pixelize",
    "diagtl",
    "diagtr",
    "diagbl",
    "diagbr",
    "hlslice",
    "hrslice",
    "vuslice",
    "vdslice",
    "hblur",
    "fadegrays",
    "wipetl",
    "wipetr",
    "wipebl",
    "wipebr",
    "squeezeh",
    "squeezev",
    "zoomin",
    "fadefast",
    "fadeslow",
  ]);

  it("every non-empty option value is a name ffmpeg's xfade filter accepts", () => {
    renderInspector();
    const select = screen.getByLabelText("Type") as HTMLSelectElement;
    const values = Array.from(select.options)
      .map((o) => o.value)
      .filter((v) => v !== "");
    expect(values.length).toBeGreaterThan(0);
    for (const v of values) {
      expect(XFADE_NAMES.has(v), `picker offers non-xfade kind ${v}`).toBe(
        true,
      );
    }
    // The two invalid friendly names must never come back.
    expect(values).not.toContain("crossfade");
    expect(values).not.toContain("dip");
  });

  it("shows a legacy 'crossfade' project kind as its real name and re-commits it normalized", async () => {
    invoke.mockResolvedValueOnce({ ...SAMPLE_PROJECT, updated_at: 6 });
    renderInspector({
      ...ITEM,
      transition_in: { kind: "crossfade", duration_ms: 400 },
    });

    // Displayed as the xfade name the backend actually renders it as.
    const select = screen.getByLabelText("Type") as HTMLSelectElement;
    expect(select.value).toBe("dissolve");

    // Editing the duration self-heals the stored kind to the real name.
    const duration = screen.getByLabelText("Duration (ms)") as HTMLInputElement;
    fireEvent.change(duration, { target: { value: "600" } });
    fireEvent.blur(duration);

    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("op_set_transition", {
        project: SAMPLE_PROJECT,
        itemId: "ti1",
        kind: "dissolve",
        durationMs: 600,
      }),
    );
  });

  it("an unknown hand-edited kind still displays as itself instead of snapping to another option", () => {
    renderInspector({
      ...ITEM,
      transition_in: { kind: "wipeup", duration_ms: 300 },
    });
    const select = screen.getByLabelText("Type") as HTMLSelectElement;
    expect(select.value).toBe("wipeup");
  });
});

// Regression (state-slider-commits-dropped-by-inflight-guard): the Transform
// sliders commit through store.run on every input tick of a drag. Ticks that
// arrive while the previous tick's IPC round-trip is still pending — including
// the final tick at pointer release — used to be silently dropped by the
// inFlight guard, leaving the committed transform stuck at an intermediate
// value. The store now queues the newest in-flight op (latest wins).
describe("ClipInspector — transform sliders vs in-flight commits", () => {
  /** Apply an op_set_transform payload the way the backend would. */
  function applySetTransform(args: {
    project: Project;
    itemId: string;
    transform: Transform;
  }): Project {
    return {
      ...args.project,
      updated_at: args.project.updated_at + 1,
      timeline_items: args.project.timeline_items.map((ti) =>
        ti.id === args.itemId ? { ...ti, transform: args.transform } : ti,
      ),
    };
  }

  it("commits the slider's release value even when earlier ticks are still in flight", async () => {
    // First IPC round-trip is held open (in flight) until we release it —
    // simulating the latency window of a real Tauri invoke. Later calls
    // resolve immediately.
    let releaseFirst: (() => void) | undefined;
    let call = 0;
    invoke.mockImplementation((cmd: unknown, rawArgs: unknown) => {
      expect(cmd).toBe("op_set_transform");
      const args = rawArgs as {
        project: Project;
        itemId: string;
        transform: Transform;
      };
      const next = applySetTransform(args);
      call += 1;
      if (call === 1) {
        return new Promise<Project>((resolve) => {
          releaseFirst = () => resolve(next);
        });
      }
      return Promise.resolve(next);
    });

    renderInspector();
    // The label's accessible name includes the live readout ("Scale1.00").
    const scale = screen.getByLabelText(/^Scale/) as HTMLInputElement;
    expect(scale.value).toBe("1");

    // Drag: tick 1 starts the IPC round-trip (held in flight) …
    fireEvent.change(scale, { target: { value: "1.15" } });
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(releaseFirst).toBeDefined();

    // … ticks 2 and 3 (3 = pointer release at 2.00) arrive while it's pending.
    fireEvent.change(scale, { target: { value: "1.6" } });
    fireEvent.change(scale, { target: { value: "2" } });

    // The first round-trip completes.
    await act(async () => {
      releaseFirst?.();
    });

    // The user released at 2.00 — that value must end up committed.
    await vi.waitFor(() => {
      const project = useProjectStore.getState().project;
      const committed = project?.timeline_items.find((ti) => ti.id === ITEM.id)
        ?.transform.scale;
      expect(committed).toBe(2);
    });
  });
});

// ── Curated effects (E6) ─────────────────────────────────────────────────────
// The inspector is the UI half of "non-curated effects must not be selectable":
// it renders the registry and nothing else, and every edit goes through
// store.run so effects land on the SAME undo stack as trim/transform.
describe("ClipInspector — effects section", () => {
  /** Apply an op_set_effect payload the way the backend would. */
  function applySetEffect(args: {
    project: Project;
    itemId: string;
    kind: string;
    params: Record<string, number>;
    enabled: boolean;
  }): Project {
    return {
      ...args.project,
      updated_at: args.project.updated_at + 1,
      timeline_items: args.project.timeline_items.map((ti) =>
        ti.id === args.itemId
          ? {
              ...ti,
              effects: [
                ...ti.effects.filter((e) => e.kind !== args.kind),
                {
                  id: `fx-${args.kind}`,
                  kind: args.kind,
                  params: args.params,
                  enabled: args.enabled,
                },
              ],
            }
          : ti,
      ),
    };
  }

  const rowFor = (id: string) => screen.getByTestId(`effect-${id}`);
  const checkboxIn = (id: string) =>
    rowFor(id).querySelector("input[type=checkbox]") as HTMLInputElement;

  it("offers exactly the curated effects — no more, no less", () => {
    renderInspector();
    for (const id of ["brightness", "contrast", "saturation", "grayscale"]) {
      expect(rowFor(id)).toBeTruthy();
    }
    // An effect nobody curated (and the export cannot render) is not offerable.
    expect(screen.queryByTestId("effect-bloom")).toBeNull();
    expect(screen.queryByTestId("effect-blur")).toBeNull();
  });

  it("shows every effect off for a clip with no effects", () => {
    renderInspector();
    expect(checkboxIn("brightness").checked).toBe(false);
    expect(checkboxIn("grayscale").checked).toBe(false);
    // No slider until the effect is on.
    expect(rowFor("brightness").querySelector("input[type=range]")).toBeNull();
  });

  it("adds an effect at its NEUTRAL default via op_set_effect", async () => {
    invoke.mockImplementation((_cmd: unknown, rawArgs: unknown) =>
      Promise.resolve(
        applySetEffect(rawArgs as Parameters<typeof applySetEffect>[0]),
      ),
    );
    renderInspector();

    await act(async () => {
      fireEvent.click(checkboxIn("brightness"));
    });

    expect(invoke).toHaveBeenLastCalledWith("op_set_effect", {
      project: SAMPLE_PROJECT,
      itemId: ITEM.id,
      kind: "brightness",
      params: { amount: 0 },
      enabled: true,
    });
  });

  it("removes an effect via op_remove_effect when unchecked", async () => {
    const withEffect: TimelineItem = {
      ...ITEM,
      effects: [
        { id: "fx-grayscale", kind: "grayscale", params: {}, enabled: true },
      ],
    };
    useProjectStore.setState({
      project: { ...SAMPLE_PROJECT, timeline_items: [withEffect] },
    });
    invoke.mockResolvedValue({ ...SAMPLE_PROJECT, updated_at: 9 });
    render(<ClipInspector item={withEffect} onClose={vi.fn()} />);

    expect(checkboxIn("grayscale").checked).toBe(true);
    await act(async () => {
      fireEvent.click(checkboxIn("grayscale"));
    });
    expect(invoke).toHaveBeenLastCalledWith("op_remove_effect", {
      project: { ...SAMPLE_PROJECT, timeline_items: [withEffect] },
      itemId: ITEM.id,
      kind: "grayscale",
    });
  });

  it("commits a slider change with the parameter's value", async () => {
    const withEffect: TimelineItem = {
      ...ITEM,
      effects: [
        {
          id: "fx-contrast",
          kind: "contrast",
          params: { amount: 1 },
          enabled: true,
        },
      ],
    };
    useProjectStore.setState({
      project: { ...SAMPLE_PROJECT, timeline_items: [withEffect] },
    });
    invoke.mockImplementation((_cmd: unknown, rawArgs: unknown) =>
      Promise.resolve(
        applySetEffect(rawArgs as Parameters<typeof applySetEffect>[0]),
      ),
    );
    render(<ClipInspector item={withEffect} onClose={vi.fn()} />);

    const slider = rowFor("contrast").querySelector(
      "input[type=range]",
    ) as HTMLInputElement;
    await act(async () => {
      fireEvent.change(slider, { target: { value: "1.75" } });
    });

    expect(invoke).toHaveBeenLastCalledWith("op_set_effect", {
      project: { ...SAMPLE_PROJECT, timeline_items: [withEffect] },
      itemId: ITEM.id,
      kind: "contrast",
      params: { amount: 1.75 },
      enabled: true,
    });
  });

  it("shows the exact ffmpeg filter the clip will export with", () => {
    // The preview is an approximation (registry.ts); showing the real filter
    // is how the panel stays honest about what the export will do.
    const withEffect: TimelineItem = {
      ...ITEM,
      effects: [
        {
          id: "fx-saturation",
          kind: "saturation",
          params: { amount: 1.4 },
          enabled: true,
        },
      ],
    };
    useProjectStore.setState({
      project: { ...SAMPLE_PROJECT, timeline_items: [withEffect] },
    });
    render(<ClipInspector item={withEffect} onClose={vi.fn()} />);
    expect(screen.getByTestId("effect-saturation-fragment").textContent).toBe(
      "eq=saturation=1.4",
    );
  });

  it("says so when an enabled effect is parked at a value that changes nothing", () => {
    const withEffect: TimelineItem = {
      ...ITEM,
      effects: [
        {
          id: "fx-brightness",
          kind: "brightness",
          params: { amount: 0 },
          enabled: true,
        },
      ],
    };
    useProjectStore.setState({
      project: { ...SAMPLE_PROJECT, timeline_items: [withEffect] },
    });
    render(<ClipInspector item={withEffect} onClose={vi.fn()} />);
    expect(screen.getByTestId("effect-brightness-fragment").textContent).toBe(
      "No change at this value",
    );
  });

  it("is not offered on a clip the export will not grade", () => {
    // `compose::is_visual` runs the per-item effect chain only on clips backed
    // by VIDEO media. Offering effects on a text overlay would store an `eq=`
    // the render then ignores — exactly the promise-the-export-can't-keep the
    // curated registry exists to prevent.
    const textItem: TimelineItem = {
      ...ITEM,
      id: "tx1",
      kind: "text",
      source_media_id: null,
      text: { text: "Lower third", style_id: null },
    };
    useProjectStore.setState({
      project: { ...SAMPLE_PROJECT, timeline_items: [textItem] },
    });
    render(<ClipInspector item={textItem} onClose={vi.fn()} />);
    expect(screen.queryByTestId("effect-brightness")).toBeNull();
    // The rest of the panel is unaffected.
    expect(screen.getByTestId("clip-inspector")).toBeTruthy();
  });

  it("restores the stored value when a disabled effect is re-enabled", async () => {
    // A project file can carry `enabled: false` with real params; re-checking
    // the box must not silently reset the user's grade to neutral.
    const withDisabled: TimelineItem = {
      ...ITEM,
      effects: [
        {
          id: "fx-saturation",
          kind: "saturation",
          params: { amount: 2.5 },
          enabled: false,
        },
      ],
    };
    useProjectStore.setState({
      project: { ...SAMPLE_PROJECT, timeline_items: [withDisabled] },
    });
    invoke.mockImplementation((_cmd: unknown, rawArgs: unknown) =>
      Promise.resolve(
        applySetEffect(rawArgs as Parameters<typeof applySetEffect>[0]),
      ),
    );
    render(<ClipInspector item={withDisabled} onClose={vi.fn()} />);

    await act(async () => {
      fireEvent.click(checkboxIn("saturation"));
    });
    expect(invoke).toHaveBeenLastCalledWith(
      "op_set_effect",
      expect.objectContaining({
        kind: "saturation",
        params: { amount: 2.5 },
        enabled: true,
      }),
    );
  });

  it("leaves the project untouched when the backend rejects the edit", async () => {
    invoke.mockRejectedValue(new Error("validation"));
    renderInspector();
    await act(async () => {
      fireEvent.click(checkboxIn("grayscale"));
    });
    expect(
      useProjectStore.getState().project?.timeline_items[0].effects,
    ).toEqual([]);
  });
});
