/**
 * Media thumbnails — one cached JPEG per media item, shared by the media bin
 * rows and the timeline's clip boxes.
 *
 * The backend (`extract_thumbnail`) grabs a single 120px-tall frame with the
 * bundled ffmpeg and writes it under the app cache dir; this module makes sure
 * that happens AT MOST once per media id per session (an in-memory memo keyed
 * on the id holds the in-flight/settled promise, so concurrent consumers await
 * the same grab). Everything degrades to `null` — audio-only media, browser/e2e
 * mode (`isTauri()` false), or a failed ffmpeg run — and the callers keep
 * today's text/icon-only look.
 *
 * This module also fires the ONE-TIME startup sweep of the filmstrip +
 * thumbnail disk caches (`prune_media_cache`, `media_cache.rs`) — neither
 * cache is ever pruned by the extraction commands themselves, so a user
 * scrubbing a long 4K timeline across every zoom tier would otherwise grow
 * them without bound across sessions. `thumbnailSrc` is the first thing any
 * media view calls, so kicking it off here (rather than a dedicated app-boot
 * effect) covers every entry point for free. Fire-and-forget: a failed sweep
 * just means the caches grow a little more before the next launch retries it.
 */

import { useEffect, useState } from "react";
import { appCacheDir, join } from "@tauri-apps/api/path";
import { convertFileSrc, isTauri } from "@tauri-apps/api/core";

import type { MediaItem } from "@/lib/bindings";
import { ipc } from "@/lib/ipc";

// In-flight/settled promise per media id — the memo also caches failures (as
// null) so broken media doesn't re-spawn ffmpeg on every render.
const thumbnails = new Map<string, Promise<string | null>>();

// At most one prune per session — subsequent calls are no-ops.
let cachePruneKicked = false;

/** Fire the startup disk-cache sweep once per session. No-op off-Tauri. */
export function kickMediaCachePrune(): void {
  if (cachePruneKicked || !isTauri()) return;
  cachePruneKicked = true;
  void (async () => {
    try {
      await ipc.media.pruneCache(await appCacheDir());
    } catch {
      // Best-effort housekeeping — a failed sweep isn't worth surfacing.
    }
  })();
}

/** Test-only: allow a fresh kick in the next test. */
export function __resetCachePruneForTests() {
  cachePruneKicked = false;
}

/** Frame time for the grab: 10% in (capped at 5 s) dodges black lead-ins. */
function thumbTimeMs(media: MediaItem): number {
  return Math.min(Math.round(media.duration_ms * 0.1), 5000);
}

/**
 * Resolve an asset URL for `media`'s thumbnail, extracting it on first use.
 * Resolves `null` when a thumbnail isn't possible (audio, browser, no ffmpeg).
 */
export function thumbnailSrc(media: MediaItem): Promise<string | null> {
  if (!isTauri() || media.kind === "audio_only") return Promise.resolve(null);
  kickMediaCachePrune();
  const cached = thumbnails.get(media.id);
  if (cached) return cached;
  const grab = (async () => {
    const out = await join(
      await appCacheDir(),
      "thumbnails",
      `${media.id}.jpg`,
    );
    const written = await ipc.media.thumbnail(
      media.path,
      thumbTimeMs(media),
      out,
    );
    return convertFileSrc(written);
  })().catch(() => null);
  thumbnails.set(media.id, grab);
  return grab;
}

/**
 * React hook: the media's thumbnail asset URL, or `null` while loading /
 * when none is possible. Pass `undefined` to opt out (e.g. text clips).
 */
export function useThumbnail(media: MediaItem | undefined): string | null {
  const [src, setSrc] = useState<string | null>(null);
  const id = media?.id;
  useEffect(() => {
    setSrc(null);
    if (!media) return;
    let cancelled = false;
    void thumbnailSrc(media).then((url) => {
      if (!cancelled && url) setSrc(url);
    });
    return () => {
      cancelled = true;
    };
    // Re-run on identity (id), not object identity — project updates recreate
    // the arrays but a media item's thumbnail never changes within a session.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);
  return src;
}
