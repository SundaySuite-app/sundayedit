import { describe, it, expect } from "vitest";

import type { MediaItem } from "@/lib/bindings/MediaItem";
import type { Project } from "@/lib/bindings/Project";
import type { TimelineItem } from "@/lib/bindings/TimelineItem";
import type { Track } from "@/lib/bindings/Track";

import { activeVideoItem, sourceTimeSec, timelineEndMs } from "./previewMap";

// ── Minimal factories (mirror the Rust model tests' `item(...)` helper) ──────

function track(id: string, index: number, extra?: Partial<Track>): Track {
  return {
    id,
    kind: "video",
    name: id,
    index,
    enabled: true,
    locked: false,
    muted: false,
    solo: false,
    ...extra,
  };
}

function media(id: string, path: string): MediaItem {
  return {
    id,
    path,
    content_hash: id,
    kind: "video",
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
  startMs: number,
  inMs: number,
  outMs: number,
  extra?: Partial<TimelineItem>,
): TimelineItem {
  return {
    id,
    track_id: trackId,
    kind: "av",
    source_media_id: mediaId,
    in_ms: inMs,
    out_ms: outMs,
    timeline_start_ms: startMs,
    speed: 1,
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
    enabled: true,
    locked: false,
    ...extra,
  };
}

function project(
  tracks: Track[],
  items: TimelineItem[],
  medias: MediaItem[],
): Project {
  return {
    media: medias,
    tracks,
    timeline_items: items,
  } as unknown as Project;
}

// ── timelineEndMs ───────────────────────────────────────────────────────────

describe("timelineEndMs", () => {
  it("adds the source span at 1× speed", () => {
    expect(timelineEndMs(item("i", "t", "m", 1000, 0, 2000))).toBe(3000);
  });

  it("compresses the span at 2× speed", () => {
    expect(
      timelineEndMs(item("i", "t", "m", 1000, 0, 2000, { speed: 2 })),
    ).toBe(2000);
  });

  it("never divides by zero on a zero speed", () => {
    // speed floored at 0.01 → span/0.01 = 200_000
    expect(timelineEndMs(item("i", "t", "m", 0, 0, 2000, { speed: 0 }))).toBe(
      200_000,
    );
  });
});

// ── activeVideoItem ─────────────────────────────────────────────────────────

describe("activeVideoItem", () => {
  it("returns null when there are no timeline items", () => {
    const p = project([track("t", 0)], [], [media("m", "/m.mp4")]);
    expect(activeVideoItem(p, 100)).toBeNull();
  });

  it("resolves a single item under the playhead", () => {
    const p = project(
      [track("t", 0)],
      [item("i", "t", "m", 0, 0, 4000)],
      [media("m", "/m.mp4")],
    );
    const hit = activeVideoItem(p, 1000);
    expect(hit?.item.id).toBe("i");
    expect(hit?.media.path).toBe("/m.mp4");
  });

  it("misses when the playhead is outside the item span", () => {
    const p = project(
      [track("t", 0)],
      [item("i", "t", "m", 1000, 0, 2000)], // spans [1000, 3000)
      [media("m", "/m.mp4")],
    );
    expect(activeVideoItem(p, 500)).toBeNull(); // before
    expect(activeVideoItem(p, 3000)).toBeNull(); // end is exclusive
    expect(activeVideoItem(p, 2999)?.item.id).toBe("i"); // just inside
  });

  it("picks the top-most track when two video clips overlap", () => {
    const p = project(
      [track("bottom", 0), track("top", 1)],
      [
        item("lo", "bottom", "m1", 0, 0, 4000),
        item("hi", "top", "m2", 0, 0, 4000),
      ],
      [media("m1", "/lo.mp4"), media("m2", "/hi.mp4")],
    );
    const hit = activeVideoItem(p, 1000);
    expect(hit?.item.id).toBe("hi");
    expect(hit?.media.path).toBe("/hi.mp4");
  });

  it("skips a disabled track and falls through to the one below", () => {
    const p = project(
      [track("bottom", 0), track("top", 1, { enabled: false })],
      [
        item("lo", "bottom", "m1", 0, 0, 4000),
        item("hi", "top", "m2", 0, 0, 4000),
      ],
      [media("m1", "/lo.mp4"), media("m2", "/hi.mp4")],
    );
    expect(activeVideoItem(p, 1000)?.item.id).toBe("lo");
  });

  it("skips a disabled item", () => {
    const p = project(
      [track("t", 0)],
      [item("i", "t", "m", 0, 0, 4000, { enabled: false })],
      [media("m", "/m.mp4")],
    );
    expect(activeVideoItem(p, 1000)).toBeNull();
  });

  // Regression (diff-preview-hides-overlay-track-video): this used to assert
  // `toBeNull()` — the preview required `track.kind === "video"`. `compose.rs`
  // does NOT: `is_visual` looks only at the MEDIA's kind, and `track_visible`
  // only at the track's `enabled`. So a video clip on an Overlay track (the
  // media bin offers "add overlay track", the lane drop handler allows it) was
  // rendered by the export and invisible in the preview — a preview that hides
  // what the export WILL draw. It must be selected.
  it("selects a video clip on a non-video track (export parity: is_visual ignores track kind)", () => {
    const p = project(
      [track("ov", 0, { kind: "overlay" })],
      [item("i", "ov", "m", 0, 0, 4000)],
      [media("m", "/m.mp4")],
    );
    expect(activeVideoItem(p, 1000)?.item.id).toBe("i");
  });

  it("still honours a DISABLED non-video track (track_visible)", () => {
    const p = project(
      [track("ov", 0, { kind: "overlay", enabled: false })],
      [item("i", "ov", "m", 0, 0, 4000)],
      [media("m", "/m.mp4")],
    );
    expect(activeVideoItem(p, 1000)).toBeNull();
  });

  // `track_index` falls back to `i32::MAX` for an item whose track cannot be
  // resolved, and `track_visible` treats it as visible — so such an item
  // composites ON TOP of everything. The preview must show the same frame.
  it("puts an item with an unresolvable track on top (track_index = i32::MAX)", () => {
    const p = project(
      [track("v", 0)],
      [
        item("onTrack", "v", "m1", 0, 0, 4000),
        item("orphan", "gone", "m2", 0, 0, 4000),
      ],
      [media("m1", "/lo.mp4"), media("m2", "/orphan.mp4")],
    );
    expect(activeVideoItem(p, 1000)?.item.id).toBe("orphan");
  });

  it("returns null when the source media can't be resolved", () => {
    const p = project(
      [track("t", 0)],
      [item("i", "t", "missing", 0, 0, 4000)],
      [media("m", "/m.mp4")],
    );
    expect(activeVideoItem(p, 1000)).toBeNull();
  });

  // Regression (diff-audio-media-on-video-track): an audio-only MediaItem can
  // end up on a Video track (older projects; ops never compared media kind to
  // track kind). Export's is_visual() requires MediaKind::Video, so the
  // preview must skip it too — otherwise an mp3 clip on an upper video track
  // occludes real footage the export would actually render.
  it("never selects audio_only media as the active video clip (export parity)", () => {
    const p = project(
      [track("v_low", 0), track("v_high", 1)],
      [
        item("clip_video", "v_low", "m_video", 0, 0, 10_000),
        item("clip_audio", "v_high", "m_audio", 0, 0, 10_000),
      ],
      [
        media("m_video", "/footage.mp4"),
        {
          ...media("m_audio", "/song.mp3"),
          kind: "audio_only",
          width: 0,
          height: 0,
          fps: 0,
          original_filename: "song.mp3",
        },
      ],
    );
    const hit = activeVideoItem(p, 1000);
    expect(hit?.media.kind).not.toBe("audio_only");
    expect(hit?.item.id).toBe("clip_video");
    expect(hit?.media.path).toBe("/footage.mp4");
  });

  it("falls through to a lower video clip when the top clip's media is unresolvable", () => {
    const p = project(
      [track("v_low", 0), track("v_high", 1)],
      [
        item("lo", "v_low", "m1", 0, 0, 4000),
        item("hi", "v_high", "missing", 0, 0, 4000),
      ],
      [media("m1", "/lo.mp4")],
    );
    expect(activeVideoItem(p, 1000)?.item.id).toBe("lo");
  });
});

// ── backfilled fresh-import shape ───────────────────────────────────────────
// Mirrors what the Rust `Project::backfill_default_timeline` synthesizes for a
// freshly imported video (project_create_from_video) or a migrated v<=3 file:
// one media item built from the video_* scalars, a Video track at index 0 (plus
// a Caption track), and ONE full-length Av clip at timeline 0, speed 1.

describe("backfilled fresh-import shape", () => {
  const DURATION = 60_000; // media() factory duration
  const backfilled = (): Project =>
    project(
      [
        track("track-video", 0),
        track("track-captions", 1, { kind: "caption" }),
      ],
      [item("item-full", "track-video", "media-primary", 0, 0, DURATION)],
      [media("media-primary", "/videos/test.mp4")],
    );

  it("maps every playhead in [0, duration) to the single placed clip", () => {
    const p = backfilled();
    for (const t of [0, 1, 29_999, DURATION - 1]) {
      const hit = activeVideoItem(p, t);
      expect(hit?.item.id).toBe("item-full");
      expect(hit?.media.path).toBe("/videos/test.mp4");
    }
    expect(activeVideoItem(p, DURATION)).toBeNull(); // end is exclusive
  });

  it("source time equals the playhead — equivalent to the legacy path", () => {
    const p = backfilled();
    for (const t of [0, 1000, 12_345, DURATION - 1]) {
      const hit = activeVideoItem(p, t)!;
      // in_ms 0 + (t - 0) * 1.0 → t/1000: mapping mode shows the exact same
      // frame the legacy single-src <video> would.
      expect(sourceTimeSec(hit.item, t)).toBeCloseTo(t / 1000);
    }
  });
});

// ── sourceTimeSec ───────────────────────────────────────────────────────────

describe("sourceTimeSec", () => {
  it("equals in_ms at the item's timeline start", () => {
    const it = item("i", "t", "m", 1000, 500, 2500);
    expect(sourceTimeSec(it, 1000)).toBeCloseTo(0.5); // in_ms 500 → 0.5s
  });

  it("advances at realtime for 1× speed", () => {
    const it = item("i", "t", "m", 1000, 0, 4000);
    // 1s into the clip on the timeline → 1s into the source.
    expect(sourceTimeSec(it, 2000)).toBeCloseTo(1);
  });

  it("advances at 2× the source rate when sped up", () => {
    const it = item("i", "t", "m", 1000, 0, 4000, { speed: 2 });
    // 1s of timeline at 2× → 2s of source (offset by in_ms=0).
    expect(sourceTimeSec(it, 2000)).toBeCloseTo(2);
  });
});
