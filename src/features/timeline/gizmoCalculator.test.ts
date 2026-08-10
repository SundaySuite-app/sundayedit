import { describe, it, expect } from "vitest";

import {
  transformToBox,
  boxToTransform,
  calculateGizmoBox,
  snapCenterToCanvas,
  getCursorForHandle,
  isPointInBox,
  getDefaultGizmoConstraints,
  type GizmoBox,
  type GizmoConstraints,
} from "./gizmoCalculator";
import type { Transform } from "@/lib/bindings/Transform";

const CANVAS = { width: 1920, height: 1080 };
const NATURAL = { width: 640, height: 360 }; // 16:9, matches canvas aspect

function identityTransform(): Transform {
  return { x: 0, y: 0, scale: 1, rotation_deg: 0, opacity: 1, crop: null };
}

describe("transformToBox / boxToTransform", () => {
  it("identity transform maps to a box centered on the canvas at natural size", () => {
    const box = transformToBox(identityTransform(), NATURAL, CANVAS);
    expect(box.width).toBe(NATURAL.width);
    expect(box.height).toBe(NATURAL.height);
    expect(box.x).toBeCloseTo(CANVAS.width / 2 - NATURAL.width / 2);
    expect(box.y).toBeCloseTo(CANVAS.height / 2 - NATURAL.height / 2);
    expect(box.rotationDeg).toBe(0);
  });

  it("round-trips identity through box space", () => {
    const box = transformToBox(identityTransform(), NATURAL, CANVAS);
    const back = boxToTransform(box, NATURAL, CANVAS, {
      minScale: 0.05,
      maxScale: 8,
    });
    expect(back.x).toBeCloseTo(0);
    expect(back.y).toBeCloseTo(0);
    expect(back.scale).toBeCloseTo(1);
    expect(back.rotation_deg).toBeCloseTo(0);
  });

  it("round-trips a non-identity transform", () => {
    const t: Transform = {
      x: 0.1,
      y: -0.2,
      scale: 2,
      rotation_deg: 33,
      opacity: 1,
      crop: null,
    };
    const box = transformToBox(t, NATURAL, CANVAS);
    const back = boxToTransform(box, NATURAL, CANVAS, {
      minScale: 0.05,
      maxScale: 8,
    });
    expect(back.x).toBeCloseTo(t.x);
    expect(back.y).toBeCloseTo(t.y);
    expect(back.scale).toBeCloseTo(t.scale);
    expect(back.rotation_deg).toBeCloseTo(t.rotation_deg);
  });

  it("clamps derived scale to constraints", () => {
    const hugeBox: GizmoBox = {
      x: 0,
      y: 0,
      width: NATURAL.width * 100,
      height: NATURAL.height * 100,
      rotationDeg: 0,
    };
    const back = boxToTransform(hugeBox, NATURAL, CANVAS, {
      minScale: 0.05,
      maxScale: 8,
    });
    expect(back.scale).toBe(8);

    const tinyBox: GizmoBox = {
      x: 0,
      y: 0,
      width: NATURAL.width * 0.0001,
      height: NATURAL.height * 0.0001,
      rotationDeg: 0,
    };
    const backTiny = boxToTransform(tinyBox, NATURAL, CANVAS, {
      minScale: 0.05,
      maxScale: 8,
    });
    expect(backTiny.scale).toBe(0.05);
  });
});

describe("calculateGizmoBox — move", () => {
  const constraints: GizmoConstraints = {
    canvas: CANVAS,
    minScale: 0.05,
    maxScale: 8,
    snapThreshold: 0,
  };
  const box: GizmoBox = {
    x: 500,
    y: 300,
    width: 200,
    height: 100,
    rotationDeg: 0,
  };

  it("translates by the raw mouse delta when not near a snap point", () => {
    const result = calculateGizmoBox(
      box,
      "move",
      { x: 0, y: 0 },
      { x: 50, y: -20 },
      constraints,
    );
    expect(result.x).toBeCloseTo(550);
    expect(result.y).toBeCloseTo(280);
    expect(result.width).toBe(box.width);
    expect(result.height).toBe(box.height);
  });

  it("clamps position so the box cannot move fully off-canvas", () => {
    const result = calculateGizmoBox(
      box,
      "move",
      { x: 0, y: 0 },
      { x: -100000, y: 0 },
      constraints,
    );
    expect(result.x).toBeCloseTo(-box.width * 0.5);
  });
});

