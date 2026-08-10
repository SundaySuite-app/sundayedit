/**
 * Pure multi-lane layout math (Task 2D) — the DOM/React-free core behind the
 * multi-track timeline: which vertical lane a pointer is over (clip cross-track
 * drag), the timeline span of a clip, the visible-track stacking order, and the
 * overall timeline duration once clips + captions can outrun the source video.
 *
 * Kept free of React so the tricky bits (hit-testing, duration derivation) are
 * unit-tested in isolation, exactly like `geometry.ts` and `previewMap.ts`.
 */

import type { Project } from "@/lib/bindings/Project";
import type { TimelineItem } from "@/lib/bindings/TimelineItem";
import type { Track } from "@/lib/bindings/Track";
import { timelineEndMs } from "./previewMap";

/** The timeline span a clip occupies, in timeline-ms — the shape
 *  `geometry.visibleCaptions` virtualizes over. `end_ms` accounts for `speed`
 *  (mirrors the Rust `TimelineItem::timeline_end_ms`, via `previewMap`). */
export function itemSpan(item: TimelineItem): {
  start_ms: number;
  end_ms: number;
} {
  return { start_ms: item.timeline_start_ms, end_ms: timelineEndMs(item) };
}

/**
 * Tracks in stacking order for rendering: TOP lane first (highest `index`),
 * bottom lane last — matching `previewMap`'s "highest index wins" compositing.
 * Ties break on `id` so the order is stable across renders.
 */
export function stackedTracks(tracks: Track[]): Track[] {
  return [...tracks].sort((a, b) =>
    b.index !== a.index ? b.index - a.index : a.id < b.id ? -1 : 1,
  );
}

/**
 * The track a vertical offset lands on, resolved through the stacking order.
 * `y` is measured from the top of the lanes area (i.e. below the
 * ruler/waveform). Returns the `Track` (so callers can gate on
 * `kind`/`locked`) or null when outside every lane.
 */
export function trackAtY(
  y: number,
  tracks: Track[],
  laneH: number,
): Track | null {
  if (laneH <= 0 || y < 0) return null;
  const stacked = stackedTracks(tracks);
  const i = Math.floor(y / laneH);
  return i < stacked.length ? stacked[i] : null;
}

/** Screen-px band, straddling each lane boundary, a pointer must clear before
 *  a cross-track drag target switches lanes (see `trackAtYSticky`). */
export const TRACK_SWITCH_HYSTERESIS_PX = 8;

/**
 * `trackAtY`, but sticky: once a drag has settled on `currentTrackId`, the
 * pointer must cross that lane's boundary by more than `hysteresisPx` before
 * the target switches to the neighbouring lane. Landing back inside the
 * current lane always holds — only a boundary CROSSING is damped. Without
 * this, hovering exactly on a lane boundary flickers the drop target (and the
 * insertion highlight with it) every pixel of jitter.
 *
 * Falls back to plain `trackAtY` when there's no current track to anchor to
 * (drag just started, or its track no longer exists).
 */
export function trackAtYSticky(
  y: number,
  tracks: Track[],
  laneH: number,
  currentTrackId: string | null,
  hysteresisPx: number = TRACK_SWITCH_HYSTERESIS_PX,
): Track | null {
  if (laneH <= 0 || y < 0) return null;
  const stacked = stackedTracks(tracks);
  const currentIndex = currentTrackId
    ? stacked.findIndex((t) => t.id === currentTrackId)
    : -1;
  if (currentIndex < 0) return trackAtY(y, tracks, laneH);

  const top = currentIndex * laneH;
  const bottom = top + laneH;
  if (y < top && top - y < hysteresisPx) return stacked[currentIndex];
  if (y >= bottom && y - bottom < hysteresisPx) return stacked[currentIndex];

  const i = Math.floor(y / laneH);
  return i < stacked.length ? stacked[i] : null;
}

/**
 * The total timeline duration, ms — the bound the viewport clamps/pans against
 * once clips and captions can extend past the primary source video. Max over:
 * the primary `video_duration_ms`, every clip's timeline end, and every caption
 * end. Never negative.
 */
export function timelineDurationMs(project: Project): number {
  let max = Math.max(0, project.video_duration_ms);
  for (const item of project.timeline_items) {
    const end = timelineEndMs(item);
    if (end > max) max = end;
  }
  for (const c of project.captions) {
    if (c.end_ms > max) max = c.end_ms;
  }
  return max;
}
