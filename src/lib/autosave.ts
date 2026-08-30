/**
 * Autosave + close guard — the "don't lose an hour of caption corrections"
 * layer (R1 trust round).
 *
 * The file layer under `project_save` has been crash-safe from the start (WAL,
 * one transaction), but nothing ever called it on its own: every change since
 * the last manual ⌘S lived only in renderer memory. For SundayEdit's actual
 * session shape — transcribe a 60-minute sermon, then hand-correct 8% of the
 * words — a crash or an accidental ⌘Q threw the whole correction pass away.
 *
 * Three pieces, all no-ops outside Tauri so browser dev and the Playwright
 * suite degrade to the previous behaviour:
 *
 *   - {@link useAutosave} — debounced write-behind. Runs only when the project
 *     already HAS a file (a never-saved project must still prompt for a path,
 *     so `saveProjectAs` stays the only thing that can create a file), and
 *     only when no op is in flight, so it can never write a half-applied edit.
 *   - {@link useCloseGuard} — intercepts the window's close request while
 *     there are unsaved changes and asks before discarding them.
 *   - {@link selectDirty} (in the store) — the indicator's single source of
 *     truth, so the badge and the guard can never disagree.
 *
 * Why debounce rather than save on every commit: a commit lands on every
 * pointer-release of a timeline drag and on every keystroke burst in a panel.
 * The debounce keeps the disk write off the interaction path entirely — the
 * timer restarts on each change, so a write only happens once the user has
 * been still for {@link AUTOSAVE_DEBOUNCE_MS}.
 */

import { useEffect, useRef } from "react";
import { isTauri } from "@tauri-apps/api/core";

import { ipc } from "./ipc";
import type { Project } from "./bindings";
import { selectDirty, useProjectStore } from "./useProjectStore";

/** Quiet time before a write-behind save fires. */
export const AUTOSAVE_DEBOUNCE_MS = 2000;

/**
 * Write `project` to `path` and record it as the saved snapshot.
 *
 * `markSaved` stores the EXACT project that reached disk, not "whatever is in
 * the store now" — so an edit that lands while the write is in flight leaves
 * the document correctly dirty instead of being marked saved and then lost.
 *
 * Exported for tests and for the manual save path, which wants the same
 * bookkeeping.
 */
export async function saveProjectTo(
  project: Project,
  path: string,
): Promise<void> {
  const { setSaving, markSaved } = useProjectStore.getState();
  setSaving(true);
  try {
    await ipc.project.save(project, path);
    markSaved(project, path);
  } catch (e) {
    setSaving(false);
    throw e;
  }
}

/**
 * Debounced autosave for a project that already lives in a file.
 *
 * Mount once at the app root. `onError` surfaces a failed write — a silent
 * autosave failure is worse than no autosave, because the indicator would
 * still be telling the user their work is safe.
 */
export function useAutosave(onError?: (message: string) => void): void {
  const project = useProjectStore((s) => s.project);
  const filePath = useProjectStore((s) => s.filePath);
  const dirty = useProjectStore(selectDirty);
  // An op mid-round-trip has NOT committed yet: writing now would persist the
  // pre-op state and then immediately be stale. Waiting also means the write
  // never overlaps the op's own commit.
  const inFlight = useProjectStore((s) => s.inFlight);

  const errorRef = useRef(onError);
  errorRef.current = onError;

  useEffect(() => {
    if (!isTauri()) return; // browser dev / E2E: no file layer to write to
    if (!project || !filePath || !dirty || inFlight) return;

    const timer = window.setTimeout(() => {
      void saveProjectTo(project, filePath).catch((e: unknown) => {
        console.error("autosave failed", e);
        errorRef.current?.((e as Error).message);
      });
    }, AUTOSAVE_DEBOUNCE_MS);

    // Any further change (or an op starting) reschedules: the write only ever
    // happens after the user has stopped for a full window.
    return () => window.clearTimeout(timer);
  }, [project, filePath, dirty, inFlight]);
}

/**
 * Ask before the window closes on unsaved changes.
 *
 * `confirmDiscard` resolves true to close anyway. It is read through a ref so
 * a new callback identity (a re-render, a locale change) never re-registers
 * the native listener — re-registering it is how you end up with two handlers
 * and two dialogs.
 */
export function useCloseGuard(confirmDiscard: () => Promise<boolean>): void {
  const confirmRef = useRef(confirmDiscard);
  confirmRef.current = confirmDiscard;

  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    void (async () => {
      try {
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const win = getCurrentWindow();
        const un = await win.onCloseRequested(async (event) => {
          if (!selectDirty(useProjectStore.getState())) return; // clean: let it close
          event.preventDefault();
          if (await confirmRef.current()) await win.destroy();
        });
        // Unmounted while the dynamic import was in flight.
        if (cancelled) un();
        else unlisten = un;
      } catch {
        // No Tauri window API (browser dev) — nothing to guard.
      }
    })();

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
