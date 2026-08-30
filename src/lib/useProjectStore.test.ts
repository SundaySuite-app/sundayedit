import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  COALESCE_WINDOW_MS,
  useProjectStore,
  selectAutosavable,
  selectCanUndo,
  selectCanRedo,
  selectDirty,
} from "./useProjectStore";
import { SAMPLE_PROJECT } from "./sampleProject";
import type { Project } from "./bindings";

/** Direct access to store actions/state (no React needed — it's a plain store). */
function store() {
  return useProjectStore.getState();
}

beforeEach(() => {
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
});

describe("useProjectStore", () => {
  it("run() commits, capping history and clearing the redo stack", async () => {
    store().reset(SAMPLE_PROJECT);
    const v1: Project = { ...SAMPLE_PROJECT, updated_at: 1 };
    await store().run(async () => v1);

    expect(store().project).toBe(v1);
    expect(selectCanUndo(store())).toBe(true);
    expect(selectCanRedo(store())).toBe(false);
  });

  it("reset() replaces the project and clears both history stacks", async () => {
    store().reset(SAMPLE_PROJECT);
    await store().run(async () => ({ ...SAMPLE_PROJECT, updated_at: 1 }));
    store().undo(); // leaves something in `future`

    const fresh: Project = { ...SAMPLE_PROJECT, id: "other" };
    store().reset(fresh);
    expect(store().project).toBe(fresh);
    expect(store().past).toEqual([]);
    expect(store().future).toEqual([]);
  });

  it("setProject() replaces without touching history (accepts an updater)", async () => {
    store().reset(SAMPLE_PROJECT);
    await store().run(async () => ({ ...SAMPLE_PROJECT, updated_at: 1 }));
    const pastLen = store().past.length;

    store().setProject((prev) => (prev ? { ...prev, name: "renamed" } : prev));
    expect(store().project?.name).toBe("renamed");
    // Direct set is non-undoable: the undo stack is untouched.
    expect(store().past.length).toBe(pastLen);
  });

  // The exact bug this store fixes: caption edits and timeline drags used to
  // live in separate stores, so they diverged and only caption edits were
  // undoable. Now BOTH flow through one `run`/undo stack — so undoing twice
  // after a caption op then a timeline op reverts each in turn.
  it("shares ONE undo stack across caption ops and timeline ops", async () => {
    const v0 = SAMPLE_PROJECT;
    store().reset(v0);

    // 1) A caption op (CaptionEditor-style): edit the first word's text.
    const afterCaptionEdit = (p: Project): Project => ({
      ...p,
      captions: p.captions.map((c, i) =>
        i === 0
          ? {
              ...c,
              words: c.words.map((w, wi) =>
                wi === 0 ? { ...w, text: "EDITED", edited: true } : w,
              ),
            }
          : c,
      ),
    });
    await store().run(async (p) => afterCaptionEdit(p));
    const v1 = store().project!;
    expect(v1.captions[0].words[0].text).toBe("EDITED");

    // 2) A timeline op (Timeline-style): move the first caption's start.
    const afterTimelineMove = (p: Project): Project => ({
      ...p,
      captions: p.captions.map((c, i) =>
        i === 0 ? { ...c, start_ms: c.start_ms + 500 } : c,
      ),
    });
    await store().run(async (p) => afterTimelineMove(p));
    const v2 = store().project!;
    expect(v2.captions[0].start_ms).toBe(v0.captions[0].start_ms + 500);
    // The caption edit is still present on top of the timeline move.
    expect(v2.captions[0].words[0].text).toBe("EDITED");

    // Undo #1 reverts the timeline move — caption edit survives.
    store().undo();
    expect(store().project).toBe(v1);
    expect(store().project!.captions[0].start_ms).toBe(v0.captions[0].start_ms);
    expect(store().project!.captions[0].words[0].text).toBe("EDITED");

    // Undo #2 reverts the caption edit — back to the original project.
    store().undo();
    expect(store().project).toBe(v0);
    expect(store().project!.captions[0].words[0].text).toBe(
      v0.captions[0].words[0].text,
    );

    // Redo walks forward through the same shared stack.
    store().redo();
    expect(store().project).toBe(v1);
    store().redo();
    expect(store().project).toBe(v2);
  });

  it("run() queues the NEWEST op that arrives while one is in flight (latest wins)", async () => {
    store().reset(SAMPLE_PROJECT);
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));

    const first = store().run(async (p) => {
      await gate;
      return { ...p, updated_at: 1 };
    });
    // Two calls arrive before the first resolves → they coalesce; only the
    // NEWEST runs once the first settles (slider ticks: release value wins).
    await store().run(async (p) => ({ ...p, updated_at: 500 }));
    await store().run(async (p) => ({ ...p, updated_at: 999 }));
    expect(store().project?.updated_at).toBe(0); // nothing committed yet

    release();
    await first; // resolves only after the queued op also settled
    expect(store().project?.updated_at).toBe(999);
    expect(store().past.length).toBe(2); // first commit + the queued commit
  });

  // Regression (state-run-commit-clobbers-concurrent-state): run() captures
  // the project before its await; the commit must be compare-and-swap so a
  // concurrent reset/setProject/undo landing mid-round-trip is not silently
  // overwritten by the stale result.
  describe("run() vs concurrent store changes (compare-and-swap commit)", () => {
    // Instance A: transcription finishes (TranscribePanel → setProject with new
    // captions) while a clip-drag commit's IPC round-trip is in flight. The
    // in-flight run must not overwrite the store with its pre-transcription
    // snapshot + moved clip.
    it("does not clobber a setProject() (transcription result) that lands mid-run", async () => {
      store().reset(SAMPLE_PROJECT);

      let release!: () => void;
      const gate = new Promise<void>((r) => (release = r));

      // A slow "timeline drag commit" op derived from the pre-await snapshot.
      const inFlight = store().run(async (p) => {
        await gate;
        return {
          ...p,
          captions: p.captions.map((c, i) =>
            i === 0 ? { ...c, start_ms: c.start_ms + 500 } : c,
          ),
        };
      });

      // Transcription lands while the drag commit is in flight.
      const transcribed: Project = {
        ...store().project!,
        captions: [
          ...store().project!.captions,
          {
            ...store().project!.captions[0],
            id: "cap-transcribed",
            start_ms: 99_000,
            end_ms: 99_500,
          },
        ],
      };
      store().setProject(transcribed);
      const capCountAfterTranscribe = store().project!.captions.length;

      release();
      await inFlight;

      // The transcription result must survive the stale commit.
      expect(
        store().project!.captions.some((c) => c.id === "cap-transcribed"),
      ).toBe(true);
      expect(store().project!.captions.length).toBe(capCountAfterTranscribe);
    });

    // Instance B: user starts a slow run op (e.g. importMedia probing a large
    // file), then hits "back to import" → reset(null). When the probe resolves,
    // the store must stay reset — not snap back to the old project, and `past`
    // must not receive the stale snapshot.
    it("does not resurrect the old project over a reset(null) that lands mid-run", async () => {
      store().reset(SAMPLE_PROJECT);

      let release!: () => void;
      const gate = new Promise<void>((r) => (release = r));

      const inFlight = store().run(async (p) => {
        await gate;
        return { ...p, updated_at: 1 };
      });

      // "Back to import" while the op is in flight.
      store().reset(null);
      expect(store().project).toBeNull();

      release();
      await inFlight;

      // The reset must win: no zombie project, no stale snapshot in history.
      expect(store().project).toBeNull();
      expect(store().past).toEqual([]);
      expect(store().future).toEqual([]);
    });

    // Instance C: Cmd-Z pressed during an in-flight op visibly undoes; the
    // op's stale commit must neither revert the undo nor wipe the redo stack.
    it("does not revert an undo() that lands mid-run (and does not wipe redo)", async () => {
      const v0 = SAMPLE_PROJECT;
      store().reset(v0);
      // A previously committed edit so undo has something to pop.
      const v1: Project = { ...v0, updated_at: 1 };
      await store().run(async () => v1);
      expect(store().project).toBe(v1);

      let release!: () => void;
      const gate = new Promise<void>((r) => (release = r));

      const inFlight = store().run(async (p) => {
        await gate;
        return { ...p, updated_at: 2 };
      });

      // Cmd-Z during the in-flight op: store visibly returns to v0.
      store().undo();
      expect(store().project).toBe(v0);
      expect(store().future.length).toBe(1); // v1 is redoable

      release();
      await inFlight;

      // The undo must not be silently reverted by the stale commit, and the
      // redo stack must survive.
      expect(store().project).toBe(v0);
      expect(store().future.length).toBe(1);
    });
  });
});

