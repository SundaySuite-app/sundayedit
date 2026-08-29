/**
 * Media bin — the source-media pool for the multi-track NLE.
 *
 * Lists every imported `MediaItem`, imports new files through the Tauri file
 * dialog (same picker pattern as `useVideoImport`), and lets the user drag a
 * media row onto a timeline lane to place it as a clip. Track creation lives
 * here too, so the bin is the one place you bring footage in and give it a lane.
 *
 * Mutations go through the shared `useProjectStore.run`, so importing media and
 * adding tracks land on the SAME undo stack as caption/timeline edits.
 */

import { useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  AlertTriangle,
  FileVideo,
  Music,
  Import,
  Plus,
  Search,
  X,
} from "lucide-react";

import type { MediaItem, Project, TrackKind } from "@/lib/bindings";
import { ipc, project as projectApi } from "@/lib/ipc";
import { useProjectStore } from "@/lib/useProjectStore";
import { useT } from "@/lib/i18n";
import { cn } from "@/lib/cn";
import { useThumbnail } from "./thumbnails";
import { useMediaAvailability } from "./useMediaAvailability";
import { useRelinkMedia, type RelinkStatus } from "./relink";

/** Phases the relink button should show as "busy" (disabled, spinner-ish label). */
const RELINK_BUSY_PHASES = new Set(["searching", "picking", "linking"]);

/** dataTransfer MIME carrying a media id from a bin row to a timeline lane. */
export const MEDIA_DND_MIME = "application/x-sundayedit-media";

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

