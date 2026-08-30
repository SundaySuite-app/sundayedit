/**
 * Preview ↔ export parity for the VISUAL STACK (R1 trust round).
 *
 * The export is the source of truth: `src-tauri/src/services/compose.rs`
 * decides which clips get composited at a given moment with
 *
 *     .filter(|it| it.enabled && track_visible(project, it) && is_visual(project, it))
 *     video_items.sort_by(track_index(a).cmp(track_index(b))
 *                          .then(a.timeline_start_ms.cmp(&b.timeline_start_ms)))
 *
 * where
 *
 *     is_visual      → the item's MEDIA is MediaKind::Video      (track kind: not consulted)
 *     track_visible  → track_of(item).is_none_or(|t| t.enabled)  (missing track = visible)
 *     track_index    → track_of(item).map(index).unwrap_or(i32::MAX)
 *
 * The preview mirrors it in `previewMap.visualItemsAt`, and everything the
 * user actually looks at is derived from that one function: the `<video>`
 * element's clip (`activeVideoItem`), the Pixi compositor's layer, and the
 * "preview is approximate" badge (`describeScene(...).unsupported`).
 *
 * This file is the drift alarm. Each case below names the clause of the Rust
 * filter it pins, so a change on either side that isn't mirrored fails here
 * rather than at the user's export. The seam it guards is the shape described
 * in the suite's `reference-seam-bugs` note: two layers each correct against
 * their own tests, disagreeing only at the boundary between them.
 *
 * The bug that motivated it: the preview's rule additionally required
 * `track.kind === "video"`. Nothing in the export, the ops layer, or the drop
 * handler enforces that — `MediaBin` offers "add overlay track" and
 * `add_timeline_item` accepts a clip on any track — so a video clip on an
 * Overlay track rendered in the export and was INVISIBLE in the preview. The
 * approximation badge was blind to it too (`countVisualItemsAt` carried a
 * private copy of the same wrong rule), so nothing told the user either.
 */

import { describe, expect, it } from "vitest";

import type { MediaItem } from "@/lib/bindings/MediaItem";
import type { Project } from "@/lib/bindings/Project";
import type { TimelineItem } from "@/lib/bindings/TimelineItem";
import type { Track } from "@/lib/bindings/Track";
import type { TrackKind } from "@/lib/bindings/TrackKind";

import { activeVideoItem, visualItemsAt } from "./previewMap";
import { describeScene } from "./compositor/scene";

const PLAYHEAD = 1000;

function track(
  id: string,
  index: number,
  kind: TrackKind = "video",
  enabled = true,
): Track {
  return {
    id,
    kind,
    name: id,
    index,
    enabled,
    locked: false,
    muted: false,
    solo: false,
    volume_db: 0,
  };
}

function media(id: string, kind: MediaItem["kind"] = "video"): MediaItem {
  return {
    id,
    path: `/${id}.mp4`,
    content_hash: id,
    kind,
    duration_ms: 60_000,
    width: 1920,
    height: 1080,
    fps: 30,
    has_audio: true,
    audio_wav_path: null,
    original_filename: `${id}.mp4`,
    added_at: 0,
  };
}

function item(
  id: string,
  trackId: string,
  mediaId: string | null,
  startMs = 0,
  enabled = true,
): TimelineItem {
  return {
    id,
    track_id: trackId,
    kind: "av",
    source_media_id: mediaId,
    in_ms: 0,
    out_ms: 4000,
    timeline_start_ms: startMs,
    speed: 1,
    gain_db: 0,
    fade_in_ms: 0,
    fade_out_ms: 0,
    transform: {
      x: 0,
      y: 0,
      scale: 1,
      rotation_deg: 0,
      opacity: 1,
      crop: null,
    },
    effects: [],
    transition_in: null,
    text: null,
    enabled,
    locked: false,
  };
}

function project(
  tracks: Track[],
  items: TimelineItem[],
  medias: MediaItem[],
): Project {
  return {
    video_width: 1920,
    video_height: 1080,
    media: medias,
    tracks,
    timeline_items: items,
  } as unknown as Project;
}

/**
 * One row per clause of the Rust filter. `expected` is the visual stack the
 * export produces at {@link PLAYHEAD}, BOTTOM → TOP.
 */
