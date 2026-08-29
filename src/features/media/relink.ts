/**
 * The relink flow — Round: relink media.
 *
 * Always in this order (see `commands/project.rs::project_relink` +
 * `timeline_ops::relink_media` on the Rust side):
 *
 *   a. Auto-search by content hash across a handful of likely directories —
 *      the moved file's own former folder, then Movies/Desktop/Downloads/
 *      Home. Most moves keep the filename, which the backend checks first.
 *   b. Nothing found → fall back to the same native file-dialog pattern
 *      `MediaBin`/`useVideoImport` already use.
 *   c. Commit through the caller's `run` (lands on the undo stack, persists)
 *      and compare the re-probed duration against what the pool already had:
 *      a different duration on the SAME content-hash target means the user
 *      picked a different file, not the moved one — surfaced as
 *      `durationChanged` rather than silently pretended away.
 *
 * Kept out of any single component (`useRelinkMedia` below wraps it with
 * per-media status) so `MediaBin`'s rows and the app-wide banner drive the
 * exact same flow instead of two hand-rolled copies.
 */
import { useCallback, useState } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  desktopDir,
  dirname,
  downloadDir,
  homeDir,
  videoDir,
} from "@tauri-apps/api/path";

import type { MediaItem, Project } from "@/lib/bindings";
import { ipc, project as projectApi } from "@/lib/ipc";
import { useProjectStore } from "@/lib/useProjectStore";
import { useT } from "@/lib/i18n";

const FALLBACK_EXTS = [
  "mp4",
  "mov",
  "mkv",
  "webm",
  "avi",
  "m4v",
  "mp3",
  "wav",
  "m4a",
  "flac",
  "ogg",
];

/** A path helper that isn't available (or a plain browser with no Tauri
 *  runtime under it) is just one fewer directory to search — never fatal. */
async function safeDir(fn: () => Promise<string>): Promise<string | null> {
  try {
    const d = await fn();
    return typeof d === "string" && d.length > 0 ? d : null;
  } catch {
    return null;
  }
}

/** The old file's parent directory, then the usual media homes — deduped,
 *  with any directory that couldn't be resolved dropped rather than passed
 *  through as `undefined`. */
export async function candidateSearchDirs(oldPath: string): Promise<string[]> {
  const [parent, movies, desktop, downloads, home] = await Promise.all([
    safeDir(() => dirname(oldPath)),
    safeDir(videoDir),
    safeDir(desktopDir),
    safeDir(downloadDir),
    safeDir(homeDir),
  ]);
  const all = [parent, movies, desktop, downloads, home].filter(
    (d): d is string => !!d,
  );
  return Array.from(new Set(all));
}

export type RelinkPhase = "searching" | "picking" | "linking";

export type RelinkOutcome =
  | { kind: "auto"; durationChanged: boolean }
  | { kind: "manual"; durationChanged: boolean }
  | { kind: "cancelled" }
  | { kind: "error"; message: string };

/**
 * Run the full flow for one pooled `MediaItem`. `run` is the caller's
 * `useProjectStore` `run` — the commit step lands on the shared undo stack
 * exactly like every other timeline op.
 */
export async function relinkMedia(params: {
  media: MediaItem;
  run: (op: (p: Project) => Promise<Project>) => Promise<void>;
  filterName: string;
  onPhase?: (phase: RelinkPhase) => void;
}): Promise<RelinkOutcome> {
  const { media, run, filterName, onPhase } = params;
  try {
    onPhase?.("searching");
    const dirs = await candidateSearchDirs(media.path);
    const found = await projectApi.relink(
      media.content_hash,
      dirs,
      media.original_filename || undefined,
    );

    let newPath = found ?? null;
    const auto = newPath !== null;
    if (!newPath) {
      onPhase?.("picking");
      const exts = await projectApi
        .acceptedExtensions()
        .catch(() => FALLBACK_EXTS);
      const picked = isTauri()
        ? await openDialog({
            multiple: false,
            filters: [{ name: filterName, extensions: exts }],
          })
        : null;
      if (typeof picked !== "string") {
        return { kind: "cancelled" };
      }
      newPath = picked;
    }

    onPhase?.("linking");
    let updated: Project | null = null;
    await run(async (p) => {
      const next = await ipc.timeline.relinkMedia(p, media.id, newPath!);
      updated = next;
      return next;
    });
    const newDuration = updated
      ? (updated as Project).media.find((m) => m.id === media.id)?.duration_ms
      : undefined;
    const durationChanged =
      newDuration !== undefined && newDuration !== media.duration_ms;
    return auto
      ? { kind: "auto", durationChanged }
      : { kind: "manual", durationChanged };
  } catch (e) {
    return {
      kind: "error",
      message: e instanceof Error ? e.message : String(e),
    };
  }
}

// ── shared per-media status, so every surface (bin row, banner) agrees ──────

export interface RelinkStatus {
  phase: RelinkPhase | "done" | "error";
  /** Localized outcome text (done) or the raw backend message (error). */
  message?: string;
}

export interface UseRelinkMedia {
  /** Kick off the flow for one media item; status updates land in `statusById`. */
  relink: (media: MediaItem) => Promise<void>;
  statusById: Record<string, RelinkStatus>;
}

/** Wraps `relinkMedia` with per-media-id progress/outcome, shared by every
 *  caller that mounts this hook (each mount gets its own map — callers that
 *  need to share status across surfaces should share the hook instance). */
export function useRelinkMedia(): UseRelinkMedia {
  const run = useProjectStore((s) => s.run);
  const t = useT();
  const [statusById, setStatusById] = useState<Record<string, RelinkStatus>>(
    {},
  );

  const relink = useCallback(
    async (media: MediaItem) => {
      setStatusById((s) => ({ ...s, [media.id]: { phase: "searching" } }));
      const outcome = await relinkMedia({
        media,
        run,
        filterName: t("importFilterName"),
        onPhase: (phase) =>
          setStatusById((s) => ({ ...s, [media.id]: { phase } })),
      });
      setStatusById((s) => {
        const next = { ...s };
        if (outcome.kind === "cancelled") {
          delete next[media.id];
        } else if (outcome.kind === "error") {
          next[media.id] = { phase: "error", message: outcome.message };
        } else {
          next[media.id] = {
            phase: "done",
            message: outcome.durationChanged
              ? t("relinkDurationWarning")
              : outcome.kind === "auto"
                ? t("relinkFoundAutomatically")
                : t("relinkSuccess"),
          };
        }
        return next;
      });
    },
    [run, t],
  );

  return { relink, statusById };
}