function fmtSeconds(ms: number): string {
  const totalSec = Math.round(ms / 1000);
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

export function MediaBin({ project }: { project: Project }) {
  const t = useT();
  const run = useProjectStore((s) => s.run);
  const [error, setError] = useState<string | null>(null);
  // Which media rows are missing on disk, re-checked whenever the pool's
  // membership or paths change (fresh open, import, relink) — see the hook
  // doc for why NOT on every project edit.
  const { missingIds } = useMediaAvailability(project);
  const { relink, statusById } = useRelinkMedia();

  async function importMedia() {
    setError(null);
    try {
      const exts = await projectApi
        .acceptedExtensions()
        .catch(() => FALLBACK_EXTS);
      const selected = await openDialog({
        multiple: false,
        filters: [{ name: t("importFilterName"), extensions: exts }],
      });
      if (typeof selected !== "string") return; // cancelled
      await run((p) => ipc.timeline.importMedia(p, selected));
    } catch (e) {
      setError(t("mediaBinImportError", { error: (e as Error).message }));
    }
  }

  async function addTrack(kind: TrackKind, name: string) {
    setError(null);
    try {
      await run((p) => ipc.timeline.addTrack(p, kind, name));
    } catch (e) {
      setError((e as Error).message);
    }
  }

  // Remove a media item from the pool. The backend rejects it while any
  // timeline clip still references the media — surface that message.
  async function removeMedia(m: MediaItem) {
    setError(null);
    try {
      await run((p) => ipc.timeline.removeMedia(p, m.id));
    } catch (e) {
      setError((e as Error).message);
    }
  }

  return (
    <div className="flex h-full flex-col">
      <div className="flex shrink-0 items-center gap-2 border-b border-[var(--color-border)] px-4 py-3">
        <button
          type="button"
          onClick={() => void importMedia()}
          className="inline-flex items-center gap-1.5 rounded-md bg-[var(--color-accent-600)] px-3 py-1.5 text-[var(--text-ui-sm)] font-medium text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)]"
        >
          <Import size={15} /> {t("mediaBinImport")}
        </button>
      </div>

      {error && (
        <div
          role="alert"
          className="shrink-0 border-b border-[var(--color-border)] bg-[var(--color-danger,#b3261e)]/15 px-4 py-2 text-[var(--text-ui-xs)] text-[var(--color-danger,#b3261e)]"
        >
          {error}
        </div>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto p-3">
        {project.media.length === 0 ? (
          <p className="px-1 py-6 text-center text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
            {t("mediaBinEmpty")}
          </p>
        ) : (
          <ul className="space-y-1.5">
            {project.media.map((m) => (
              <MediaRow
                key={m.id}
                media={m}
                missing={missingIds.has(m.id)}
                status={statusById[m.id]}
                onRemove={() => void removeMedia(m)}
                onRelink={() => void relink(m)}
              />
            ))}
          </ul>
        )}
      </div>

      <div className="shrink-0 border-t border-[var(--color-border)] px-3 py-2.5">
        <div className="mb-1.5 flex items-center gap-1.5 text-[var(--text-ui-xs)] font-semibold text-[var(--color-fg-muted)]">
          <Plus size={12} /> {t("mediaBinAddTrackHeader")}
        </div>
        <div className="flex flex-wrap gap-1.5">
          <AddTrackButton
            label={t("mediaBinAddVideoTrack")}
            onClick={() => void addTrack("video", t("mediaBinAddVideoTrack"))}
          />
          <AddTrackButton
            label={t("mediaBinAddAudioTrack")}
            onClick={() => void addTrack("audio", t("mediaBinAddAudioTrack"))}
          />
          <AddTrackButton
            label={t("mediaBinAddOverlayTrack")}
            onClick={() =>
              void addTrack("overlay", t("mediaBinAddOverlayTrack"))
            }
          />
        </div>
        <p className="mt-2 text-[10px] text-[var(--color-fg-subtle)]">
          {t("mediaBinDragHint")}
        </p>
      </div>
    </div>
  );
}

function MediaRow({
  media,
  missing,
  status,
  onRemove,
  onRelink,
}: {
  media: MediaItem;
  missing: boolean;
  status: RelinkStatus | undefined;
  onRemove: () => void;
  onRelink: () => void;
}) {
  const t = useT();
  const Icon = media.kind === "audio_only" ? Music : FileVideo;
  // Source-frame thumbnail (memoized per media id; null for audio / browser
  // mode — the kind icon keeps standing in). Never attempted for a missing
  // file — there's nothing on disk for ffmpeg to read.
  const thumb = useThumbnail(missing ? undefined : media);
  const busy = !!status && RELINK_BUSY_PHASES.has(status.phase);
  const statusText = status && relinkStatusText(status, t);
  return (
    <li
      draggable
      onDragStart={(e) => {
        e.dataTransfer.setData(MEDIA_DND_MIME, media.id);
        e.dataTransfer.effectAllowed = "copy";
      }}
      className={cn(
        "flex cursor-grab items-center gap-2.5 rounded-md border px-2.5 py-2 active:cursor-grabbing",
        missing
          ? "border-[var(--color-danger,#b3261e)]/50 bg-[var(--color-danger,#b3261e)]/5"
          : "border-[var(--color-border)] bg-[var(--color-bg-surface)]",
      )}
      title={media.path}
    >
      {thumb ? (
        <img
          src={thumb}
          alt=""
          aria-hidden="true"
          draggable={false}
          className="h-9 w-14 shrink-0 rounded object-cover"
        />
      ) : missing ? (
        <AlertTriangle
          size={16}
          className="shrink-0 text-[var(--color-danger,#b3261e)]"
          aria-hidden="true"
        />
      ) : (
        <Icon
          size={16}
          className="shrink-0 text-[var(--color-fg-muted)]"
          aria-hidden="true"
        />
      )}
      <div className="min-w-0 flex-1">
        <div className="truncate text-[var(--text-ui-sm)]">
          {media.original_filename || media.path}
        </div>
        {missing ? (
          <div
            className="truncate text-[10px] text-[var(--color-danger,#b3261e)]"
            title={media.path}
          >
            {t("mediaBinMissing")} · {media.path}
          </div>
        ) : (
          <div className="text-[10px] tabular-nums text-[var(--color-fg-subtle)]">
            {fmtSeconds(media.duration_ms)}
            {media.width > 0 && ` · ${media.width}×${media.height}`}
          </div>
        )}
        {statusText && (
          <div
            data-testid="relink-status"
            className={cn(
              "truncate text-[10px]",
              status?.phase === "error"
                ? "text-[var(--color-danger,#b3261e)]"
                : "text-[var(--color-fg-subtle)]",
            )}
          >
            {statusText}
          </div>
        )}
      </div>
      {missing && (
        <button
          type="button"
          data-testid="relink-media"
          onClick={onRelink}
          disabled={busy}
          title={t("mediaBinRelink")}
          aria-label={t("mediaBinRelink")}
          className="inline-flex shrink-0 items-center gap-1 rounded-md border border-[var(--color-border)] px-2 py-1 text-[var(--text-ui-xs)] text-[var(--color-fg-muted)] hover:bg-[var(--color-bg)] hover:text-[var(--color-fg)] disabled:opacity-50"
        >
          <Search size={12} />
          {t("mediaBinRelink")}
        </button>
      )}
      <button
        type="button"
        data-testid="remove-media"
        onClick={onRemove}
        title={t("mediaBinRemove")}
        aria-label={t("mediaBinRemove")}
        className="grid h-6 w-6 shrink-0 place-items-center rounded text-[var(--color-fg-subtle)] hover:bg-[var(--color-bg)] hover:text-[var(--color-danger,#b3261e)]"
      >
        <X size={13} />
      </button>
    </li>
  );
}

/** Localized progress/outcome line under a missing row. Backend error
 *  messages are surfaced verbatim (same convention as the import/remove
 *  error strip above), never translated. */
function relinkStatusText(
  status: RelinkStatus,
  t: ReturnType<typeof useT>,
): string {
  switch (status.phase) {
    case "searching":
      return t("relinkSearching");
    case "picking":
      return t("relinkPicking");
    case "linking":
      return t("relinkLinking");
    case "error":
      return status.message ?? t("relinkFailedGeneric");
    case "done":
      return status.message ?? t("relinkSuccess");
  }
}

function AddTrackButton({
  label,
  onClick,
}: {
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "rounded-md border border-[var(--color-border)] px-2 py-1 text-[var(--text-ui-xs)] text-[var(--color-fg-muted)]",
        "hover:bg-[var(--color-bg-surface)] hover:text-[var(--color-fg)]",
      )}
    >
      {label}
    </button>
  );
}
