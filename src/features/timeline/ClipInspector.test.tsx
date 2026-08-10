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
