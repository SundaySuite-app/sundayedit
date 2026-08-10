/**
 * Pure timeline math — the spatial core of the NLE timeline (Phase 1.3).
 *
 * Kept free of React/DOM so the tricky parts (time↔pixel mapping, zoom-around-
 * anchor, frame snapping, ruler tick selection, and the virtualization window
 * that keeps 5000+ captions smooth) are unit-tested in isolation.
 */

/** The visible viewport: zoom (pxPerMs), pan (scrollMs = time at x=0), width. */
export interface TimelineView {
  pxPerMs: number;
  scrollMs: number;
  widthPx: number;
}

/** Zoom bounds. ~0.00005 px/ms ≈ 20 s across 1000px (way-out); 2 px/ms is
 *  frame-level (one 30fps frame ≈ 33ms ≈ 66px). */
export const MIN_PX_PER_MS = 0.00002;
export const MAX_PX_PER_MS = 2;

export function clampZoom(pxPerMs: number): number {
  return Math.min(MAX_PX_PER_MS, Math.max(MIN_PX_PER_MS, pxPerMs));
}

export function msToX(ms: number, view: TimelineView): number {
  return (ms - view.scrollMs) * view.pxPerMs;
}

export function xToMs(x: number, view: TimelineView): number {
  return view.scrollMs + x / view.pxPerMs;
}

/** [startMs, endMs] currently visible in the viewport. */
export function visibleRange(view: TimelineView): [number, number] {
  return [view.scrollMs, view.scrollMs + view.widthPx / view.pxPerMs];
}

/**
 * Zoom by `factor` (>1 in, <1 out) while keeping the time under `anchorX`
 * pinned to that pixel — the standard "zoom toward the cursor" behaviour.
 * Clamps zoom and prevents scrolling before 0.
 */
export function zoomAround(
  view: TimelineView,
  factor: number,
  anchorX: number,
): TimelineView {
  const anchorMs = xToMs(anchorX, view);
  const pxPerMs = clampZoom(view.pxPerMs * factor);
  // Keep anchorMs at anchorX: anchorX = (anchorMs - scrollMs) * pxPerMs
  const scrollMs = Math.max(0, anchorMs - anchorX / pxPerMs);
  return { ...view, pxPerMs, scrollMs };
}

/** Round a time to the nearest frame boundary for the given fps. */
export function snapToFrame(ms: number, fps: number): number {
  if (fps <= 0) return ms;
  const frame = Math.round((ms / 1000) * fps);
  return Math.round((frame / fps) * 1000);
}

/**
 * Fixed SCREEN-pixel width of a clip/caption box's trim hit-zone, on EACH
 * edge, independent of zoom. Clip geometry (left/width) is computed in
 * screen px from `pxPerMs` already, so a caller just needs to know how much
 * of that width to carve off the two ends for "grab the edge to trim" versus
 * the middle "grab to move" — this is that one number, named so both edges
 * agree and a zoom step can't silently drift them apart.
 */
export const CLIP_EDGE_HIT_PX = 8;

/**
 * How wide (screen px) ONE trim edge's hit-zone should render at `clipWidthPx`.
 * Normally `edgePx`, but a clip narrower than `2×edgePx` would otherwise have
 * its two edge zones overlap and cover the box entirely — with no zone left to
 * grab-and-move a fully zoomed-out sliver of a clip. Below that width the
 * zones shrink to exactly half each, so the two together still tile the whole
 * box (trim-only, no dead move region, but never MORE than the box).
 */
export function edgeHitWidthPx(
  clipWidthPx: number,
  edgePx: number = CLIP_EDGE_HIT_PX,
): number {
  return Math.max(0, Math.min(edgePx, clipWidthPx / 2));
}

/** Top shuttle speed (×) reachable by repeated J/L taps. */
export const MAX_SHUTTLE = 8;

/**
 * The J/K/L transport state machine, returning the next signed playback rate
 * (negative = reverse, 0 = stop, 1 = realtime). Standard NLE behaviour:
 *   - `k` stops.
 *   - `l` plays forward — first tap from stop/reverse → 1×, then doubles
 *     (1→2→4→8, capped).
 *   - `j` mirrors `l` in reverse.
 */
