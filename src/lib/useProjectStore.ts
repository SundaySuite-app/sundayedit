/**
 * Project store — the single source of truth for the open project, its
 * undo/redo history, and whether it has unsaved changes.
 *
 * Because every Rust caption/timeline operation is a pure function returning a
 * new Project (see ADR-002), undo/redo is trivial: keep a stack of previous
 * Project states. We never diff; we snapshot.
 *
 * One store owns EVERY undoable edit — caption ops (CaptionEditor), timeline
 * drags (Timeline) AND the dock/modal panels (translate, polish, cleanup,
 * reflow, speakers, context, clips, style) share the same `past`/`future`
 * stacks, so they no longer diverge or clobber each other.
 *
 * ── The three ways to change the project ───────────────────────────────────
 *
 *   `run(op)`      — an async EDIT that has to round-trip through Rust.
 *                      await run((p) => ipc.ops.editWord(p, capId, i, "new"));
 *                    Undoable. Commits on success, leaves state untouched and
 *                    rethrows on failure.
 *
 *   `commit(next)` — a synchronous EDIT whose new Project the caller already
 *                    holds (a panel that ran its own IPC call, or a plain
 *                    field patch). Undoable, same stacks as `run`.
 *
 *   `setProject`   — a NON-undoable whole-project replacement that is not an
 *                    edit of the open document: transcription results dropping
 *                    a fresh caption track in, export-config plumbing. It
 *                    clears `future`, because leaving a redo branch alive
 *                    across a replacement lets ⌘⇧Z resurrect the state the
 *                    user deliberately undid AND throw away what replaced it.
 *
 *   `reset`        — open/import/demo/back-to-import. Clears BOTH stacks and
 *                    marks the project saved (nothing has been edited yet).
 *
 * Two concurrency rules keep `run`'s round-trip honest:
 *
 *   - An op that arrives while another is in flight is not dropped — the
 *     NEWEST one is kept and runs as soon as the current op settles (rapid
 *     slider ticks coalesce; the pointer-release value always lands).
 *   - The commit is compare-and-swap: if the store changed while the op was
 *     in flight (reset/setProject/commit/undo/redo landing mid-round-trip),
 *     the stale result is discarded instead of clobbering the newer state.
 *
 * ── Coalescing ─────────────────────────────────────────────────────────────
 * Panel text fields fire onChange per KEYSTROKE and sliders per pointer-move.
 * One undo entry each would flush the 100-deep history in a single typed
 * sentence and evict the user's real edits, so `commit` takes an optional
 * `coalesceKey`: consecutive commits carrying the same key within
 * {@link COALESCE_WINDOW_MS} fold into the one undo step. Any other store
 * mutation (a `run`, an undo, a different panel) breaks the chain.
 *
 * ── Dirty tracking ─────────────────────────────────────────────────────────
 * `savedSnapshot` is the exact Project last written to (or read from) disk.
 * Because every state is an immutable snapshot, "has unsaved changes" is the
 * reference comparison {@link selectDirty} — which means undoing back to the
 * saved state correctly reports CLEAN. `filePath` is the file that snapshot
 * lives in, or null for a project that has never been saved (autosave stays
 * off until the user picks a path; see `useAutosave`).
 *
 * History is capped (default 100) so long sessions don't grow unbounded.
 */

import { useEffect } from "react";
import { create } from "zustand";
import type { Project } from "./bindings";

const HISTORY_CAP = 100;

/**
 * How long a burst of same-key commits keeps folding into one undo entry.
 * Long enough that a typed phrase is one ⌘Z, short enough that coming back to
 * the same field after a pause starts a fresh step.
 */
export const COALESCE_WINDOW_MS = 800;

export interface CommitOptions {
  /**
   * Fold this commit into the previous one when that commit carried the SAME
   * key and landed less than {@link COALESCE_WINDOW_MS} ago. For per-keystroke
   * / per-pointer-move sources (panel text fields, style sliders) so one typed
   * phrase is one undo step rather than 40.
   */
  coalesceKey?: string;
}

export interface ProjectStore {
  project: Project | null;
  past: Project[];
  future: Project[];
  /** Whether an op is currently in flight (disable buttons). */
  busy: boolean;
  // Guard against overlapping ops corrupting the stacks. Kept in state (not a
  // ref) but never rendered — flipped synchronously before the first await.
  inFlight: boolean;
  /**
   * The exact Project last written to (or read from) disk. Reference-compared
   * against `project` by {@link selectDirty} — snapshots are immutable, so
   * identity IS "unchanged since the last save".
   */
  savedSnapshot: Project | null;
  /** Where `savedSnapshot` lives, or null for a project never saved to a file. */
  filePath: string | null;
  /** True while an autosave/manual save round-trip is running (indicator only). */
  saving: boolean;

  /** Run an async op (current Project → new Project). Commits on success. */
  run: (op: (current: Project) => Promise<Project>) => Promise<void>;
  /**
   * Commit an EDIT whose result the caller already holds — undoable, same
   * stacks as `run`. Accepts a value or an updater. A no-op when there is no
   * open project, or when the value is identical to the current one (so an
   * onChange that re-emits the same object never manufactures an undo step).
   */
  commit: (
    next: Project | ((prev: Project) => Project),
    options?: CommitOptions,
  ) => void;
  undo: () => void;
  redo: () => void;
  /**
   * Replace the whole project (open/import/demo/back-to-import). Clears both
   * history stacks and marks the result saved — nothing has been edited yet.
   * `filePath` is the file it came from, or null for import/demo.
   */
  reset: (project: Project | null, filePath?: string | null) => void;
  /**
   * Non-undoable whole-project replacement for changes that are not edits of
   * the open document (transcription results, export-config plumbing).
   * Accepts a value or an updater, matching React's `setState`.
   *
   * It CLEARS `future`: a redo stack that outlives a replacement lets ⌘⇧Z
   * restore the branch the user deliberately undid and discard whatever
   * replaced it.
   */
  setProject: (
    next: Project | null | ((prev: Project | null) => Project | null),
  ) => void;
  /** Record that `saved` is now on disk at `path` (manual save or autosave). */
  markSaved: (saved: Project, path: string) => void;
  setSaving: (saving: boolean) => void;
}

