import { describe, it, expect } from "vitest";

import type { Effect, Project, TimelineItem } from "@/lib/bindings";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import { stackColorMatrix } from "../effects/registry";
import { approximationNotice, describeScene } from "./scene";

const BASE = SAMPLE_PROJECT;
const ITEM = BASE.timeline_items[0];

function withItems(items: TimelineItem[]): Project {
  return { ...BASE, timeline_items: items };
}

function fx(kind: string, params: unknown = {}): Effect {
  return { id: `fx-${kind}`, kind, params, enabled: true };
}

describe("describeScene — canvas", () => {
  it("describes the project's own canvas, the space compose.rs composites in", () => {
    const scene = describeScene(BASE, 1000);
    expect(scene.width).toBe(1920);
    expect(scene.height).toBe(1080);
  });

  it("falls back to 1080p when the project has no probed geometry", () => {
    const scene = describeScene(
      { ...BASE, video_width: 0, video_height: 0 },
      1000,
    );
    expect([scene.width, scene.height]).toEqual([1920, 1080]);
  });

  it("draws nothing without a project", () => {
    expect(describeScene(undefined, 0).layers).toEqual([]);
  });
});

describe("describeScene — layer selection", () => {
  it("draws the clip under the playhead", () => {
    const scene = describeScene(BASE, 5_000);
    expect(scene.layers).toHaveLength(1);
    expect(scene.layers[0].itemId).toBe("ti1");
    expect(scene.layers[0].mediaPath).toBe("/demo/sermon.mp4");
  });

  it("draws nothing in a gap — the compositor holds black, it does not guess", () => {
    expect(describeScene(BASE, 30_000).layers).toEqual([]);
  });

  it("uses the SAME top-most-track rule as the <video> path", () => {
    // Sharing `previewMap.activeVideoItem` is what stops the two preview paths
    // showing different clips at the same playhead.
    const upper: TimelineItem = { ...ITEM, id: "ti2", track_id: "tv2" };
    const project: Project = {
      ...BASE,
      tracks: [
        ...BASE.tracks,
        {
          id: "tv2",
          kind: "video",
          name: "Video 2",
          index: 2,
          enabled: true,
          locked: false,
          muted: false,
          solo: false,
        },
      ],
      timeline_items: [ITEM, upper],
    };
    expect(describeScene(project, 5_000).layers[0].itemId).toBe("ti2");
  });

  it("skips a disabled clip", () => {
    expect(
      describeScene(withItems([{ ...ITEM, enabled: false }]), 5_000).layers,
    ).toEqual([]);
  });
});

describe("describeScene — geometry mirrors compose.rs", () => {
  it("maps transform x/y to canvas pixels, like overlay=W*x:H*y", () => {
    const item: TimelineItem = {
      ...ITEM,
      transform: { ...ITEM.transform, x: 0.25, y: 0.5 },
    };
    const layer = describeScene(withItems([item]), 5_000).layers[0];
    expect(layer.x).toBe(480); // 1920 * 0.25
    expect(layer.y).toBe(540); // 1080 * 0.5
  });

  it("passes scale through as a SOURCE scale, not a fit-to-canvas", () => {
    // `scale=iw*s:ih*s` in the export — the clip is not resized to the canvas.
    const item: TimelineItem = {
      ...ITEM,
      transform: { ...ITEM.transform, scale: 0.5 },
    };
    expect(describeScene(withItems([item]), 5_000).layers[0].scale).toBe(0.5);
  });

  it("converts rotation to radians", () => {
    const item: TimelineItem = {
      ...ITEM,
      transform: { ...ITEM.transform, rotation_deg: 90 },
    };
    expect(
      describeScene(withItems([item]), 5_000).layers[0].rotationRad,
    ).toBeCloseTo(Math.PI / 2, 10);
  });

  it("clamps opacity and a negative scale, like set_transform does", () => {
    const item: TimelineItem = {
      ...ITEM,
      transform: { ...ITEM.transform, opacity: 5, scale: -2 },
    };
    const layer = describeScene(withItems([item]), 5_000).layers[0];
    expect(layer.alpha).toBe(1);
    expect(layer.scale).toBe(0);
  });
});

