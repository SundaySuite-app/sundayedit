/**
 * Pure preview mapping (Task 2C) — resolves *which* video clip and *what source
 * time* the preview surface should show for a given timeline playhead.
 *
 * The NLE timeline can stack several video tracks and lay many clips per track;
 * the preview is a single <video> element, so we must pick one clip per frame:
 * the TOP of the export's visual stack under the playhead, then map the
 * playhead back into that clip's source media time (accounting for `speed`).
 * Keeping this DOM/React-free — like `mediaSync` — lets us unit-test the
 * selection + arithmetic offline.
 *
 * ── The stack rule is the export's, not the timeline's ──────────────────────
 * {@link visualItemsAt} is the single mirror of `compose.rs`:
 *
 *   | compose.rs (`build_filter_complex`)   | here (`visualItemsAt`)          |
 *   | ------------------------------------- | ------------------------------- |
 *   | `it.enabled`                          | `item.enabled`                  |
 *   | `is_visual` — media kind is Video     | `media.kind === "video"`        |
 *   | `track_visible` — `is_none_or(enabled)`| track missing OR `track.enabled`|
 *   | `track_index` — `unwrap_or(i32::MAX)` | `stackIndex`, missing = MAX     |
 *   | sort `(track_index, timeline_start)`  | same sort; last = top           |
 *
 * Note what `is_visual` does NOT check: the TRACK's kind. A video clip dropped
 * on an Overlay track (the media bin offers "add overlay track", and the lane
 * drop handler allows it) is composited by the export exactly like one on a
 * Video track. This mirror used to filter on `track.kind === "video"`, so that
 * clip was invisible in the preview and present in the render — the preview
 * hiding something the export WILL draw, which is the half of the parity
 * invariant that costs the user a re-export. Every consumer (the `<video>`
 * path, the Pixi compositor, the "preview is approximate" badge) now derives
 * from this one function, so the two rules cannot drift apart again.
 */

import type { MediaItem } from "@/lib/bindings/MediaItem";
import type { Project } from "@/lib/bindings/Project";
import type { TimelineItem } from "@/lib/bindings/TimelineItem";

/** Where an item ends on the timeline, accounting for `speed`. Mirrors the
 *  Rust `TimelineItem::timeline_end_ms` (floor division, speed floored at
 *  0.01 so a zero/near-zero speed can't divide by zero). */
export function timelineEndMs(item: TimelineItem): number {
  const speed = Math.max(0.01, item.speed);
  return (
    item.timeline_start_ms + Math.trunc((item.out_ms - item.in_ms) / speed)
  );
}

/**
 * A clip the export will composite at `playheadMs`, with the stacking index it
 * will be composited at.
 */
export interface VisualItem {
  item: TimelineItem;
  media: MediaItem;
  /** The owning track's `index`; a missing track composites on top (i32::MAX). */
  stackIndex: number;
}

/** Mirrors Rust's `i32::MAX` fallback for an item whose track cannot be found. */
const MISSING_TRACK_INDEX = 2147483647;

/**
 * Every clip the export's visual stack contains at `playheadMs`, ordered
 * BOTTOM → TOP — the same order `build_filter_complex` chains its `overlay`
 * nodes in, so the last element is the frame the viewer actually sees.
 *
 * Pure; no DOM, no React, no Pixi.
 */
export function visualItemsAt(
  project: Project,
  playheadMs: number,
): VisualItem[] {
  if (!project.timeline_items || project.timeline_items.length === 0) return [];

  // Track lookup once: `tracks` is small but this runs per frame.
  const tracks = new Map(project.tracks.map((tr) => [tr.id, tr]));

  const found: VisualItem[] = [];
  for (const item of project.timeline_items) {
    if (!item.enabled) continue;
    if (
      playheadMs < item.timeline_start_ms ||
      playheadMs >= timelineEndMs(item)
    ) {
      continue; // playhead not within this clip
    }
    // `track_visible`: an item whose track cannot be resolved is treated as
    // visible, exactly like Rust's `is_none_or`.
    const track = tracks.get(item.track_id);
    if (track && !track.enabled) continue;
    // `is_visual`: the MEDIA must be video-kind. The track's kind is not
    // consulted — the export doesn't consult it either.
    const media = project.media.find((m) => m.id === item.source_media_id);
    if (!media || media.kind !== "video") continue;
    found.push({
      item,
      media,
      stackIndex: track ? track.index : MISSING_TRACK_INDEX,
    });
  }

  // `video_items.sort_by(track_index.then(timeline_start_ms))` — a stable sort
  // on both sides, so ties keep timeline order in the same way.
  found.sort(
    (a, b) =>
      a.stackIndex - b.stackIndex ||
      a.item.timeline_start_ms - b.item.timeline_start_ms,
  );
  return found;
}

/**
 * The clip on TOP of the export's visual stack at `playheadMs`, or null when
 * the export would draw bare canvas there.
 *
 * Thin wrapper over {@link visualItemsAt} so the single <video> preview and
 * the compositor's layer choice cannot disagree with the export.
 */
export function activeVideoItem(
  project: Project,
  playheadMs: number,
): { item: TimelineItem; media: MediaItem } | null {
  const stack = visualItemsAt(project, playheadMs);
  if (stack.length === 0) return null;
  const top = stack[stack.length - 1];
  return { item: top.item, media: top.media };
}

/**
 * Map a timeline playhead into the active item's source-media time (seconds).
 * At `timeline_start_ms` this is `in_ms`; it advances by `speed`× realtime.
 */
export function sourceTimeSec(item: TimelineItem, playheadMs: number): number {
  return (
    (item.in_ms + (playheadMs - item.timeline_start_ms) * item.speed) / 1000
  );
}
