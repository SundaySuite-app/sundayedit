/**
 * Transform-gizmo calculator — pure math for the 8-handle resize/rotate
 * overlay, adapted from the Clypra editor's transform calculator.
 *
 * Source: github.com/AIEraDev/Clypra @ 2e85676f0c56d1e5f28fabcd9a3ab9952442a35b
 *   path: src/components/editor/transform/calculator.ts
 * License: MIT (see THIRD-PARTY-NOTICES.md at repo root for full text).
 *
 * ---
 * Coordinate model adaptation
 * ---
 * Clypra's `Clip` carries an absolute pixel box in canvas space: `x, y`
 * (top-left) and `width, height`, with `rotation` in degrees. Corner/edge
 * drags there resize `width`/`height` independently and can end up with any
 * aspect ratio.
 *
 * SundayEdit's `Transform` (src/lib/bindings/Transform.ts, mirrors
 * `src-tauri/src/model.rs`) is resolution-independent and has no
 * width/height at all: `{ x, y, scale, rotation_deg, opacity, crop }`, where
 * `x`/`y` are fractional offsets of the output frame from the item's natural
 * centered position, and `scale` is a single uniform factor (identity =
 * `x:0, y:0, scale:1, rotation_deg:0`). There is no independent width/height
 * to store, so this port is necessarily uniform-scale-only — corner *and*
 * edge handles both scale the item proportionally from its "natural" size
 * (`naturalSize`, the on-screen box at `scale: 1`), there is no aspect-lock
 * toggle (aspect is always locked, by construction of the data model), and
 * the text-auto-height branch (Clypra clips can be text boxes with
 * content-driven height) is dropped — SundayEdit's `TextSpec` sizing is
 * handled elsewhere and is out of scope for this pure geometry module.
 *
 * The approach: convert the current `Transform` + `naturalSize` +
 * `canvasSize` into a pixel-space box (`transformToBox`), run Clypra's
 * corner/edge/rotate delta math against that box (ported near-verbatim,
 * `handle*` functions below), then convert the resulting box back into a
 * `Transform` patch (`boxToTransform`). `getCursorForHandle` and
 * `isPointInBox` are lifted with only the rename `Clip` -> `Box`.
 *
 * The center/edge snap guides are a fresh pure implementation
 * (`snapCenterToCanvas`) inspired by the *behavior* described in Clypra's
 * `TransformOverlay.tsx` ("stateful magnetic center snapping"), not a
 * lift — that logic lives inline in a React component there (drag-session
 * refs, escape thresholds) and isn't a pure function to port. What's kept
 * here is the free-standing geometric idea: snap the box center to the
 * canvas center/edges when within a pixel threshold.
 */

import type { Transform } from "@/lib/bindings/Transform";

export type GizmoHandle =
  "move" | "n" | "s" | "e" | "w" | "nw" | "ne" | "sw" | "se" | "rotate";

/** On-screen size of the item at `scale: 1`, in canvas pixels. */
export interface NaturalSize {
  width: number;
  height: number;
}

/** The render surface the transform's `x`/`y` fractions are relative to. */
export interface CanvasSize {
  width: number;
  height: number;
}

/** A pixel-space bounding box (top-left + size), pre-rotation, plus rotation. */
export interface GizmoBox {
  x: number;
  y: number;
  width: number;
  height: number;
  rotationDeg: number;
}

export interface GizmoConstraints {
  canvas: CanvasSize;
  minScale: number;
  maxScale: number;
  /** Pixel distance within which a dragged box center snaps to the canvas center/edges. */
  snapThreshold?: number;
}

export function getDefaultGizmoConstraints(
  canvas: CanvasSize,
): GizmoConstraints {
  return { canvas, minScale: 0.05, maxScale: 8, snapThreshold: 8 };
}

/** Convert a `Transform` (+ its natural unscaled size) into a canvas-pixel box. */
export function transformToBox(
  transform: Transform,
  naturalSize: NaturalSize,
  canvas: CanvasSize,
): GizmoBox {
  const width = naturalSize.width * transform.scale;
  const height = naturalSize.height * transform.scale;
  const centerX = canvas.width / 2 + transform.x * canvas.width;
  const centerY = canvas.height / 2 + transform.y * canvas.height;
  return {
    x: centerX - width / 2,
    y: centerY - height / 2,
    width,
    height,
    rotationDeg: transform.rotation_deg,
  };
}

