/**
 * Pure preview mapping (Task 2C) — resolves *which* video clip and *what source
 * time* the preview surface should show for a given timeline playhead.
 *
 * The NLE timeline can stack several video tracks and lay many clips per track;
 * the preview is a single <video> element, so we must pick one clip per frame:
 * the top-most (highest track `index`) enabled video track that has a clip
 * under the playhead, then map the playhead back into that clip's source media
 * time (accounting for `speed`). Keeping this DOM/React-free — like
 * `mediaSync` — lets us unit-test the selection + arithmetic offline.
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
 * Among enabled Video tracks, pick the TOP-most (highest `index`) track whose
 * item's [timeline_start_ms, timeline_end_ms) contains `playheadMs`, and
 * resolve that item's `source_media_id` to a MediaItem. Only items backed by
 * video-kind media count — export parity with `compose.rs::is_visual`, so an
 * audio clip parked on a video track can never hijack the preview. Returns
 * null when no such clip is under the playhead.
 */
export function activeVideoItem(
  project: Project,
  playheadMs: number,
): { item: TimelineItem; media: MediaItem } | null {
  if (!project.timeline_items || project.timeline_items.length === 0) {
    return null;
  }

  // Video tracks that are enabled, indexed for a quick top-most lookup.
  const videoTrackIndex = new Map<string, number>();
  for (const track of project.tracks) {
    if (track.kind === "video" && track.enabled) {
      videoTrackIndex.set(track.id, track.index);
    }
  }
  if (videoTrackIndex.size === 0) return null;

  let best: TimelineItem | null = null;
  let bestMedia: MediaItem | null = null;
  let bestIndex = -Infinity;
  for (const item of project.timeline_items) {
    if (!item.enabled) continue;
    const trackIdx = videoTrackIndex.get(item.track_id);
    if (trackIdx === undefined) continue; // not on an enabled video track
    if (
      playheadMs < item.timeline_start_ms ||
      playheadMs >= timelineEndMs(item)
    ) {
      continue; // playhead not within this clip
    }
    if (trackIdx <= bestIndex) continue;
    // Resolve the media as part of selection: a non-video (or unresolvable)
    // clip is not visual — skip it so a lower video clip can win, exactly
    // like the export's visual stack.
    const media = project.media.find((m) => m.id === item.source_media_id);
    if (!media || media.kind !== "video") continue;
    best = item;
    bestMedia = media;
    bestIndex = trackIdx;
  }
  if (!best || !bestMedia) return null;
  return { item: best, media: bestMedia };
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
