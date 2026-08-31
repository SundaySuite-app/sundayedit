/**
 * Filmstrip tiles — the multi-frame backdrop on a video clip box (E3-UI).
 *
 * Replaces the single dimmed thumbnail (`useThumbnail`, still the fallback)
 * with tiles rendered by the backend's `extract_filmstrip_tile`: one JPEG per
 * tile holding several source frames side by side. Tiles are addressed on the
 * SAME fixed, absolute per-zoom-tier grid the Rust side uses
 * (`src-tauri/src/services/tiles.rs`) — this module is the TS mirror of that
 * grid math, kept in exact lockstep (see the constants below) so a tile key
 * computed here always matches one the backend would compute for the same
 * `(tier, index)`. ts-rs only exported the `TileRef` *shape*; the grid
 * functions themselves are plain arithmetic, so they're reproduced rather
 * than round-tripped through IPC on every scroll frame.
 *
 * Three layers, in increasing impurity:
 *   1. Grid math — tier/span/index/key, `tilesForRange`. Pure, unit-tested.
 *   2. `pickZoomTier` + `sourceRangeForVisible` + `tileScreenRect` — pure
 *      mapping from the timeline's screen geometry to the source-media tile
 *      grid a clip should request. Unit-tested.
 *   3. `selectDisplayTiles` — pure "what do I show right now" policy: a tile
 *      that hasn't rendered yet reuses its nearest ready ANCESTOR (a coarser
 *      tile fully covers the same source range, just blockier) instead of
 *      showing nothing, exactly like `services::tiles`' nesting property was
 *      built for. Unit-tested with an injected `isReady`.
 *   4. The cache + `requestFilmstripTile` + `useFilmstripTiles` — impure: disk
 *      IO through `ipc.media.filmstripTile`, an in-memory LRU-bounded cache,
 *      and the React hook gluing it to a render. `isTauri()`-guarded; the
 *      hook resolves to an empty tile list off-Tauri (browser/e2e), so
 *      `ClipBox` keeps today's plain thumbnail look there.
 */

import { useEffect, useMemo, useReducer } from "react";
import { appCacheDir, join } from "@tauri-apps/api/path";
import { convertFileSrc, isTauri } from "@tauri-apps/api/core";

import type { MediaItem, TimelineItem } from "@/lib/bindings";
import { ipc } from "@/lib/ipc";
import { kickMediaCachePrune } from "@/features/media/thumbnails";

// ── 1. Grid math — mirrors services::tiles (Rust) exactly ───────────────────

/** Span of a tier-0 tile, ms. Halves exactly down to `TILE_MAX_TIER`. */
export const TILE_BASE_SPAN_MS = 64_000;
/** Finest addressable tier: 64 000 >> 8 = 250 ms per tile. */
export const TILE_MAX_TIER = 8;
/** Frames rendered side by side into one filmstrip tile. */
export const TILE_COLS_DEFAULT = 8;
/** Tile height in px — must match `services::tiles::TILE_HEIGHT_PX`. */
export const TILE_HEIGHT_PX = 72;

export function clampTier(tier: number): number {
  return Math.min(TILE_MAX_TIER, Math.max(0, Math.trunc(tier)));
}

/** How much timeline a tile covers at `tier`, ms. */
export function tileSpanMs(tier: number): number {
  return TILE_BASE_SPAN_MS >> clampTier(tier);
}

/** Stable, filename-safe cache key for one tile. */
export function tileKey(tier: number, index: number): string {
  return `z${clampTier(tier)}i${Math.max(0, Math.trunc(index))}`;
}

/** Index of the tile containing `ms` at `tier`. Negative times clamp to 0. */
export function tileIndexAt(ms: number, tier: number): number {
  return Math.floor(Math.max(0, ms) / tileSpanMs(tier));
}

/** The `[start, end)` ms range tile `index` covers at `tier`. */
export function tileRangeMs(tier: number, index: number): [number, number] {
  const span = tileSpanMs(tier);
  const start = Math.max(0, Math.trunc(index)) * span;
  return [start, start + span];
}

/** The tile at `tier - 1` containing tile `index` at `tier` — `null` at tier 0. */
export function parentTile(tier: number, index: number): number | null {
  if (clampTier(tier) === 0) return null;
  return Math.floor(Math.max(0, index) / 2);
}

export interface FilmTile {
  tier: number;
  index: number;
  startMs: number;
  endMs: number;
  key: string;
}