/** Convert a canvas-pixel box back into a `Transform` patch (x, y, scale, rotation_deg). */
export function boxToTransform(
  box: GizmoBox,
  naturalSize: NaturalSize,
  canvas: CanvasSize,
  constraints: Pick<GizmoConstraints, "minScale" | "maxScale">,
): Pick<Transform, "x" | "y" | "scale" | "rotation_deg"> {
  const centerX = box.x + box.width / 2;
  const centerY = box.y + box.height / 2;

  // Uniform scale: derive from whichever natural dimension is non-degenerate,
  // averaging when both are usable so a stray aspect mismatch (e.g. from a
  // non-uniform intermediate box) doesn't bias toward one axis.
  const scaleFromWidth =
    naturalSize.width > 0 ? box.width / naturalSize.width : undefined;
  const scaleFromHeight =
    naturalSize.height > 0 ? box.height / naturalSize.height : undefined;
  const candidates = [scaleFromWidth, scaleFromHeight].filter(
    (s): s is number => s !== undefined && Number.isFinite(s),
  );
  const rawScale =
    candidates.length > 0
      ? candidates.reduce((a, b) => a + b, 0) / candidates.length
      : 1;
  const scale = Math.max(
    constraints.minScale,
    Math.min(constraints.maxScale, rawScale),
  );

  return {
    x: canvas.width > 0 ? (centerX - canvas.width / 2) / canvas.width : 0,
    y: canvas.height > 0 ? (centerY - canvas.height / 2) / canvas.height : 0,
    scale,
    rotation_deg: box.rotationDeg,
  };
}

/**
 * Calculate the new box from a handle drag operation.
 *
 * @param box - The box state at drag start (NOT live state — avoids compounding deltas across frames)
 * @param handle - Which handle is being dragged
 * @param startMousePos - Mouse position at drag start (canvas space)
 * @param currentMousePos - Current mouse position (canvas space)
 * @param constraints - Gizmo constraints (canvas bounds, scale limits, snap threshold)
 * @param startAngle - For rotation: the initial angle at mousedown (radians). Optional.
 */
export function calculateGizmoBox(
  box: GizmoBox,
  handle: GizmoHandle,
  startMousePos: { x: number; y: number },
  currentMousePos: { x: number; y: number },
  constraints: GizmoConstraints,
  startAngle?: number,
): GizmoBox {
  const rawDelta = {
    x: currentMousePos.x - startMousePos.x,
    y: currentMousePos.y - startMousePos.y,
  };

  const rotationRad = (box.rotationDeg * Math.PI) / 180;
  const cosTheta = Math.cos(rotationRad);
  const sinTheta = Math.sin(rotationRad);

  // Project mouse delta into the box's rotated local coordinate system.
  const localDelta = {
    x: rawDelta.x * cosTheta + rawDelta.y * sinTheta,
    y: -rawDelta.x * sinTheta + rawDelta.y * cosTheta,
  };

  switch (handle) {
    case "move":
      return handleMove(box, rawDelta, constraints);
    case "nw":
    case "ne":
    case "sw":
    case "se":
      return handleCornerDrag(box, handle, localDelta, constraints);
    case "n":
    case "s":
    case "e":
    case "w":
      return handleEdgeDrag(
        box,
        handle,
        localDelta,
        constraints,
        cosTheta,
        sinTheta,
      );
    case "rotate":
      return handleRotation(box, currentMousePos, startAngle);
    default:
      return box;
  }
}

/** Handle move (drag border): translate, then snap the center toward canvas center/edges. */
function handleMove(
  box: GizmoBox,
  delta: { x: number; y: number },
  constraints: GizmoConstraints,
): GizmoBox {
  let newX = box.x + delta.x;
  let newY = box.y + delta.y;

  // Allow partial off-canvas, matching the "move constrained but not clipped" feel.
  const minX = -box.width * 0.5;
  const maxX = constraints.canvas.width - box.width * 0.5;
  const minY = -box.height * 0.5;
  const maxY = constraints.canvas.height - box.height * 0.5;
  newX = Math.max(minX, Math.min(maxX, newX));
  newY = Math.max(minY, Math.min(maxY, newY));

  const snapped = snapCenterToCanvas({ ...box, x: newX, y: newY }, constraints);
  return snapped;
}

