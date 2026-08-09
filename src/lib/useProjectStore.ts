/**
 * Project store — the single source of truth for the open project and its
 * undo/redo history (unifies the old `useEditorHistory` snapshot store with
 * App's `useState<Project>`).
 *
 * Because every Rust caption/timeline operation is a pure function returning a
 * new Project (see ADR-002), undo/redo is trivial: keep a stack of previous
 * Project states. We never diff; we snapshot.
 *
 * One store now owns EVERY undoable edit — caption ops (CaptionEditor) AND
 * timeline drags (Timeline) share the same `past`/`future` stacks, so they no
 * longer diverge or clobber each other and every edit is undoable.
 *
 *   const run = useProjectStore((s) => s.run);
 *   await run((p) => ipc.ops.editWord(p, capId, i, "new"));
 *
 * `run` takes an async operation that maps the current Project to a new one
 * (via the Rust IPC). On success it commits: pushes the old state to `past`,
 * clears `future`. On failure it leaves state untouched and rethrows so the
 * caller can surface the error. Two concurrency rules keep the round-trip
 * honest:
 *
 *   - An op that arrives while another is in flight is not dropped — the
 *     NEWEST one is kept and runs as soon as the current op settles (rapid
 *     slider ticks coalesce; the pointer-release value always lands).
 *   - The commit is compare-and-swap: if the store changed while the op was
 *     in flight (reset/setProject/undo/redo landing mid-round-trip), the
 *     stale result is discarded instead of clobbering the newer state.
 *
 * `setProject` is the non-undoable escape hatch for whole-project replacements
 * that were never part of the undo stack (dock-panel edits, style changes,
 * transcription results). It mirrors React's `setState` signature.
 *
 * History is capped (default 100) so long sessions don't grow unbounded.
 */

import { useEffect } from "react";
import { create } from "zustand";
import type { Project } from "./bindings";

const HISTORY_CAP = 100;

export interface ProjectStore {
  project: Project | null;
  past: Project[];
  future: Project[];
  /** Whether an op is currently in flight (disable buttons). */
  busy: boolean;
  // Guard against overlapping ops corrupting the stacks. Kept in state (not a
  // ref) but never rendered — flipped synchronously before the first await.
  inFlight: boolean;

  /** Run an async op (current Project → new Project). Commits on success. */
  run: (op: (current: Project) => Promise<Project>) => Promise<void>;
  undo: () => void;
  redo: () => void;
  /** Replace the whole project (e.g. after import/open). Resets history. */
  reset: (project: Project | null) => void;
  /**
   * Non-undoable whole-project replacement (dock panels, style, transcribe).
   * Accepts a value or an updater, matching React's `setState`.
   */
  setProject: (
    next: Project | null | ((prev: Project | null) => Project | null),
  ) => void;
}

export const useProjectStore = create<ProjectStore>((set, get) => {
  // The newest op that arrived while another was in flight — ran (and cleared)
  // when the current op settles. Latest-wins: a burst of slider ticks collapses
  // to the final one, so the value the user released on is never lost.
  let pending: ((current: Project) => Promise<Project>) | null = null;

  return {
    project: null,
    past: [],
    future: [],
    busy: false,
    inFlight: false,

    run: async (op) => {
      const { inFlight, project } = get();
      if (inFlight) {
        pending = op;
        return;
      }
      if (!project) return;
      // Capture the pre-op state; the commit pushes this exact snapshot.
      set({ inFlight: true, busy: true });
      try {
        const next = await op(project);
        set((s) => {
          // Compare-and-swap: reset/setProject/undo/redo may have replaced the
          // project while the op was in flight — its result was derived from a
          // snapshot that no longer exists, so drop it rather than clobber.
          if (s.project !== project) return s;
          const appended = [...s.past, project];
          const past =
            appended.length > HISTORY_CAP
              ? appended.slice(appended.length - HISTORY_CAP)
              : appended;
          return { project: next, past, future: [] };
        });
      } finally {
        set({ inFlight: false, busy: false });
        const queued = pending;
        pending = null;
        if (queued) await get().run(queued);
      }
    },

    undo: () =>
      set((s) => {
        if (s.past.length === 0 || !s.project) return s;
        const previous = s.past[s.past.length - 1];
        return {
          project: previous,
          past: s.past.slice(0, -1),
          future: [s.project, ...s.future],
        };
      }),

    redo: () =>
      set((s) => {
        if (s.future.length === 0 || !s.project) return s;
        const next = s.future[0];
        return {
          project: next,
          past: [...s.past, s.project],
          future: s.future.slice(1),
        };
      }),

    reset: (project) => set({ project, past: [], future: [] }),

    setProject: (next) =>
      set((s) => ({
        project:
          typeof next === "function"
            ? (next as (prev: Project | null) => Project | null)(s.project)
            : next,
      })),
  };
});

/** Selectors — history availability, derived so buttons can subscribe cheaply. */
export const selectCanUndo = (s: ProjectStore): boolean => s.past.length > 0;
export const selectCanRedo = (s: ProjectStore): boolean => s.future.length > 0;

/**
 * ⌘Z / ⌘⇧Z (Ctrl on Windows). Mount once at the app root. Ignores keystrokes
 * while typing in an input/textarea/contenteditable so fields keep their own
 * native undo.
 */
export function useUndoHotkeys(): void {
  const undo = useProjectStore((s) => s.undo);
  const redo = useProjectStore((s) => s.redo);
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const mod = e.metaKey || e.ctrlKey;
      if (!mod || e.key.toLowerCase() !== "z") return;
      const target = e.target as HTMLElement | null;
      if (
        target &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.isContentEditable)
      ) {
        return; // let the field handle its own undo
      }
      e.preventDefault();
      if (e.shiftKey) redo();
      else undo();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [undo, redo]);
}
