import { describe, it, expect } from "vitest";

import {
  solveCubicBezier,
  getEasingProgress,
  parseColor,
  interpolateColor,
  interpolateNumber,
  evaluateProperty,
  isKeyframed,
  type KeyframedProperty,
  type Keyframe,
} from "./animation";

describe("solveCubicBezier", () => {
  it("returns t unchanged at the endpoints", () => {
    expect(solveCubicBezier(0.42, 0, 1, 1, 0)).toBe(0);
    expect(solveCubicBezier(0.42, 0, 1, 1, 1)).toBe(1);
  });

  it("linear control points (0,0,1,1) is the identity curve", () => {
    for (const t of [0.1, 0.25, 0.5, 0.75, 0.9]) {
      expect(solveCubicBezier(0, 0, 1, 1, t)).toBeCloseTo(t, 3);
    }
  });

  it("ease-in-out (0.42,0,0.58,1) is symmetric around t=0.5", () => {
    const mid = solveCubicBezier(0.42, 0, 0.58, 1, 0.5);
    expect(mid).toBeCloseTo(0.5, 2);
  });

  it("stays within [0,1] for standard easing curves", () => {
    for (const t of [0, 0.2, 0.4, 0.6, 0.8, 1]) {
      const y = solveCubicBezier(0.42, 0, 1, 1, t);
      expect(y).toBeGreaterThanOrEqual(-1e-6);
      expect(y).toBeLessThanOrEqual(1 + 1e-6);
    }
  });
});

describe("getEasingProgress", () => {
  it.each([
    ["linear", 0.5, 0.5],
    ["linear", 0, 0],
    ["linear", 1, 1],
  ] as const)("%s at t=%f -> %f", (easing, t, expected) => {
    expect(getEasingProgress(easing, t)).toBeCloseTo(expected, 5);
  });

  it("ease-in starts slow (below linear before the midpoint)", () => {
    expect(getEasingProgress("ease-in", 0.25)).toBeLessThan(0.25);
  });

  it("ease-out starts fast (above linear before the midpoint)", () => {
    expect(getEasingProgress("ease-out", 0.25)).toBeGreaterThan(0.25);
  });

  it("cubic-bezier uses the supplied control points", () => {
    const withPoints = getEasingProgress(
      "cubic-bezier",
      0.5,
      [0.42, 0, 0.58, 1],
    );
    const easeInOut = getEasingProgress("ease-in-out", 0.5);
    expect(withPoints).toBeCloseTo(easeInOut, 5);
  });

  it("cubic-bezier without control points falls back to linear", () => {
    expect(getEasingProgress("cubic-bezier", 0.3)).toBe(0.3);
  });

  it("unknown easing keyword falls back to linear", () => {
    expect(getEasingProgress("bogus" as Keyframe<unknown>["easing"], 0.3)).toBe(
      0.3,
    );
  });
});

describe("parseColor", () => {
  it.each([
    ["transparent", [0, 0, 0, 0]],
    ["#fff", [255, 255, 255, 1]],
    ["#000", [0, 0, 0, 1]],
    ["#ff0000", [255, 0, 0, 1]],
    ["#00ff0080", [0, 255, 0, 128 / 255]],
    ["rgb(10, 20, 30)", [10, 20, 30, 1]],
    ["rgba(10, 20, 30, 0.5)", [10, 20, 30, 0.5]],
  ] as const)("parses %s", (input, expected) => {
    const [r, g, b, a] = parseColor(input);
    expect(r).toBeCloseTo(expected[0]);
    expect(g).toBeCloseTo(expected[1]);
    expect(b).toBeCloseTo(expected[2]);
    expect(a).toBeCloseTo(expected[3], 3);
  });

  it("falls back to opaque white for unrecognized input", () => {
    expect(parseColor("not-a-color")).toEqual([255, 255, 255, 1]);
  });

  it("#rgba shorthand expands each channel", () => {
    expect(parseColor("#f00f")).toEqual([255, 0, 0, 1]);
  });
});

describe("interpolateColor", () => {
  it("interpolates midpoint between black and white", () => {
    expect(interpolateColor("#000000", "#ffffff", 0.5)).toBe(
      "rgba(128, 128, 128, 1.000)",
    );
  });

  it("returns the start color at t=0 and end color (as rgba) at t=1", () => {
    expect(interpolateColor("#ff0000", "#00ff00", 0)).toBe(
      "rgba(255, 0, 0, 1.000)",
    );
    expect(interpolateColor("#ff0000", "#00ff00", 1)).toBe(
      "rgba(0, 255, 0, 1.000)",
    );
  });

  it("interpolates alpha", () => {
    expect(interpolateColor("rgba(0,0,0,0)", "rgba(0,0,0,1)", 0.5)).toBe(
      "rgba(0, 0, 0, 0.500)",
    );
  });
});