/** Corner drag: uniform scale around the box center (aspect is always locked in our model). */
function handleCornerDrag(
  box: GizmoBox,
  handle: "nw" | "ne" | "sw" | "se",
  delta: { x: number; y: number },
  constraints: GizmoConstraints,
): GizmoBox {
  const centerX = box.x + box.width / 2;
  const centerY = box.y + box.height / 2;
  const dirX = handle === "ne" || handle === "se" ? 1 : -1;
  const dirY = handle === "sw" || handle === "se" ? 1 : -1;
  const primaryDelta =
    Math.abs(delta.x) >= Math.abs(delta.y) ? delta.x * dirX : delta.y * dirY;

  const refDim = Math.max(1, Math.max(box.width, box.height));
  const scaleFactor = 1 + (primaryDelta * 2) / refDim;

  const newWidth = Math.max(1, box.width * scaleFactor);
  const newHeight = Math.max(1, box.height * scaleFactor);

  const clamped = clampBoxScale(newWidth, newHeight, box, constraints);

  return {
    x: centerX - clamped.width / 2,
    y: centerY - clamped.height / 2,
    width: clamped.width,
    height: clamped.height,
    rotationDeg: box.rotationDeg,
  };
}

/** Edge drag: single-axis pixel resize, then re-derived to a uniform scale (aspect always locked). */
function handleEdgeDrag(
  box: GizmoBox,
  handle: "n" | "s" | "e" | "w",
  delta: { x: number; y: number },
  constraints: GizmoConstraints,
  cosTheta: number,
  sinTheta: number,
): GizmoBox {
  const centerX = box.x + box.width / 2;
  const centerY = box.y + box.height / 2;

  // The edge moves by the local-space delta along its axis; convert that
  // linear delta into a uniform scale factor via the aspect-preserving axis.
  let axisDelta: number;
  let sign: number;
  let refDim: number;
  switch (handle) {
    case "n":
      axisDelta = -delta.y;
      sign = -1;
      refDim = box.height;
      break;
    case "s":
      axisDelta = delta.y;
      sign = 1;
      refDim = box.height;
      break;
    case "e":
      axisDelta = delta.x;
      sign = 1;
      refDim = box.width;
      break;
    case "w":
    default:
      axisDelta = -delta.x;
      sign = -1;
      refDim = box.width;
      break;
  }

  const scaleFactor = 1 + (axisDelta * 2) / Math.max(1, refDim);
  const newWidth = Math.max(1, box.width * scaleFactor);
  const newHeight = Math.max(1, box.height * scaleFactor);
  const clamped = clampBoxScale(newWidth, newHeight, box, constraints);

  const dw = clamped.width - box.width;
  const dh = clamped.height - box.height;
  const isVertical = handle === "n" || handle === "s";
  const half = (isVertical ? dh : dw) / 2;
  const axisSign = sign;

  // Move the center along the box's rotated axis so the *opposite* edge stays put.
  const newCenterX = isVertical
    ? centerX + axisSign * half * sinTheta
    : centerX + axisSign * half * cosTheta;
  const newCenterY = isVertical
    ? centerY - axisSign * half * cosTheta
    : centerY + axisSign * half * sinTheta;

  return {
    x: newCenterX - clamped.width / 2,
    y: newCenterY - clamped.height / 2,
    width: clamped.width,
    height: clamped.height,
    rotationDeg: box.rotationDeg,
  };
}

// Box-level clamp keeps pixel dimensions sane (never zero/negative) and
// aspect-locked (uniform-scale invariant); the min/max *scale* limits from
// GizmoConstraints are enforced later, in boxToTransform, where the box's
// natural (scale: 1) size is known.
function clampBoxScale(
  width: number,
  height: number,
  box: GizmoBox,
  _constraints: GizmoConstraints,
): { width: number; height: number } {
  const aspect = box.width / Math.max(1, box.height);
  // Re-derive a single scale factor from whichever dimension moved more, so
  // corner/edge drags never desync width/height (uniform-scale invariant).
  const scaleFromWidth = width / Math.max(1, box.width);
  const scaleFromHeight = height / Math.max(1, box.height);
  const scale =
    Math.abs(scaleFromWidth - 1) >= Math.abs(scaleFromHeight - 1)
      ? scaleFromWidth
      : scaleFromHeight;

  const minDim = 4; // absolute pixel floor so a handle can never collapse the box to zero
  let w = Math.max(minDim, box.width * scale);
  let h = Math.max(minDim, box.height * scale);

  // Respect aspect explicitly (defensive — scale already keeps it, but the floor above can break it).
  if (w / h > aspect) {
    h = w / aspect;
  } else {
    w = h * aspect;
  }

  return { width: Math.max(minDim, w), height: Math.max(minDim, h) };
}

/**
 * Handle rotation around the box center. Delta-angle from `startAngle`
 * avoids an initial jump snap on mousedown, and near-axis-aligned angles
 * snap to 45-degree increments for a "professional NLE" feel.
 */
