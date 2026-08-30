/**
 * Autosave + close guard (R1 trust round).
 *
 * The failure these prevent is the expensive one: SundayEdit sessions are
 * "transcribe a 60-minute sermon, then hand-correct 8% of the words", and
 * before this everything since the last manual ⌘S lived only in renderer
 * memory. The tests below pin the four properties that make write-behind safe
 * rather than merely present:
 *
 *   - it never invents a file (a never-saved project must still prompt),
 *   - it never writes mid-op (an in-flight round-trip has not committed yet),
 *   - it debounces, so the disk write is off the interaction path,
 *   - it records the EXACT snapshot written, so an edit landing during the
 *     write leaves the project correctly dirty instead of falsely clean.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";

import { AUTOSAVE_DEBOUNCE_MS, useAutosave, useCloseGuard } from "./autosave";
import { selectDirty, useProjectStore } from "./useProjectStore";
import { SAMPLE_PROJECT } from "./sampleProject";
import type { Project } from "./bindings";

// `tauriEnv` flips what `isTauri()` reports per test: everything here must be
// inert in the browser/E2E build, and that is itself a behaviour worth pinning.
let tauriEnv = true;
const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
  isTauri: () => tauriEnv,
}));

// The dynamic `import("@tauri-apps/api/window")` the close guard makes.
const onCloseRequested = vi.fn();
const destroy = vi.fn();
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ onCloseRequested, destroy }),
}));

const PATH = "/tmp/talk.sundayedit";

function Autosaver({ onError }: { onError?: (m: string) => void }) {
  useAutosave(onError);
  return null;
}

function Guard({ confirm }: { confirm: () => Promise<boolean> }) {
  useCloseGuard(confirm);
  return null;
}

/** The single `project_save` argument shape, so a drift in it fails here. */
function savedProjects(): Project[] {
  return invoke.mock.calls
    .filter(([cmd]) => cmd === "project_save")
    .map(([, args]) => (args as { project: Project }).project);
}