export function shuttleRate(current: number, key: "j" | "k" | "l"): number {
  if (key === "k") return 0;
  if (key === "l") {
    return current < 1 ? 1 : Math.min(MAX_SHUTTLE, current * 2);
  }
  // "j"
  return current > -1 ? -1 : Math.max(-MAX_SHUTTLE, current * 2);
}

/**
 * Snap `ms` to the nearest target whose on-screen distance is within
 * `tolerancePx`; otherwise return `ms` unchanged. Targets are caption edges,
 * the playhead, and the timeline bounds — so dragging an edge "clicks" onto a
 * neighbour. Ties go to the closer pixel distance.
 */
export function snap(
  ms: number,
  targets: number[],
  pxPerMs: number,
  tolerancePx = 6,
): number {
  let best = ms;
  let bestPx = tolerancePx;
  for (const target of targets) {
    const dpx = Math.abs(target - ms) * pxPerMs;
    if (dpx <= bestPx) {
      bestPx = dpx;
      best = target;
    }
  }
  return best;
}

interface Spanned {
  start_ms: number;
  end_ms: number;
}

/** First index whose `end_ms` >= `ms`, via binary search on start-sorted
 *  captions. Used to bound the virtualization scan. */
function lowerBound<T extends Spanned>(items: T[], ms: number): number {
  let lo = 0;
  let hi = items.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (items[mid].end_ms < ms) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

/**
 * Indices of captions overlapping [startMs, endMs] (start-sorted input),
 * with `buffer` extra items each side so drag/scroll doesn't pop. This is the
 * virtualization window — only these get DOM nodes.
 */
export function visibleCaptions<T extends Spanned>(
  captions: T[],
  startMs: number,
  endMs: number,
  buffer = 5,
): { index: number; item: T }[] {
  const out: { index: number; item: T }[] = [];
  let i = Math.max(0, lowerBound(captions, startMs) - buffer);
  for (; i < captions.length; i++) {
    const c = captions[i];
    if (c.start_ms > endMs) {
      // Past the window — emit `buffer` trailing items then stop.
      for (let j = 0; j < buffer && i + j < captions.length; j++) {
        out.push({ index: i + j, item: captions[i + j] });
      }
      break;
    }
    out.push({ index: i, item: c });
  }
  return out;
}

const NICE_INTERVALS_MS = [
  100, 250, 500, 1_000, 2_000, 5_000, 10_000, 15_000, 30_000, 60_000, 120_000,
  300_000, 600_000, 900_000, 1_800_000, 3_600_000,
];

/** Pick the smallest "nice" tick interval whose on-screen spacing is at least
 *  `minPxBetween`, so ruler labels never crowd. */
export function tickIntervalMs(view: TimelineView, minPxBetween = 80): number {
  for (const interval of NICE_INTERVALS_MS) {
    if (interval * view.pxPerMs >= minPxBetween) return interval;
  }
  return NICE_INTERVALS_MS[NICE_INTERVALS_MS.length - 1];
}

/** Ruler tick times across the visible range at the chosen nice interval. */
export function rulerTicks(view: TimelineView, minPxBetween = 80): number[] {
  const interval = tickIntervalMs(view, minPxBetween);
  const [start, end] = visibleRange(view);
  const first = Math.ceil(start / interval) * interval;
  const ticks: number[] = [];
  for (let ms = first; ms <= end; ms += interval) ticks.push(ms);
  return ticks;
}

/** HH:MM:SS:FF timecode (frames for the given fps). Hours omitted under 1h. */
export function formatTimecode(ms: number, fps = 30): string {
  const clamped = Math.max(0, ms);
  const totalSec = Math.floor(clamped / 1000);
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = totalSec % 60;
  const f = Math.min(
    Math.max(0, Math.round(fps) - 1),
    Math.floor(((clamped % 1000) / 1000) * fps),
  );
  const pad = (n: number) => String(n).padStart(2, "0");
  const tail = `${pad(m)}:${pad(s)}:${pad(f)}`;
  return h > 0 ? `${h}:${tail}` : tail;
}