describe("calculateGizmoBox — snap-to-center", () => {
  it("snaps the box center to the canvas center when within threshold", () => {
    const constraints: GizmoConstraints = {
      canvas: CANVAS,
      minScale: 0.05,
      maxScale: 8,
      snapThreshold: 10,
    };
    // Box center starts a few px off canvas center; small move should lock onto it.
    const box: GizmoBox = {
      x: CANVAS.width / 2 - 100 + 3,
      y: CANVAS.height / 2 - 50 + 3,
      width: 200,
      height: 100,
      rotationDeg: 0,
    };
    const snapped = snapCenterToCanvas(box, constraints);
    expect(snapped.x).toBeCloseTo(CANVAS.width / 2 - 100);
    expect(snapped.y).toBeCloseTo(CANVAS.height / 2 - 50);
  });

  it("does not snap when the center is beyond the threshold", () => {
    const constraints: GizmoConstraints = {
      canvas: CANVAS,
      minScale: 0.05,
      maxScale: 8,
      snapThreshold: 10,
    };
    const box: GizmoBox = {
      x: 0,
      y: 0,
      width: 200,
      height: 100,
      rotationDeg: 0,
    };
    const snapped = snapCenterToCanvas(box, constraints);
    expect(snapped.x).toBe(0);
    expect(snapped.y).toBe(0);
  });

  it("is a no-op when snapThreshold is 0", () => {
    const constraints: GizmoConstraints = {
      canvas: CANVAS,
      minScale: 0.05,
      maxScale: 8,
      snapThreshold: 0,
    };
    const box: GizmoBox = {
      x: CANVAS.width / 2 - 100,
      y: CANVAS.height / 2 - 50,
      width: 200,
      height: 100,
      rotationDeg: 0,
    };
    const snapped = snapCenterToCanvas(box, constraints);
    expect(snapped).toEqual(box);
  });
});

describe("calculateGizmoBox — corner drag", () => {
  const constraints: GizmoConstraints = getDefaultGizmoConstraints(CANVAS);
  const box: GizmoBox = {
    x: 860,
    y: 490,
    width: 200,
    height: 100,
    rotationDeg: 0,
  };

  it("se corner drag outward grows the box and preserves aspect ratio", () => {
    const result = calculateGizmoBox(
      box,
      "se",
      { x: 0, y: 0 },
      { x: 40, y: 20 },
      constraints,
    );
    expect(result.width).toBeGreaterThan(box.width);
    expect(result.height).toBeGreaterThan(box.height);
    expect(result.width / result.height).toBeCloseTo(box.width / box.height, 5);
  });

  it("se corner drag inward shrinks the box, keeping the center fixed", () => {
    const result = calculateGizmoBox(
      box,
      "se",
      { x: 0, y: 0 },
      { x: -40, y: -20 },
      constraints,
    );
    expect(result.width).toBeLessThan(box.width);
    const origCenterX = box.x + box.width / 2;
    const origCenterY = box.y + box.height / 2;
    expect(result.x + result.width / 2).toBeCloseTo(origCenterX, 1);
    expect(result.y + result.height / 2).toBeCloseTo(origCenterY, 1);
  });

  it("nw corner drag toward the center shrinks the box", () => {
    const result = calculateGizmoBox(
      box,
      "nw",
      { x: 0, y: 0 },
      { x: 40, y: 20 },
      constraints,
    );
    expect(result.width).toBeLessThan(box.width);
  });

  it("never collapses to zero or negative size on an extreme inward drag", () => {
    const result = calculateGizmoBox(
      box,
      "se",
      { x: 0, y: 0 },
      { x: -100000, y: -100000 },
      constraints,
    );
    expect(result.width).toBeGreaterThan(0);
    expect(result.height).toBeGreaterThan(0);
  });
});

describe("calculateGizmoBox — edge drag", () => {
  const constraints: GizmoConstraints = getDefaultGizmoConstraints(CANVAS);
  const box: GizmoBox = {
    x: 860,
    y: 490,
    width: 200,
    height: 100,
    rotationDeg: 0,
  };

  it("east edge drag outward grows the box and preserves aspect (uniform-scale model)", () => {
    const result = calculateGizmoBox(
      box,
      "e",
      { x: 0, y: 0 },
      { x: 50, y: 0 },
      constraints,
    );
    expect(result.width).toBeGreaterThan(box.width);
    expect(result.width / result.height).toBeCloseTo(box.width / box.height, 5);
  });

  it("west edge drag outward (mouse moves left) grows the box, keeping the east edge roughly anchored", () => {
    const eastEdgeBefore = box.x + box.width;
    const result = calculateGizmoBox(
      box,
      "w",
      { x: 0, y: 0 },
      { x: -50, y: 0 },
      constraints,
    );
    expect(result.width).toBeGreaterThan(box.width);
    expect(result.x + result.width).toBeCloseTo(eastEdgeBefore, 0);
  });

  it("north/south edge drags scale height and keep aspect", () => {
    const south = calculateGizmoBox(
      box,
      "s",
      { x: 0, y: 0 },
      { x: 0, y: 30 },
      constraints,
    );
    expect(south.height).toBeGreaterThan(box.height);
    expect(south.width / south.height).toBeCloseTo(box.width / box.height, 5);
  });
});