beforeEach(() => {
  tauriEnv = true;
  invoke.mockReset();
  invoke.mockResolvedValue(undefined);
  onCloseRequested.mockReset();
  onCloseRequested.mockResolvedValue(() => {});
  destroy.mockReset();
  useProjectStore.setState({
    project: null,
    past: [],
    future: [],
    busy: false,
    inFlight: false,
    savedSnapshot: null,
    filePath: null,
    saving: false,
  });
  vi.useFakeTimers();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

/** Let the debounce elapse and the save promise settle. */
async function settle() {
  await vi.advanceTimersByTimeAsync(AUTOSAVE_DEBOUNCE_MS + 10);
}

describe("useAutosave", () => {
  it("writes the dirty project back to its file after the debounce", async () => {
    useProjectStore.getState().reset(SAMPLE_PROJECT, PATH);
    render(<Autosaver />);

    useProjectStore.getState().commit((p) => ({ ...p, name: "edited" }));
    expect(savedProjects()).toHaveLength(0); // not on the interaction path

    await settle();

    expect(savedProjects()).toHaveLength(1);
    expect(savedProjects()[0].name).toBe("edited");
    expect(invoke).toHaveBeenCalledWith("project_save", {
      project: expect.objectContaining({ name: "edited" }),
      path: PATH,
    });
    // …and the document now reads clean.
    expect(selectDirty(useProjectStore.getState())).toBe(false);
    expect(useProjectStore.getState().saving).toBe(false);
  });

  it("debounces a burst into ONE write, carrying the LAST state", async () => {
    useProjectStore.getState().reset(SAMPLE_PROJECT, PATH);
    render(<Autosaver />);

    for (const name of ["a", "ab", "abc"]) {
      useProjectStore.getState().commit((p) => ({ ...p, name }));
      await vi.advanceTimersByTimeAsync(AUTOSAVE_DEBOUNCE_MS / 2);
    }
    await settle();

    expect(savedProjects()).toHaveLength(1);
    expect(savedProjects()[0].name).toBe("abc");
  });

  it("never writes a project that has no file — it must prompt instead", async () => {
    useProjectStore.getState().reset(SAMPLE_PROJECT); // imported, never saved
    render(<Autosaver />);

    useProjectStore.getState().commit((p) => ({ ...p, name: "edited" }));
    await settle();

    expect(savedProjects()).toHaveLength(0);
    expect(selectDirty(useProjectStore.getState())).toBe(true);
  });

  it("never writes a clean project", async () => {
    useProjectStore.getState().reset(SAMPLE_PROJECT, PATH);
    render(<Autosaver />);
    await settle();
    expect(savedProjects()).toHaveLength(0);
  });

  // The write must not overlap an op's round-trip: `run` captures the project
  // BEFORE its await, so persisting mid-flight stores a state the user is
  // already past, and the op's commit would land on top of a "saved" marker.
  it("does not fire while an op is in flight, and fires once it settles", async () => {
    useProjectStore.getState().reset(SAMPLE_PROJECT, PATH);
    render(<Autosaver />);
    useProjectStore.getState().commit((p) => ({ ...p, name: "edited" }));

    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const op = useProjectStore.getState().run(async (p) => {
      await gate;
      return { ...p, name: "from-op" };
    });

    await settle();
    expect(savedProjects()).toHaveLength(0); // blocked by inFlight

    release();
    await op;
    await settle();

    expect(savedProjects()).toHaveLength(1);
    expect(savedProjects()[0].name).toBe("from-op");
  });

  // Reference identity is the whole dirty mechanism: mark the snapshot that
  // actually reached disk, never "whatever the store holds when the write
  // returns" — otherwise the edit made during the write is reported saved.
  it("leaves the project dirty when an edit lands during the write", async () => {
    useProjectStore.getState().reset(SAMPLE_PROJECT, PATH);
    let releaseWrite!: () => void;
    invoke.mockImplementation(
      () => new Promise<void>((r) => (releaseWrite = () => r())),
    );
    render(<Autosaver />);

    useProjectStore.getState().commit((p) => ({ ...p, name: "v1" }));
    await vi.advanceTimersByTimeAsync(AUTOSAVE_DEBOUNCE_MS + 10);
    expect(useProjectStore.getState().saving).toBe(true);

    useProjectStore.getState().commit((p) => ({ ...p, name: "v2" }));
    releaseWrite();
    await vi.advanceTimersByTimeAsync(0);

    expect(useProjectStore.getState().project?.name).toBe("v2");
    expect(selectDirty(useProjectStore.getState())).toBe(true);
  });

  it("surfaces a failed write instead of silently claiming the work is safe", async () => {
    useProjectStore.getState().reset(SAMPLE_PROJECT, PATH);
    invoke.mockRejectedValue(new Error("disk full"));
    const onError = vi.fn();
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    render(<Autosaver onError={onError} />);

    useProjectStore.getState().commit((p) => ({ ...p, name: "edited" }));
    await settle();

    expect(onError).toHaveBeenCalledWith("disk full");
    expect(selectDirty(useProjectStore.getState())).toBe(true);
    expect(useProjectStore.getState().saving).toBe(false);
    spy.mockRestore();
  });

  it("is inert outside Tauri (browser dev / Playwright)", async () => {
    tauriEnv = false;
    useProjectStore.getState().reset(SAMPLE_PROJECT, PATH);
    render(<Autosaver />);
    useProjectStore.getState().commit((p) => ({ ...p, name: "edited" }));
    await settle();
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe("useCloseGuard", () => {
  /** Register the guard and return the handler Tauri would call. */
  async function mountGuard(confirm: () => Promise<boolean>) {
    render(<Guard confirm={confirm} />);
    await vi.waitFor(() => expect(onCloseRequested).toHaveBeenCalled());
    return onCloseRequested.mock.calls[0][0] as (e: {
      preventDefault: () => void;
    }) => Promise<void>;
  }

  it("lets a clean project close untouched", async () => {
    useProjectStore.getState().reset(SAMPLE_PROJECT, PATH);
    const confirm = vi.fn(async () => true);
    const handler = await mountGuard(confirm);

    const preventDefault = vi.fn();
    await handler({ preventDefault });

    expect(preventDefault).not.toHaveBeenCalled();
    expect(confirm).not.toHaveBeenCalled();
    expect(destroy).not.toHaveBeenCalled();
  });

  it("blocks the close and asks when there are unsaved changes", async () => {
    useProjectStore.getState().reset(SAMPLE_PROJECT, PATH);
    useProjectStore.getState().commit((p) => ({ ...p, name: "edited" }));
    const confirm = vi.fn(async () => false); // "Keep editing"
    const handler = await mountGuard(confirm);

    const preventDefault = vi.fn();
    await handler({ preventDefault });

    expect(preventDefault).toHaveBeenCalled();
    expect(confirm).toHaveBeenCalled();
    expect(destroy).not.toHaveBeenCalled(); // the window stays open
  });

  it("closes anyway once the user confirms the discard", async () => {
    useProjectStore.getState().reset(SAMPLE_PROJECT, PATH);
    useProjectStore.getState().commit((p) => ({ ...p, name: "edited" }));
    const handler = await mountGuard(async () => true);

    await handler({ preventDefault: vi.fn() });
    expect(destroy).toHaveBeenCalled();
  });

  it("registers nothing outside Tauri", async () => {
    tauriEnv = false;
    render(<Guard confirm={async () => true} />);
    await vi.advanceTimersByTimeAsync(10);
    expect(onCloseRequested).not.toHaveBeenCalled();
  });
});
