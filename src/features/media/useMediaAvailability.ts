/**
 * Missing-media detection — Round: relink media.
 *
 * Runs `check_media_paths` whenever the project's media POOL changes (a
 * member added/removed, or a path swapped by a relink) — not on every
 * project update. Every Rust op returns a brand-new `Project` object (even
 * when unrelated fields changed), so keying off `project` itself would refire
 * the check on every caption edit; keying off a fingerprint of the pool's
 * ids+paths instead covers exactly the two triggers that matter — a fresh
 * project open (the pool is populated from scratch, so the fingerprint goes
 * from "" to real) and any subsequent import/remove/relink (the fingerprint
 * changes) — while an edit to captions, style, tracks, etc. is a no-op here.
 *
 * The check itself is a `Path::exists` stat per media item (see
 * `services/video::media_availability`), cheap enough to run eagerly.
 * Off-Tauri (plain browser dev, no E2E mock) `invoke` rejects; this hook
 * swallows that and reports nothing missing rather than surfacing an error
 * for a check nobody asked to see.
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { isTauri } from "@tauri-apps/api/core";

import type { MediaAvailability, Project } from "@/lib/bindings";
import { project as projectApi } from "@/lib/ipc";

export interface MediaAvailabilityState {
  /** Raw per-media rows from the last successful check. */
  availability: MediaAvailability[];
  /** Media ids currently believed missing. Stable reference between checks
   *  that don't change the result, so it's cheap to hand to a memoized
   *  render subtree (e.g. the timeline's clip lanes) as-is. */
  missingIds: Set<string>;
  /** Re-run the check now (e.g. a manual retry after the user moved the file
   *  back). Also what the effect below calls on mount/pool-change. */
  refresh: () => Promise<void>;
}

const EMPTY_AVAILABILITY: MediaAvailability[] = [];
const EMPTY_IDS: Set<string> = new Set();

export function useMediaAvailability(
  project: Project | null | undefined,
): MediaAvailabilityState {
  const [availability, setAvailability] =
    useState<MediaAvailability[]>(EMPTY_AVAILABILITY);

  const refresh = useCallback(async () => {
    if (!project || project.media.length === 0 || !isTauri()) {
      setAvailability(EMPTY_AVAILABILITY);
      return;
    }
    try {
      const result = await projectApi.checkMediaPaths(project);
      setAvailability(Array.isArray(result) ? result : EMPTY_AVAILABILITY);
    } catch {
      // No Tauri backend under this render (browser dev without the E2E
      // mock) — nothing to report.
      setAvailability(EMPTY_AVAILABILITY);
    }
  }, [project]);

  // The pool's identity+path fingerprint — see the module doc for why this,
  // and not `project`, is the effect dependency.
  const poolKey = project
    ? project.media.map((m) => `${m.id}:${m.path}`).join("|")
    : "";

  useEffect(() => {
    void refresh();
    // `refresh` itself changes identity whenever `project` does (every op
    // round-trip), which would defeat `poolKey`'s whole point — so the
    // effect keys ONLY on the fingerprint, and always calls the latest
    // `refresh` closure (safe: it reads the current `project` via closure,
    // not a stale one, because `poolKey` changing is what re-triggers this).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [poolKey]);

  const missingIds = useMemo(() => {
    const missing = availability.filter((a) => !a.exists);
    return missing.length === 0
      ? EMPTY_IDS
      : new Set(missing.map((a) => a.media_id));
  }, [availability]);

  return { availability, missingIds, refresh };
}