const CASES: Array<{
  clause: string;
  name: string;
  project: Project;
  expected: string[];
}> = [
  {
    clause: "is_visual — media kind only; the TRACK's kind is never consulted",
    name: "a video clip on an Overlay track is composited",
    project: project(
      [track("ov", 0, "overlay")],
      [item("clip", "ov", "m")],
      [media("m")],
    ),
    expected: ["clip"],
  },
  {
    clause: "is_visual — media kind only; the TRACK's kind is never consulted",
    name: "a video clip on a Caption track is composited",
    project: project(
      [track("cap", 0, "caption")],
      [item("clip", "cap", "m")],
      [media("m")],
    ),
    expected: ["clip"],
  },
  {
    clause: "is_visual — media kind only; the TRACK's kind is never consulted",
    name: "a video clip on an Audio track is composited",
    project: project(
      [track("a", 0, "audio")],
      [item("clip", "a", "m")],
      [media("m")],
    ),
    expected: ["clip"],
  },
  {
    clause: "is_visual — audio-only media is not visual, even on a Video track",
    name: "an mp3 clip on a Video track is NOT composited",
    project: project(
      [track("v", 0)],
      [item("clip", "v", "m")],
      [media("m", "audio_only")],
    ),
    expected: [],
  },
  {
    clause: "is_visual — an unresolvable source_media_id is not visual",
    name: "a clip whose media is missing is NOT composited",
    project: project(
      [track("v", 0)],
      [item("clip", "v", "gone")],
      [media("m")],
    ),
    expected: [],
  },
  {
    clause: "it.enabled",
    name: "a disabled item is NOT composited",
    project: project(
      [track("v", 0)],
      [item("clip", "v", "m", 0, false)],
      [media("m")],
    ),
    expected: [],
  },
  {
    clause: "track_visible — track.enabled, for ANY track kind",
    name: "a disabled Overlay track hides its video clip",
    project: project(
      [track("ov", 0, "overlay", false)],
      [item("clip", "ov", "m")],
      [media("m")],
    ),
    expected: [],
  },
  {
    clause: "track_visible — is_none_or: a missing track counts as visible",
    name: "an item whose track was deleted is still composited",
    project: project([], [item("orphan", "gone", "m")], [media("m")]),
    expected: ["orphan"],
  },
  {
    clause: "track_index — unwrap_or(i32::MAX): a missing track sorts on TOP",
    name: "an orphaned item composites above every real track",
    project: project(
      [track("v", 0), track("v2", 99)],
      [
        item("low", "v", "m1"),
        item("high", "v2", "m2"),
        item("orphan", "x", "m3"),
      ],
      [media("m1"), media("m2"), media("m3")],
    ),
    expected: ["low", "high", "orphan"],
  },
  {
    clause: "sort by track_index — LOW index composites first (bottom)",
    name: "a video track and an overlay track stack by index, not by kind",
    project: project(
      // The overlay track sits BELOW the video track here: the export sorts on
      // `index` alone, so a preview that assumed "overlay is always on top"
      // (or "overlay never draws") would show the wrong frame.
      [track("ov", 0, "overlay"), track("v", 1)],
      [item("under", "ov", "m1"), item("over", "v", "m2")],
      [media("m1"), media("m2")],
    ),
    expected: ["under", "over"],
  },
  {
    clause: "sort tie-break — .then(timeline_start_ms) on the same track",
    name: "two clips on one track under the playhead order by start",
    project: project(
      [track("v", 0)],
      // Deliberately out of timeline order in the array, and overlapping —
      // `add_timeline_item` prevents that on Video lanes, but a hand-edited or
      // migrated project file can still contain it.
      [item("later", "v", "m2", 500), item("earlier", "v", "m1", 0)],
      [media("m1"), media("m2")],
    ),
    expected: ["earlier", "later"],
  },
];

describe("visualItemsAt mirrors compose.rs's visual stack", () => {
  for (const c of CASES) {
    it(`${c.name} [${c.clause}]`, () => {
      expect(visualItemsAt(c.project, PLAYHEAD).map((v) => v.item.id)).toEqual(
        c.expected,
      );
    });
  }
});

/**
 * Every preview surface must agree with that one list. These would have failed
 * before the fix: `activeVideoItem` and `scene.countVisualItemsAt` each held
 * their own copy of the rule.
 */
describe("every preview surface derives from the same stack", () => {
  for (const c of CASES) {
    it(`<video> element shows the TOP of the stack — ${c.name}`, () => {
      const top = c.expected[c.expected.length - 1] ?? null;
      expect(activeVideoItem(c.project, PLAYHEAD)?.item.id ?? null).toBe(top);
    });

    it(`compositor draws the TOP of the stack — ${c.name}`, () => {
      const scene = describeScene(c.project, PLAYHEAD);
      const top = c.expected[c.expected.length - 1] ?? null;
      expect(scene.layers.map((l) => l.itemId)).toEqual(top ? [top] : []);
    });

    it(`approximation badge fires iff the export stacks — ${c.name}`, () => {
      const scene = describeScene(c.project, PLAYHEAD);
      expect(scene.unsupported.includes("stacked-layers")).toBe(
        c.expected.length > 1,
      );
    });
  }

  // The concrete regression, stated once in plain terms: an Overlay track
  // carrying a second video over the base is a real composite the single
  // <video> preview cannot show. The badge must say so.
  it("flags a base video + an overlay-track video as an approximated stack", () => {
    const p = project(
      [track("v", 0), track("ov", 1, "overlay")],
      [item("base", "v", "m1"), item("pip", "ov", "m2")],
      [media("m1"), media("m2")],
    );
    const scene = describeScene(p, PLAYHEAD);
    expect(scene.unsupported).toContain("stacked-layers");
    expect(scene.layers.map((l) => l.itemId)).toEqual(["pip"]);
  });
});
