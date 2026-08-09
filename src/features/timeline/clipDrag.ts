/**
 * Pure clip-drag commit math — the DOM/React-free core behind releasing a clip
 * drag on the multi-track timeline. Maps a finished drag (move / left-edge trim
 * / right-edge trim, expressed as a TIMELINE-domain delta in ms) to the backend
 * op the component dispatches, as a discriminated union.
 *
 * Domain rules (mirrors `services/timeline_ops.rs::trim_timeline_item`):
 * `in_ms`/`out_ms` live in SOURCE-media ms; `timeline_start_ms` and the drag
 * delta live in TIMELINE ms; the two domains relate through `speed`
 * (source-delta = timeline-delta × speed, timeline length = (out − in) / speed).
 * So dragging the LEFT edge by Δ moves `in` by Δ×speed AND `timeline_start` by
 * Δ; dragging the RIGHT edge by Δ moves only `out`, by Δ×speed.
 *
 * The left edge clamps through ONE effective delta so both constraints
 * (`timeline_start ≥ 0`, `in ≥ 0`) stay coupled — clamping them independently
 * (as the backend must, seeing only absolute values) could shift the clip's
 * content when a drag overshoots past zero. Outputs are rounded to whole ms:
 * the delta can be fractional at fractional speeds (the right-edge timeline
 * base is start + (out − in)/speed) and the backend fields are integers.
 *
 * Kept free of React so the tricky bits are unit-tested in isolation, exactly
 * like `geometry.ts`, `laneLayout.ts` and `previewMap.ts`.
 */

import type { TimelineItem } from "@/lib/bindings/TimelineItem";

/** The three clip-drag gestures (matches the component's `ClipDrag["kind"]`). */
export type ClipDragKind = "move" | "resize-start" | "resize-end";

/** The origin-state of the dragged clip, captured at pointer-down. */
export type ClipDragItem = Pick<
  TimelineItem,
  "id" | "track_id" | "in_ms" | "out_ms" | "timeline_start_ms" | "speed"
>;

/** The backend op a finished drag commits to, or `none` for a no-op release. */
export type ClipDragOp =
  | { op: "none" }
  | { op: "move"; itemId: string; trackId: string; timelineStartMs: number }
  | {
      op: "trim-start";
      itemId: string;
      newInMs: number;
      newTimelineStartMs: number;
    }
  | { op: "trim-end"; itemId: string; newOutMs: number };

/**
 * Map a finished clip drag to its backend op. `deltaMs` is the TIMELINE-domain
 * displacement of the dragged edge; `targetTrackId` is the cross-track drop
 * target (move only — defaults to the clip's own track).
 */
export function clipDragToOp(
  item: ClipDragItem,
  dragKind: ClipDragKind,
  deltaMs: number,
  targetTrackId: string = item.track_id,
): ClipDragOp {
  const speed = Math.max(0.01, item.speed);

  if (dragKind === "move") {
    // Clamp so the clip's leading edge never goes negative.
    const d = Math.max(deltaMs, -item.timeline_start_ms);
    if (d === 0 && targetTrackId === item.track_id) return { op: "none" };
    return {
      op: "move",
      itemId: item.id,
      trackId: targetTrackId,
      timelineStartMs: Math.round(item.timeline_start_ms + d),
    };
  }

  if (dragKind === "resize-start") {
    // One effective delta honouring BOTH left bounds: timeline_start ≥ 0 and
    // source in ≥ 0 (a timeline Δ consumes Δ×speed of source).
    const d = Math.max(deltaMs, -item.timeline_start_ms, -item.in_ms / speed);
    if (d === 0) return { op: "none" };
    return {
      op: "trim-start",
      itemId: item.id,
      newInMs: Math.round(item.in_ms + d * speed),
      newTimelineStartMs: Math.round(item.timeline_start_ms + d),
    };
  }

  // resize-end: only the source-out edge moves, by Δ×speed. The backend clamps
  // to the media duration and keeps in < out.
  if (deltaMs === 0) return { op: "none" };
  return {
    op: "trim-end",
    itemId: item.id,
    newOutMs: Math.round(item.out_ms + deltaMs * speed),
  };
}