/**
 * Every tile at `tier` overlapping `[startMs, endMs)`, in order. Clamps
 * rather than rejects: a negative start snaps to 0, and an end at or before
 * the start still yields the single tile containing the start.
 */
export function tilesForRange(
  startMs: number,
  endMs: number,
  tier: number,
): FilmTile[] {
  const t = clampTier(tier);
  const start = Math.max(0, startMs);
  const end = Math.max(start + 1, endMs);
  const first = tileIndexAt(start, t);
  const last = tileIndexAt(end - 1, t);
  const out: FilmTile[] = [];
  for (let index = first; index <= last; index++) {
    const [s, e] = tileRangeMs(t, index);
    out.push({ tier: t, index, startMs: s, endMs: e, key: tileKey(t, index) });
  }
  return out;
}

// ── 2. Screen ↔ source-grid mapping ──────────────────────────────────────────

/** On-screen px a tier's tile would render at, given px-per-SOURCE-ms. */
function tileScreenWidthPx(tier: number, pxPerSourceMs: number): number {
  return tileSpanMs(tier) * pxPerSourceMs;
}

const DEFAULT_MAX_TILE_SCREEN_PX = 480;

/**
 * The tier whose on-screen tile width best fits the current zoom: the
 * COARSEST tier (smallest `t`, cheapest — fewest tiles, most reuse while
 * panning) whose tile still renders at or under `maxTileScreenPx`. Finer
 * tiers exist for when even tier 0 would still overflow that budget (deep
 * zoom-in); `TILE_MAX_TIER` is the floor when nothing fits.
 */
export function pickZoomTier(
  pxPerSourceMs: number,
  maxTileScreenPx: number = DEFAULT_MAX_TILE_SCREEN_PX,
): number {
  if (!(pxPerSourceMs > 0) || !Number.isFinite(pxPerSourceMs)) return 0;
  for (let t = 0; t <= TILE_MAX_TIER; t++) {
    if (tileScreenWidthPx(t, pxPerSourceMs) <= maxTileScreenPx) return t;
  }
  return TILE_MAX_TIER;
}

type ClipLike = Pick<
  TimelineItem,
  "timeline_start_ms" | "in_ms" | "out_ms" | "speed"
>;

/**
 * Map the clip's ON-SCREEN visible timeline window into SOURCE-media ms —
 * the range of the underlying file this clip currently shows on screen —
 * clamped to the clip's own `[in_ms, out_ms)` (never request frames the clip
 * doesn't use) and never empty.
 */
export function sourceRangeForVisible(
  item: ClipLike,
  visStartMs: number,
  visEndMs: number,
): { startMs: number; endMs: number } {
  const speed = Math.max(0.01, item.speed);
  const toSource = (t: number) =>
    item.in_ms + (t - item.timeline_start_ms) * speed;
  const a = toSource(visStartMs);
  const b = toSource(visEndMs);
  const lo = Math.min(a, b);
  const hi = Math.max(a, b);
  const startMs = Math.max(item.in_ms, lo);
  const endMs = Math.min(item.out_ms, hi);
  return { startMs, endMs: Math.max(endMs, startMs + 1) };
}

/** px-per-source-ms for a clip, given the timeline's px-per-TIMELINE-ms. */
export function pxPerSourceMs(item: ClipLike, pxPerMs: number): number {
  return pxPerMs / Math.max(0.01, item.speed);
}

/** Where one tile renders inside the clip box, in screen px relative to the
 *  box's own left edge (which sits at `item.timeline_start_ms`). */
export function tileScreenRect(
  item: ClipLike,
  tile: { startMs: number; endMs: number },
  pxPerMs: number,
): { leftPx: number; widthPx: number } {
  const speed = Math.max(0.01, item.speed);
  const leftPx = ((tile.startMs - item.in_ms) / speed) * pxPerMs;
  const widthPx = ((tile.endMs - tile.startMs) / speed) * pxPerMs;
  return { leftPx, widthPx };
}

// ── 3. Stale-while-loading selection policy ──────────────────────────────────

export interface TileSelection extends FilmTile {
  /** Key of the tile actually available to show now — itself, a coarser
   *  ready ancestor, or `null` if nothing is ready yet. */
  displayKey: string | null;
  /** True when `displayKey` is an ancestor standing in for the real tile. */
  stale: boolean;
}

