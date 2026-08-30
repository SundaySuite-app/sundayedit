/**
 * Timeline ops added after `src/lib/ipc.ts` was frozen for another agent's
 * concurrent work (R3-B) — kept in their own module instead of touching that
 * file. Same `invoke` + `IPCError` shape `ipc.ts`'s internal `call()` uses, so
 * a caller can treat this exactly like any other `ipc.timeline.*` wrapper.
 *
 * See `duplicate_timeline_item` (services/timeline_ops.rs) for the placement
 * rule: a deep copy lands immediately after the original on the same track,
 * clamped into/around neighbours exactly like `add_timeline_item` /
 * `move_timeline_item`.
 */

import { invoke } from "@tauri-apps/api/core";

import type { AppError, Project } from "@/lib/bindings";
import { IPCError } from "@/lib/ipc";

/** Duplicate a clip; the backend mints the new item's id. */
export async function duplicateTimelineItem(
  project: Project,
  itemId: string,
): Promise<Project> {
  try {
    return await invoke<Project>("op_duplicate_timeline_item", {
      project,
      itemId,
    });
  } catch (raw) {
    if (raw && typeof raw === "object" && "code" in raw && "message" in raw) {
      throw new IPCError(raw as AppError);
    }
    throw raw instanceof Error ? raw : new Error(String(raw));
  }
}

/**
 * The id of the one item present in `after` but absent from `before` — used
 * to find a just-created item (e.g. a duplicate) without the backend having
 * to hand ids back out-of-band, matching how every other op here returns just
 * the new `Project`. `null` when nothing was added (defensive; should not
 * happen for the callers in this module).
 */
export function newestTimelineItemId(
  before: Project,
  after: Project,
): string | null {
  const beforeIds = new Set(before.timeline_items.map((i) => i.id));
  const added = after.timeline_items.find((i) => !beforeIds.has(i.id));
  return added?.id ?? null;
}