function handleRotation(
  box: GizmoBox,
  mousePos: { x: number; y: number },
  startAngle?: number,
): GizmoBox {
  const centerX = box.x + box.width / 2;
  const centerY = box.y + box.height / 2;
  const currentAngle = Math.atan2(mousePos.y - centerY, mousePos.x - centerX);

  let degrees: number;
  if (startAngle !== undefined) {
    const deltaAngle = currentAngle - startAngle;
    degrees = box.rotationDeg + (deltaAngle * 180) / Math.PI;
  } else {
    degrees = (currentAngle * 180) / Math.PI;
  }

  degrees = (((degrees % 360) + 540) % 360) - 180;

  const snapThreshold = 5; // degrees
  const snapAngles = [0, 45, 90, 135, 180, -45, -90, -135, -180];
  for (const snapAngle of snapAngles) {
    if (Math.abs(degrees - snapAngle) < snapThreshold) {
      degrees = snapAngle;
      break;
    }
  }

  return { ...box, rotationDeg: degrees };
}

/**
 * Snap the box's center toward the canvas center (both axes independently)
 * when within `constraints.snapThreshold` pixels. Pure geometric equivalent
 * of Clypra's stateful magnetic-snap UX (see header note) — no drag-session
 * state, just "is the center close enough right now".
 */
export function snapCenterToCanvas(
  box: GizmoBox,
  constraints: GizmoConstraints,
): GizmoBox {
  const threshold = constraints.snapThreshold ?? 0;
  if (threshold <= 0) return box;

  const centerX = box.x + box.width / 2;
  const centerY = box.y + box.height / 2;
  const canvasCenterX = constraints.canvas.width / 2;
  const canvasCenterY = constraints.canvas.height / 2;

  let x = box.x;
  let y = box.y;
  if (Math.abs(centerX - canvasCenterX) <= threshold) {
    x = canvasCenterX - box.width / 2;
  }
  if (Math.abs(centerY - canvasCenterY) <= threshold) {
    y = canvasCenterY - box.height / 2;
  }

  return { ...box, x, y };
}

/**
 * Get the cursor style for a gizmo handle, accounting for box rotation so
 * the resize direction shown matches what dragging would actually do.
 */
export function getCursorForHandle(
  handle: GizmoHandle,
  rotationDeg: number = 0,
): string {
  const baseCursors: Record<GizmoHandle, string> = {
    move: "move",
    nw: "nwse-resize",
    ne: "nesw-resize",
    sw: "nesw-resize",
    se: "nwse-resize",
    n: "ns-resize",
    s: "ns-resize",
    e: "ew-resize",
    w: "ew-resize",
    rotate: "grab",
  };

  if (handle === "move" || handle === "rotate") {
    return baseCursors[handle];
  }

  // Resize handles: rotate the cursor to match box rotation. Cursor
  // directions cycle every 45 degrees through 8 compass directions.
  const cursorAngles: string[] = [
    "ns-resize", // 0
    "nesw-resize", // 45
    "ew-resize", // 90
    "nwse-resize", // 135
    "ns-resize", // 180
    "nesw-resize", // 225
    "ew-resize", // 270
    "nwse-resize", // 315
  ];

  const handleBaseAngle: Record<string, number> = {
    n: 0,
    ne: 45,
    e: 90,
    se: 135,
    s: 180,
    sw: 225,
    w: 270,
    nw: 315,
  };

  const baseAngle = handleBaseAngle[handle] ?? 0;
  const totalAngle = (baseAngle + rotationDeg + 360) % 360;
  const index = Math.round(totalAngle / 45) % 8;

  return cursorAngles[index];
}

/**
 * Check if a point (canvas space) is inside a box's bounds, accounting for
 * rotation by inverse-rotating the point around the box center first.
 */
export function isPointInBox(
  point: { x: number; y: number },
  box: GizmoBox,
): boolean {
  if (box.rotationDeg === 0) {
    return (
      point.x >= box.x &&
      point.x <= box.x + box.width &&
      point.y >= box.y &&
      point.y <= box.y + box.height
    );
  }

  const centerX = box.x + box.width / 2;
  const centerY = box.y + box.height / 2;
  const dx = point.x - centerX;
  const dy = point.y - centerY;

  const rad = (-box.rotationDeg * Math.PI) / 180;
  const cos = Math.cos(rad);
  const sin = Math.sin(rad);
  const unrotatedX = dx * cos - dy * sin + centerX;
  const unrotatedY = dx * sin + dy * cos + centerY;

  return (
    unrotatedX >= box.x &&
    unrotatedX <= box.x + box.width &&
    unrotatedY >= box.y &&
    unrotatedY <= box.y + box.height
  );
}