/**
 * Resolve each `wanted` tile to what should actually render right now.
 * Ready tiles show themselves; a tile still loading walks its parent chain
 * (coarser tiers wholly contain it — see the module doc) for the nearest
 * ready ancestor to show as a blocky placeholder; with no ready ancestor
 * either, `displayKey` is `null` (caller falls back to no backdrop there).
 */
export function selectDisplayTiles(
  wanted: FilmTile[],
  isReady: (key: string) => boolean,
): TileSelection[] {
  return wanted.map((w) => {
    if (isReady(w.key)) {
      return { ...w, displayKey: w.key, stale: false };
    }
    let tier = w.tier;
    let index = w.index;
    for (;;) {
      const parent = parentTile(tier, index);
      if (parent === null) break;
      tier -= 1;
      index = parent;
      const key = tileKey(tier, index);
      if (isReady(key)) {
        return { ...w, displayKey: key, stale: true };
      }
    }
    return { ...w, displayKey: null, stale: false };
  });
}

// ── 4. Cache + IPC + hook (impure) ───────────────────────────────────────────

interface CacheEntry {
  status: "loading" | "ready" | "error";
  promise: Promise<string | null>;
}

/** Bound on distinct tiles held in memory across every clip/media — an LRU
 *  (insertion-order Map, touched on read) evicts the coldest once exceeded.
 *  240 tiles × ~8 frames each is generous for what's ever on screen at once. */
export const MAX_CACHED_TILES = 240;

const cache = new Map<string, CacheEntry>();

function cacheKey(mediaId: string, tier: number, index: number): string {
  return `${mediaId}:${tileKey(tier, index)}`;
}

function touch(key: string, entry: CacheEntry) {
  cache.delete(key);
  cache.set(key, entry); // re-insert = MRU end
  while (cache.size > MAX_CACHED_TILES) {
    const oldest = cache.keys().next().value;
    if (oldest === undefined) break;
    cache.delete(oldest);
  }
}

/** The resolved asset URL for a ready tile, or `null` otherwise. */
function tileUrl(mediaId: string, tier: number, index: number): string | null {
  const entry = cache.get(cacheKey(mediaId, tier, index));
  if (!entry || entry.status !== "ready") return null;
  // status is only "ready" once the promise has settled to a URL — safe to
  // read synchronously via the settled-value side channel below.
  return settledUrls.get(cacheKey(mediaId, tier, index)) ?? null;
}

// Settled URLs live in a side map (not the promise) so `tileUrl` can read them
// synchronously — the promise itself is only useful to `await`.
const settledUrls = new Map<string, string>();

/**
 * Kick off (or reuse) the render of one filmstrip tile. Memoized per
 * `(mediaId, tier, index)` — concurrent/rapid requests for the same tile
 * (scrolling back and forth) await the same in-flight promise. No-ops off-
 * Tauri or for audio-only media, resolving `null` so callers degrade cleanly.
 */
export function requestFilmstripTile(
  media: MediaItem,
  tier: number,
  index: number,
): Promise<string | null> {
  if (!isTauri() || media.kind === "audio_only") return Promise.resolve(null);
  // Same one-time-per-session sweep `thumbnailSrc` kicks off — belt and
  // braces in case a caller reaches filmstrip tiles without ever going
  // through the plain thumbnail path first (see `media_cache.rs`).
  kickMediaCachePrune();
  const key = cacheKey(media.id, tier, index);
  const existing = cache.get(key);
  if (existing) {
    touch(key, existing);
    return existing.promise;
  }
  const [startMs, endMs] = tileRangeMs(tier, index);
  const promise = (async () => {
    const out = await join(
      await appCacheDir(),
      "filmstrip",
      `${media.id}_${tileKey(tier, index)}.jpg`,
    );
    const written = await ipc.media.filmstripTile(
      media.path,
      startMs,
      endMs,
      TILE_COLS_DEFAULT,
      out,
    );
    return convertFileSrc(written);
  })().catch(() => null);

  const entry: CacheEntry = { status: "loading", promise };
  touch(key, entry);
  void promise.then((url) => {
    if (url) settledUrls.set(key, url);
    touch(key, { status: url ? "ready" : "error", promise });
  });
  return promise;
}

/** Test-only: drop every cached tile + in-flight request. */
export function __resetFilmstripCacheForTests() {
  cache.clear();
  settledUrls.clear();
}

const OVERSCAN_PX = 200;

