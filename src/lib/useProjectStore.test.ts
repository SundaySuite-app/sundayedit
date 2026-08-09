import { beforeEach, describe, expect, it } from "vitest";

import {
  useProjectStore,
  selectCanUndo,
  selectCanRedo,
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
