/**
 * Pure scene description for the GPU preview compositor (E6; ADR-013).
 *
 * Same discipline as `previewMap.ts` and `mediaReconcile.ts`: the compositor
 * component is a thin **executor** that owns a Pixi `Application` and a hidden
 * `<video>`, and every decision about WHAT to draw is made here — DOM-free,
 * Pixi-free, and unit-testable without a GPU.
 *
 * ── Coordinates ─────────────────────────────────────────────────────────────
 * The scene is described in **project canvas pixels** (`video_width` ×
 * `video_height`), which is the same space `compose.rs` composites in, so the
 * numbers below are the direct mirror of the export's geometry:
 *
 * | export (`build_filter_complex`)        | preview (here)                |
 * | -------------------------------------- | ----------------------------- |
 * | `scale=iw*s:ih*s`                       | `scale = transform.scale`     |
 * | `rotate=<deg>*PI/180`                   | `rotationRad`                 |
 * | `format=rgba,colorchannelmixer=aa=<o>`  | `alpha`                       |
 * | `overlay=<W*x>:<H*y>`                   | `x`, `y` (top-left, unscaled) |
 * | per-item effect chain (`eq=`/`hue=`)    | `colorMatrix`                 |
 *
 * Note what that table means: the clip is overlaid at its SOURCE size times
 * `scale` — the export does not fit media to the canvas, and neither do we.
 *
 * ── Deliberately not modelled ───────────────────────────────────────────────
 * `Transform.crop` (the export's `crop=` filter) is not described here. The
 * compositor is fed by ONE hidden `<video>` — the element the existing
 * reconcile loop already drives (ADR-009 §1) — so it can only draw the layer
 * that element is showing, and a partial feature set on a preview that is
 * explicitly an approximation is worse than an honest gap. `describeScene`
 * therefore returns at most one layer and flags `unsupported` for anything the
 * export will render differently, so the UI can say so instead of lying. The
 * preview-render proxy (real ffmpeg) remains the exact answer.
 */

import type { Project } from "@/lib/bindings/Project";
import type { TKey } from "@/lib/i18n";
import { stackColorMatrix } from "../effects/registry";
import { visualItemsAt } from "../previewMap";

export interface CompositorLayer {
  itemId: string;
  /** Media path the layer's texture must come from (the `<video>` source). */
  mediaPath: string;
  /** Top-left position on the project canvas, in canvas pixels. */
  x: number;
  y: number;
  /** Uniform scale applied to the SOURCE pixels (not a fit-to-canvas). */
  scale: number;
  rotationRad: number;
  alpha: number;
  /** 5×4 colour matrix for the clip's effect stack, or null for none. */
  colorMatrix: number[] | null;
}

export interface CompositorScene {
  /** Canvas the layers are described against. */
  width: number;
  height: number;
  /** Layers to draw, back to front. Empty means "hold black". */
  layers: CompositorLayer[];
  /**
   * Aspects of the export the compositor cannot reproduce for this frame.
   * Empty on the common path; used to tell the user the preview is
   * approximate rather than to silently diverge.
   */
  unsupported: Array<"crop" | "stacked-layers">;
}

/** Fallback canvas when a project has no probed geometry yet. */
const FALLBACK_WIDTH = 1920;
const FALLBACK_HEIGHT = 1080;

/**
 * Describe the frame the compositor should draw for `playheadMs`.
 *
 * Layer selection AND the stacked-layers count both come from the one
 * `previewMap.visualItemsAt` call — the single mirror of `compose.rs`'s visual
 * stack. Deriving both from the same list is the point: this file used to
 * re-implement the rule privately in `countVisualItemsAt`, and the copy was
 * wrong in the same way the old `activeVideoItem` was (it required
 * `track.kind === "video"`). A video clip on an Overlay track was therefore
 * neither drawn NOR counted — so the "preview is approximate" badge stayed
 * silent about the very stack it exists to warn about.
 */
export function describeScene(
  project: Project | undefined,
  playheadMs: number,
): CompositorScene {
  const width =
    project && project.video_width > 0 ? project.video_width : FALLBACK_WIDTH;
  const height =
    project && project.video_height > 0
      ? project.video_height
      : FALLBACK_HEIGHT;
  const empty: CompositorScene = { width, height, layers: [], unsupported: [] };
  if (!project) return empty;

  // Bottom → top, exactly as the export chains its overlay nodes.
  const stack = visualItemsAt(project, playheadMs);
  if (stack.length === 0) return empty;

  const { item, media } = stack[stack.length - 1];
  const t = item.transform;
  const unsupported: CompositorScene["unsupported"] = [];
  if (t.crop) unsupported.push("crop");

  // More than one visual clip under the playhead means the export composites a
  // stack the single-element preview cannot show. Counting is cheap and the
  // honesty is worth it.
  if (stack.length > 1) {
    unsupported.push("stacked-layers");
  }

  return {
    width,
    height,
    layers: [
      {
        itemId: item.id,
        mediaPath: media.path,
        x: Math.round(width * t.x),
        y: Math.round(height * t.y),
        scale: t.scale > 0 ? t.scale : 0,
        rotationRad: (t.rotation_deg * Math.PI) / 180,
        alpha: Math.min(1, Math.max(0, t.opacity)),
        colorMatrix: stackColorMatrix(item.effects),
      },
    ],
    unsupported,
  };
}

/**
 * The one-line notice for what this frame's preview cannot reproduce, or
 * `null` when it reproduces everything.
 *
 * `unsupported` existed from the start but nothing rendered it, which made the
 * preview quietly lie about exactly the thing the product promises: a cropped
 * clip drew uncropped, a stacked composite drew as its top layer alone, and
 * the user found out at export. Flagging it is only honest if someone SAYS it,
 * so the sentence is built here — pure, and unit-tested — and the compositor
 * only has to place the badge.
 */
export function approximationNotice(
  unsupported: CompositorScene["unsupported"],
  t: (key: TKey) => string,
): string | null {
  if (unsupported.length === 0) return null;
  const parts = unsupported.map((u) =>
    t(u === "crop" ? "previewApproxCrop" : "previewApproxStack"),
  );
  return `${t("previewApproximate")}: ${parts.join(", ")}`;
}