/** One tile image to paint, already resolved to what actually exists. */
export interface FilmstripPaint {
  /** The DISPLAYED tile's key — stable React key, unique within the list. */
  key: string;
  /** The displayed tile's own grid address (a stand-in reports the ancestor's). */
  tier: number;
  index: number;
  /** Screen px relative to the clip box's left edge, for the tile's OWN range. */
  leftPx: number;
  widthPx: number;
  url: string;
  /** True when this is a coarse ancestor standing in for a still-loading tile. */
  stale: boolean;
}

/**
 * React hook: the filmstrip tiles to paint across a video clip box right now.
 * Empty off-Tauri (browser/e2e) or for non-video media — `ClipBox` falls back
 * to the existing single-thumbnail backdrop in that case. Screen geometry
 * (`leftPx`/`widthPx`) is relative to the clip box's own left edge.
 */
export function useFilmstripTiles(
  media: MediaItem | undefined,
  item: ClipLike,
  pxPerMs: number,
  visStartMs: number,
  visEndMs: number,
): FilmstripPaint[] {
  // Advances every time one of THIS clip's tiles settles. It has to be a real
  // dependency of the paint list below, not just a re-render trigger: the tile
  // cache lives OUTSIDE React, so none of the geometric inputs (`media`,
  // `wanted`, `pxPerMs`, `item`) change when a tile becomes available. A memo
  // keyed only on those keeps handing back the pre-load answer — the strip
  // stays blank until an unrelated scroll or edit happens to invalidate it.
  const [cacheEpoch, bumpCacheEpoch] = useReducer((n: number) => n + 1, 0);

  const wanted = useMemo(() => {
    if (!media || media.kind !== "video" || !isTauri()) return [];
    const ppSourceMs = pxPerSourceMs(item, pxPerMs);
    const overscanSourceMs = ppSourceMs > 0 ? OVERSCAN_PX / ppSourceMs : 0;
    const { startMs, endMs } = sourceRangeForVisible(
      item,
      visStartMs,
      visEndMs,
    );
    const tier = pickZoomTier(ppSourceMs);
    return tilesForRange(
      startMs - overscanSourceMs,
      endMs + overscanSourceMs,
      tier,
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    media?.id,
    media?.kind,
    item.timeline_start_ms,
    item.in_ms,
    item.out_ms,
    item.speed,
    pxPerMs,
    visStartMs,
    visEndMs,
  ]);

  const mediaId = media?.id;
  useEffect(() => {
    if (!mediaId || wanted.length === 0) return;
    let cancelled = false;
    for (const w of wanted) {
      void requestFilmstripTile(media!, w.tier, w.index).then(() => {
        if (!cancelled) bumpCacheEpoch();
      });
    }
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mediaId, wanted]);

  return useMemo(() => {
    if (!media || wanted.length === 0) return [];
    const selected = selectDisplayTiles(wanted, (key) => {
      // `key` is `tileKey(tier,index)`; recover tier/index isn't needed —
      // resolve readiness through the same cache key shape directly.
      return cache.get(`${media.id}:${key}`)?.status === "ready";
    });
    const out: FilmstripPaint[] = [];
    // A coarse ancestor covers SEVERAL of the wanted tiles at once, so the same
    // stand-in is selected by each of them. Paint it once: repeating it would
    // stack its opacity into a visible bright band.
    const painted = new Set<string>();
    for (const s of selected) {
      if (!s.displayKey || painted.has(s.displayKey)) continue;
      // Parse the display tile's own tier/index back out of its key: an
      // ancestor's JPEG holds 2^n× the source range of the tile it stands in
      // for, so it must be laid out at ITS OWN rect. Squeezing it into the
      // wanted tile's narrower slot would show the wrong frames at the wrong
      // positions — blocky is fine, misplaced is not. The clip box's
      // `overflow-hidden` trims whatever hangs past the edges.
      const m = /^z(\d+)i(\d+)$/.exec(s.displayKey);
      if (!m) continue;
      const dTier = Number(m[1]);
      const dIndex = Number(m[2]);
      const url = tileUrl(media.id, dTier, dIndex);
      if (!url) continue;
      painted.add(s.displayKey);
      const [dStart, dEnd] = tileRangeMs(dTier, dIndex);
      const { leftPx, widthPx } = tileScreenRect(
        item,
        { startMs: dStart, endMs: dEnd },
        pxPerMs,
      );
      out.push({
        key: s.displayKey,
        tier: dTier,
        index: dIndex,
        leftPx,
        widthPx,
        url,
        stale: s.displayKey !== s.key,
      });
    }
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [media, wanted, pxPerMs, item, cacheEpoch]);
}
