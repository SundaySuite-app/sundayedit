/**
 * The GPU compositor's runtime flag — off by default, persisted, and able to
 * turn itself off (E6; ADR-013).
 *
 * Two separate things decide whether the Pixi compositor runs:
 *
 *   1. **The user's choice** (`enabled`), persisted in `localStorage` next to
 *      the other preferences (see `i18n.ts`'s locale store — same pattern, same
 *      "localStorage may be unavailable" tolerance). Default **false**: the
 *      shipped experience is unchanged until someone opts in.
 *   2. **The runtime's answer** (`unavailableReason`), written by the automatic
 *      off-switch. If the capability probe fails, or Pixi throws while
 *      starting, the compositor reports it here and the preview falls back to
 *      the `<video>` path for the rest of the session.
 *
 * They are kept apart on purpose: an automatic fallback must not silently
 * rewrite the user's setting, or the toggle would appear to flip itself on a
 * machine that simply cannot run it — and the user would never see WHY. The
 * setting stays where they put it; the reason is shown next to it.
 *
 * `active` (the derived selector below) is the only thing the preview should
 * branch on: enabled AND nothing has disqualified it.
 */

import { create } from "zustand";

import type { CompositorCapability } from "./capability";

const STORAGE_KEY = "sundayedit.gpuCompositor";

/** The persisted user choice. Absent / unreadable storage means OFF. */
export function initialEnabled(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) === "1";
  } catch {
    /* localStorage may be unavailable */
    return false;
  }
}

export interface CompositorFlagState {
  /** The user's setting. */
  enabled: boolean;
  /**
   * Why the compositor is unavailable this session despite the setting, or
   * null when nothing has disqualified it. Set by the automatic off-switch.
   */
  unavailableReason: CompositorCapability["reason"] | null;
  setEnabled: (enabled: boolean) => void;
  /** Automatic off-switch: the probe failed, or the renderer threw. */
  reportUnavailable: (reason: CompositorCapability["reason"]) => void;
  /** Clear a previous session-level disqualification (used by tests + retry). */
  clearUnavailable: () => void;
}

export const useCompositorFlag = create<CompositorFlagState>((set) => ({
  enabled: initialEnabled(),
  unavailableReason: null,
  setEnabled: (enabled) => {
    try {
      localStorage.setItem(STORAGE_KEY, enabled ? "1" : "0");
    } catch {
      /* ignore — the setting simply won't survive a restart */
    }
    // Turning it back ON is an explicit "try again": drop the stale reason so
    // the probe gets a fresh say instead of the UI showing last hour's failure.
    set(enabled ? { enabled, unavailableReason: null } : { enabled });
  },
  reportUnavailable: (reason) => set({ unavailableReason: reason }),
  clearUnavailable: () => set({ unavailableReason: null }),
}));

/**
 * Should the Pixi compositor actually be mounted? The ONLY predicate the
 * preview may branch on.
 */
export const selectCompositorActive = (s: CompositorFlagState): boolean =>
  s.enabled && s.unavailableReason === null;