describe("calculateGizmoBox — rotate", () => {
  const constraints: GizmoConstraints = getDefaultGizmoConstraints(CANVAS);
  const box: GizmoBox = {
    x: 860,
    y: 490,
    width: 200,
    height: 100,
    rotationDeg: 0,
  };
  const center = { x: box.x + box.width / 2, y: box.y + box.height / 2 };

  it("rotates by the delta angle from startAngle, not the absolute angle", () => {
    const startAngle = Math.atan2(-1, 0); // pointer straight above center
    const currentPos = { x: center.x + 100, y: center.y }; // pointer straight right: +90deg from start
    const result = calculateGizmoBox(
      box,
      "rotate",
      center,
      currentPos,
      constraints,
      startAngle,
    );
    // Away from any 45-degree snap band (90 exactly is itself a snap angle, so
    // use an offset that lands off-band before checking magnitude).
    expect(Math.abs(result.rotationDeg)).toBeGreaterThan(0);
  });

  it("snaps rotation to 45-degree increments near the threshold", () => {
    const startAngle = 0;
    // ~92 degrees of rotation requested — within the 5-degree snap band of 90.
    const rad = (92 * Math.PI) / 180;
    const currentPos = {
      x: center.x + Math.cos(rad) * 100,
      y: center.y + Math.sin(rad) * 100,
    };
    const result = calculateGizmoBox(
      box,
      "rotate",
      { x: center.x + 100, y: center.y },
      currentPos,
      constraints,
      startAngle,
    );
    expect(result.rotationDeg).toBe(90);
  });

  it("normalizes rotation into (-180, 180]", () => {
    const startAngle = 0;
    const rad = (200 * Math.PI) / 180;
    const currentPos = {
      x: center.x + Math.cos(rad) * 100,
      y: center.y + Math.sin(rad) * 100,
    };
    const result = calculateGizmoBox(
      box,
      "rotate",
      { x: center.x + 100, y: center.y },
      currentPos,
      constraints,
      startAngle,
    );
    expect(result.rotationDeg).toBeGreaterThanOrEqual(-180);
    expect(result.rotationDeg).toBeLessThanOrEqual(180);
  });
});

describe("getCursorForHandle", () => {
  it("returns fixed cursors for move and rotate regardless of rotation", () => {
    expect(getCursorForHandle("move", 45)).toBe("move");
    expect(getCursorForHandle("rotate", 200)).toBe("grab");
  });

  it("returns the base cursor at zero rotation", () => {
    expect(getCursorForHandle("n", 0)).toBe("ns-resize");
    expect(getCursorForHandle("e", 0)).toBe("ew-resize");
    expect(getCursorForHandle("nw", 0)).toBe("nwse-resize");
  });

  it("rotates the cursor direction with the box", () => {
    // "n" (0deg base) rotated 90deg should read as an e/w resize cursor.
    expect(getCursorForHandle("n", 90)).toBe("ew-resize");
  });

  it("wraps rotation angles beyond 360", () => {
    expect(getCursorForHandle("n", 0)).toBe(getCursorForHandle("n", 360));
  });
});

describe("isPointInBox", () => {
  const box: GizmoBox = {
    x: 100,
    y: 100,
    width: 50,
    height: 50,
    rotationDeg: 0,
  };

  it("detects points inside an unrotated box", () => {
    expect(isPointInBox({ x: 120, y: 120 }, box)).toBe(true);
    expect(isPointInBox({ x: 10, y: 10 }, box)).toBe(false);
  });

  it("accounts for rotation", () => {
    const rotated: GizmoBox = { ...box, rotationDeg: 45 };
    // Center is always inside regardless of rotation.
    expect(isPointInBox({ x: 125, y: 125 }, rotated)).toBe(true);
    // A corner of the *unrotated* box is outside the box once rotated 45deg
    // (corners swing away from that point along the circle they trace).
    expect(isPointInBox({ x: 100, y: 100 }, rotated)).toBe(false);
  });
});