export const useProjectStore = create<ProjectStore>((set, get) => {
  // The newest op that arrived while another was in flight — ran (and cleared)
  // when the current op settles. Latest-wins: a burst of slider ticks collapses
  // to the final one, so the value the user released on is never lost.
  let pending: ((current: Project) => Promise<Project>) | null = null;

  // Coalescing chain state for `commit`. Module-scope rather than store state
  // because it is bookkeeping, never rendered, and must not trigger updates.
  let lastCoalesceKey: string | null = null;
  let lastCoalesceAt = 0;
  /** Any non-coalescing mutation ends the current run of same-key commits. */
  const breakCoalescing = () => {
    lastCoalesceKey = null;
  };

  /**
   * Push `prev` onto `past` (capped) and clear `future`. The one place the
   * undo stacks grow, shared by `run`'s commit and `commit`.
   */
  const pushHistory = (past: Project[], prev: Project): Project[] => {
    const appended = [...past, prev];
    return appended.length > HISTORY_CAP
      ? appended.slice(appended.length - HISTORY_CAP)
      : appended;
  };

  return {
    project: null,
    past: [],
    future: [],
    busy: false,
    inFlight: false,
    savedSnapshot: null,
    filePath: null,
    saving: false,

    run: async (op) => {
      const { inFlight, project } = get();
      if (inFlight) {
        pending = op;
        return;
      }
      if (!project) return;
      breakCoalescing();
      // Capture the pre-op state; the commit pushes this exact snapshot.
      set({ inFlight: true, busy: true });
      try {
        const next = await op(project);
        set((s) => {
          // Compare-and-swap: reset/setProject/undo/redo may have replaced the
          // project while the op was in flight — its result was derived from a
          // snapshot that no longer exists, so drop it rather than clobber.
          if (s.project !== project) return s;
          return {
            project: next,
            past: pushHistory(s.past, project),
            future: [],
          };
        });
      } finally {
        set({ inFlight: false, busy: false });
        const queued = pending;
        pending = null;
        if (queued) await get().run(queued);
      }
    },

    commit: (next, options) => {
      const key = options?.coalesceKey ?? null;
      const now = Date.now();
      // Decide BEFORE mutating: a same-key commit inside the window replaces
      // the top of the chain instead of pushing a new step, so `past` keeps
      // the state from before the whole burst.
      const fold =
        key !== null &&
        key === lastCoalesceKey &&
        now - lastCoalesceAt < COALESCE_WINDOW_MS;
      set((s) => {
        const prev = s.project;
        if (!prev) return s; // `commit` edits an OPEN document; `reset` opens one
        const value = typeof next === "function" ? next(prev) : next;
        if (value === prev) return s; // nothing changed — no undo step
        lastCoalesceKey = key;
        lastCoalesceAt = now;
        return fold
          ? { project: value, future: [] }
          : { project: value, past: pushHistory(s.past, prev), future: [] };
      });
    },

    undo: () => {
      breakCoalescing();
      set((s) => {
        if (s.past.length === 0 || !s.project) return s;
        const previous = s.past[s.past.length - 1];
        return {
          project: previous,
          past: s.past.slice(0, -1),
          future: [s.project, ...s.future],
        };
      });
    },

    redo: () => {
      breakCoalescing();
      set((s) => {
        if (s.future.length === 0 || !s.project) return s;
        const next = s.future[0];
        return {
          project: next,
          past: [...s.past, s.project],
          future: s.future.slice(1),
        };
      });
    },

    reset: (project, filePath = null) => {
      breakCoalescing();
      set({
        project,
        past: [],
        future: [],
        savedSnapshot: project,
        filePath,
      });
    },

    setProject: (next) => {
      breakCoalescing();
      set((s) => ({
        project:
          typeof next === "function"
            ? (next as (prev: Project | null) => Project | null)(s.project)
            : next,
        // A replacement invalidates the redo branch. Without this, undo →
        // panel edit → redo restored the undone state AND discarded the
        // panel's result (regression: state-setproject-resurrects-redo).
        future: [],
      }));
    },

    markSaved: (saved, path) =>
      set({ savedSnapshot: saved, filePath: path, saving: false }),

    setSaving: (saving) => set({ saving }),
  };
});

/** Selectors — history availability, derived so buttons can subscribe cheaply. */
export const selectCanUndo = (s: ProjectStore): boolean => s.past.length > 0;
export const selectCanRedo = (s: ProjectStore): boolean => s.future.length > 0;

/**
 * Does the open project differ from what is on disk?
 *
 * Reference comparison, not a deep diff: every state in this store is an
 * immutable snapshot, so `project === savedSnapshot` means "byte-identical to
 * the saved file" — and undoing back to the saved state correctly reads CLEAN
 * instead of nagging forever.
 */
export const selectDirty = (s: ProjectStore): boolean =>
  s.project !== null && s.project !== s.savedSnapshot;

/** Can this project be autosaved, i.e. does it already live in a file? */
export const selectAutosavable = (s: ProjectStore): boolean =>
  s.project !== null && s.filePath !== null;

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