// ── setProject vs the redo branch ────────────────────────────────────────────
// Regression (state-setproject-resurrects-redo). `setProject` used to touch
// NEITHER stack, so a replacement landing after an undo left the undone state
// sitting in `future`. One ⌘⇧Z then restored the branch the user had
// deliberately undone AND threw away whatever replaced it. Every dock panel
// committed this way, so the shape was reachable from nine different panels.

describe("setProject() and the redo branch", () => {
  it("clears `future` so a redo cannot resurrect the undone branch", () => {
    const v0 = SAMPLE_PROJECT;
    store().reset(v0);
    const v1: Project = { ...v0, name: "edited" };
    store().commit(v1);

    store().undo(); // back to v0; v1 sits in `future`
    expect(store().project).toBe(v0);
    expect(selectCanRedo(store())).toBe(true);

    // A transcription result (or any non-edit replacement) lands.
    const transcribed: Project = { ...v0, name: "transcribed" };
    store().setProject(transcribed);

    expect(selectCanRedo(store())).toBe(false);
    store().redo(); // no-op
    expect(store().project).toBe(transcribed);
  });
});

// ── commit(): the undoable panel edit ────────────────────────────────────────
// The AI/dock panels hand back a whole new Project they computed themselves.
// They used to land via `setProject`, which meant a translate run that rewrote
// every caption was unrecoverable. `commit` puts them on the same stacks as
// `run`.