describe("interpolateNumber", () => {
  it.each([
    [0, 10, 0, 0],
    [0, 10, 1, 10],
    [0, 10, 0.5, 5],
    [-5, 5, 0.5, 0],
  ])("interpolateNumber(%f, %f, %f) = %f", (start, end, t, expected) => {
    expect(interpolateNumber(start, end, t)).toBeCloseTo(expected);
  });
});

describe("isKeyframed", () => {
  it("recognizes a KeyframedProperty", () => {
    expect(isKeyframed({ keyframes: [], defaultValue: 0 })).toBe(true);
  });

  it("rejects primitives, null, and plain objects without a keyframes array", () => {
    expect(isKeyframed(5)).toBe(false);
    expect(isKeyframed("x")).toBe(false);
    expect(isKeyframed(null)).toBe(false);
    expect(isKeyframed({ value: 1 })).toBe(false);
  });
});

describe("evaluateProperty", () => {
  it("returns undefined for an undefined property", () => {
    expect(evaluateProperty<number>(undefined, 0.5, 1)).toBeUndefined();
  });

  it("returns a static (non-keyframed) value directly", () => {
    expect(evaluateProperty<number>(42, 0.5, 1)).toBe(42);
  });

  it("returns defaultValue when there are no keyframes", () => {
    const prop: KeyframedProperty<number> = { keyframes: [], defaultValue: 7 };
    expect(evaluateProperty(prop, 0.5, 1)).toBe(7);
  });

  it("returns the single keyframe's value regardless of time offset", () => {
    const prop: KeyframedProperty<number> = {
      keyframes: [{ time: 0.3, value: 99, easing: "linear" }],
      defaultValue: 0,
    };
    expect(evaluateProperty(prop, 0, 1)).toBe(99);
    expect(evaluateProperty(prop, 1, 1)).toBe(99);
  });

  it("clamps to the first/last keyframe outside the range", () => {
    const prop: KeyframedProperty<number> = {
      keyframes: [
        { time: 0.2, value: 10, easing: "linear" },
        { time: 0.8, value: 20, easing: "linear" },
      ],
      defaultValue: 0,
    };
    expect(evaluateProperty(prop, 0, 1)).toBe(10);
    expect(evaluateProperty(prop, 1, 1)).toBe(20);
  });

  it("linearly interpolates numeric values between keyframes", () => {
    const prop: KeyframedProperty<number> = {
      keyframes: [
        { time: 0, value: 0, easing: "linear" },
        { time: 1, value: 100, easing: "linear" },
      ],
      defaultValue: 0,
    };
    expect(evaluateProperty(prop, 0.5, 1)).toBeCloseTo(50);
  });

  it("applies the left keyframe's easing to the segment", () => {
    const prop: KeyframedProperty<number> = {
      keyframes: [
        { time: 0, value: 0, easing: "ease-in" },
        { time: 1, value: 100, easing: "linear" },
      ],
      defaultValue: 0,
    };
    // ease-in is slow to start, so progress at t=0.5 should be below the linear 50.
    expect(evaluateProperty(prop, 0.5, 1)).toBeLessThan(50);
  });

  it("interpolates hex color string values", () => {
    const prop: KeyframedProperty<string> = {
      keyframes: [
        { time: 0, value: "#000000", easing: "linear" },
        { time: 1, value: "#ffffff", easing: "linear" },
      ],
      defaultValue: "#000000",
    };
    expect(evaluateProperty(prop, 0.5, 1)).toBe("rgba(128, 128, 128, 1.000)");
  });

  it("step-interpolates non-color string values at the 0.5 boundary", () => {
    const prop: KeyframedProperty<string> = {
      keyframes: [
        { time: 0, value: "left-label", easing: "linear" },
        { time: 1, value: "right-label", easing: "linear" },
      ],
      defaultValue: "left-label",
    };
    expect(evaluateProperty(prop, 0.4, 1)).toBe("left-label");
    expect(evaluateProperty(prop, 0.6, 1)).toBe("right-label");
  });

  it("finds the correct segment among 3+ unsorted keyframes", () => {
    const prop: KeyframedProperty<number> = {
      keyframes: [
        { time: 0.75, value: 30, easing: "linear" },
        { time: 0, value: 0, easing: "linear" },
        { time: 0.25, value: 10, easing: "linear" },
      ],
      defaultValue: -1,
    };
    // Segment [0.25, 0.75]: value goes 10 -> 30.
    expect(evaluateProperty(prop, 0.5, 1)).toBeCloseTo(20);
  });
});