describe("describeScene — effects", () => {
  it("carries the clip's effect stack as one colour matrix", () => {
    const item: TimelineItem = {
      ...ITEM,
      effects: [fx("brightness", { amount: 0.2 }), fx("grayscale")],
    };
    const layer = describeScene(withItems([item]), 5_000).layers[0];
    expect(layer.colorMatrix).toEqual(stackColorMatrix(item.effects));
    expect(layer.colorMatrix).not.toBeNull();
  });

  it("is null when nothing contributes — same rule as the export", () => {
    const item: TimelineItem = {
      ...ITEM,
      effects: [fx("bloom"), fx("contrast", { amount: 1 })],
    };
    expect(describeScene(withItems([item]), 5_000).layers[0].colorMatrix).toBe(
      null,
    );
  });
});

describe("describeScene — honest gaps", () => {
  it("flags crop, which the export renders and the compositor does not", () => {
    const item: TimelineItem = {
      ...ITEM,
      transform: {
        ...ITEM.transform,
        crop: { x: 0, y: 0, width: 0.5, height: 0.5 },
      },
    };
    expect(describeScene(withItems([item]), 5_000).unsupported).toContain(
      "crop",
    );
  });

  it("flags a stacked composite the single-element preview cannot show", () => {
    const upper: TimelineItem = { ...ITEM, id: "ti2", track_id: "tv2" };
    const project: Project = {
      ...BASE,
      tracks: [
        ...BASE.tracks,
        {
          id: "tv2",
          kind: "video",
          name: "Video 2",
          index: 2,
          enabled: true,
          locked: false,
          muted: false,
          solo: false,
        },
      ],
      timeline_items: [ITEM, upper],
    };
    expect(describeScene(project, 5_000).unsupported).toContain(
      "stacked-layers",
    );
  });

  it("reports nothing unsupported on the ordinary single-clip timeline", () => {
    expect(describeScene(BASE, 5_000).unsupported).toEqual([]);
  });
});

// ── The honesty badge ────────────────────────────────────────────────────────
// `unsupported` was computed and asserted from day one, and rendered nowhere:
// a cropped clip drew uncropped and a stack drew as its top layer, with the
// user finding out at export. These pin the sentence the compositor now shows.

describe("approximationNotice", () => {
  // A stand-in for `useT` — the real catalogue is asserted by the i18n suite;
  // here what matters is WHICH keys are asked for.
  const t = (key: string) => `<${key}>`;

  it("says nothing when the preview reproduces the export", () => {
    expect(approximationNotice([], t)).toBeNull();
    expect(approximationNotice(describeScene(BASE, 5_000).unsupported, t)).toBe(
      null,
    );
  });

  it("names crop", () => {
    expect(approximationNotice(["crop"], t)).toBe(
      "<previewApproximate>: <previewApproxCrop>",
    );
  });

  it("names a stacked composite", () => {
    expect(approximationNotice(["stacked-layers"], t)).toBe(
      "<previewApproximate>: <previewApproxStack>",
    );
  });

  it("lists every gap in one sentence, in scene order", () => {
    expect(approximationNotice(["crop", "stacked-layers"], t)).toBe(
      "<previewApproximate>: <previewApproxCrop>, <previewApproxStack>",
    );
  });

  it("speaks for a real cropped scene end to end", () => {
    const cropped: TimelineItem = {
      ...ITEM,
      transform: {
        ...ITEM.transform,
        crop: { x: 0.1, y: 0.1, width: 0.8, height: 0.8 },
      },
    };
    const notice = approximationNotice(
      describeScene(withItems([cropped]), 5_000).unsupported,
      t,
    );
    expect(notice).toContain("<previewApproxCrop>");
  });
});