describe("commit()", () => {
  it("is undoable: ⌘Z restores the pre-panel project", () => {
    const v0 = SAMPLE_PROJECT;
    store().reset(v0);

    const translated: Project = {
      ...v0,
      captions: v0.captions.map((c) => ({
        ...c,
        words: c.words.map((w) => ({ ...w, text: "translated" })),
      })),
    };
    store().commit(translated);

    expect(store().project).toBe(translated);
    expect(selectCanUndo(store())).toBe(true);
    store().undo();
    expect(store().project).toBe(v0);
  });

  it("accepts an updater and clears the redo stack", () => {
    store().reset(SAMPLE_PROJECT);
    store().commit((p) => ({ ...p, name: "a" }));
    store().undo();
    expect(selectCanRedo(store())).toBe(true);

    store().commit((p) => ({ ...p, name: "b" }));
    expect(store().project?.name).toBe("b");
    expect(selectCanRedo(store())).toBe(false);
  });

  it("is a no-op without an open project, and for an identical value", () => {
    store().commit({ ...SAMPLE_PROJECT }); // no project open
    expect(store().project).toBeNull();
    expect(store().past).toEqual([]);

    store().reset(SAMPLE_PROJECT);
    store().commit(SAMPLE_PROJECT); // same reference — nothing changed
    expect(store().past).toEqual([]);
  });

  it("caps history like run()", () => {
    store().reset(SAMPLE_PROJECT);
    for (let i = 0; i < 130; i++) {
      store().commit((p) => ({ ...p, updated_at: i }));
    }
    expect(store().past.length).toBe(100);
  });
});

// ── commit() coalescing ──────────────────────────────────────────────────────
// Panel text fields fire per keystroke and style sliders per pointer-move.
// Without coalescing, typing a 200-character description would push 200 undo
// entries and evict the 100-deep history the user's real edits live in — a
// regression introduced BY making the panels undoable.

describe("commit() coalescing", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("folds a same-key burst into ONE undo step", () => {
    const v0 = SAMPLE_PROJECT;
    store().reset(v0);

    for (const title of ["S", "Su", "Sun", "Sund", "Sunday"]) {
      store().commit((p) => ({ ...p, name: title }), {
        coalesceKey: "panel:projectmeta",
      });
      vi.advanceTimersByTime(50);
    }

    expect(store().project?.name).toBe("Sunday");
    expect(store().past.length).toBe(1);
    store().undo();
    expect(store().project).toBe(v0); // one ⌘Z undoes the whole word
  });

  it("starts a new step after the coalescing window lapses", () => {
    store().reset(SAMPLE_PROJECT);
    store().commit((p) => ({ ...p, name: "a" }), { coalesceKey: "k" });
    vi.advanceTimersByTime(COALESCE_WINDOW_MS + 1);
    store().commit((p) => ({ ...p, name: "ab" }), { coalesceKey: "k" });

    expect(store().past.length).toBe(2);
  });

  it("does not fold across different keys", () => {
    store().reset(SAMPLE_PROJECT);
    store().commit((p) => ({ ...p, name: "a" }), { coalesceKey: "panel:meta" });
    store().commit((p) => ({ ...p, name: "b" }), {
      coalesceKey: "panel:style",
    });
    expect(store().past.length).toBe(2);
  });

  it("does not fold across an unrelated store action", async () => {
    store().reset(SAMPLE_PROJECT);
    store().commit((p) => ({ ...p, name: "a" }), { coalesceKey: "k" });
    // A timeline drag (run) lands between two keystrokes in the same field.
    await store().run(async (p) => ({ ...p, updated_at: 7 }));
    store().commit((p) => ({ ...p, name: "ab" }), { coalesceKey: "k" });

    // 3 steps: the first keystroke burst, the drag, the second burst.
    expect(store().past.length).toBe(3);
    store().undo();
    expect(store().project?.name).toBe("a");
  });

  it("never coalesces an unkeyed commit (one discrete action = one step)", () => {
    store().reset(SAMPLE_PROJECT);
    store().commit((p) => ({ ...p, name: "a" }));
    store().commit((p) => ({ ...p, name: "b" }));
    expect(store().past.length).toBe(2);
  });
});

// ── dirty tracking ───────────────────────────────────────────────────────────
// Feeds the topbar indicator, the close guard, and the autosave scheduler.
// One derived predicate so those three can never disagree.

describe("dirty tracking", () => {
  it("a freshly opened project is clean; any edit dirties it", () => {
    store().reset(SAMPLE_PROJECT, "/tmp/talk.sundayedit");
    expect(selectDirty(store())).toBe(false);
    expect(selectAutosavable(store())).toBe(true);

    store().commit((p) => ({ ...p, name: "edited" }));
    expect(selectDirty(store())).toBe(true);
  });

  it("run() and setProject() both dirty the project", async () => {
    store().reset(SAMPLE_PROJECT, "/tmp/talk.sundayedit");
    await store().run(async (p) => ({ ...p, updated_at: 1 }));
    expect(selectDirty(store())).toBe(true);

    store().markSaved(store().project!, "/tmp/talk.sundayedit");
    expect(selectDirty(store())).toBe(false);

    store().setProject((p) => (p ? { ...p, updated_at: 2 } : p));
    expect(selectDirty(store())).toBe(true);
  });

  it("undoing back to the saved state reports CLEAN again", () => {
    store().reset(SAMPLE_PROJECT, "/tmp/talk.sundayedit");
    store().commit((p) => ({ ...p, name: "edited" }));
    expect(selectDirty(store())).toBe(true);

    store().undo();
    // Snapshots are immutable, so identity IS "identical to the saved file".
    expect(selectDirty(store())).toBe(false);
  });

  it("markSaved records the EXACT snapshot written, so a mid-save edit stays dirty", () => {
    store().reset(SAMPLE_PROJECT, "/tmp/talk.sundayedit");
    store().commit((p) => ({ ...p, name: "v1" }));
    const written = store().project!;

    // An edit lands while the write is in flight…
    store().commit((p) => ({ ...p, name: "v2" }));
    // …and the write completes, reporting what it actually put on disk.
    store().markSaved(written, "/tmp/talk.sundayedit");

    expect(store().project?.name).toBe("v2");
    expect(selectDirty(store())).toBe(true); // v2 is NOT on disk
  });

  it("a never-saved project is not autosavable (it must prompt for a path)", () => {
    store().reset(SAMPLE_PROJECT); // import/demo: no file
    expect(selectAutosavable(store())).toBe(false);
    store().commit((p) => ({ ...p, name: "edited" }));
    expect(selectDirty(store())).toBe(true);
    expect(selectAutosavable(store())).toBe(false);
  });

  it("reset() clears dirt and adopts the new file path", () => {
    store().reset(SAMPLE_PROJECT, "/tmp/a.sundayedit");
    store().commit((p) => ({ ...p, name: "edited" }));

    store().reset({ ...SAMPLE_PROJECT, id: "other" }, "/tmp/b.sundayedit");
    expect(selectDirty(store())).toBe(false);
    expect(store().filePath).toBe("/tmp/b.sundayedit");
    expect(store().past).toEqual([]);
  });
});
