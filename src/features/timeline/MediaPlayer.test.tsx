import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, cleanup, act } from "@testing-library/react";

import type { MediaItem } from "@/lib/bindings/MediaItem";
import type { Project } from "@/lib/bindings/Project";
import type { TimelineItem } from "@/lib/bindings/TimelineItem";
import type { Track } from "@/lib/bindings/Track";

import { MediaPlayer } from "./MediaPlayer";

// The NLE branch runs clip media through convertFileSrc; there is no Tauri
// runtime under Vitest, so mock the lowest layer.
vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (p: string) => `asset://localhost/${p}`,
}));

// jsdom implements <video> as an element but not playback: play()/pause() are
// stubbed to throw "Not implemented", and currentTime/duration/paused are not
// wired to a media clock. We mock the bits MediaPlayer reconciles against so we
// can observe the calls it makes (offline — no real video is loaded).
let mockState: { currentTime: number; paused: boolean; duration: number };
let playSpy: ReturnType<typeof vi.spyOn>;
let pauseSpy: ReturnType<typeof vi.spyOn>;

function installVideoMock() {
  mockState = { currentTime: 0, paused: true, duration: 60 };
  const proto = window.HTMLMediaElement.prototype;
  playSpy = vi.spyOn(proto, "play").mockImplementation(() => {
    mockState.paused = false;
    return Promise.resolve();
  });
  pauseSpy = vi.spyOn(proto, "pause").mockImplementation(() => {
    mockState.paused = true;
  });
  vi.spyOn(proto, "currentTime", "get").mockImplementation(
    () => mockState.currentTime,
  );
  vi.spyOn(proto, "currentTime", "set").mockImplementation((v: number) => {
    mockState.currentTime = v;
  });
  vi.spyOn(proto, "paused", "get").mockImplementation(() => mockState.paused);
  vi.spyOn(proto, "duration", "get").mockImplementation(
    () => mockState.duration,
  );
}

// Drive the requestAnimationFrame loop deterministically.
let rafCb: FrameRequestCallback | null = null;
function installRaf() {
  rafCb = null;
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb) => {
    rafCb = cb;
    return 1;
  });
  vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
}
function pumpFrame() {
  act(() => {
    rafCb?.(performance.now());
  });
}

beforeEach(() => {
  installVideoMock();
  installRaf();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("MediaPlayer", () => {
  it("seeks the element to the playhead while paused (rate 0)", () => {
    render(
      <MediaPlayer
        src="asset://x.mp4"
        playheadMs={2000}
        rate={0}
        durationMs={60_000}
        fps={30}
      />,
    );
    pumpFrame();
    expect(mockState.currentTime).toBeCloseTo(2.0);
    expect(playSpy).not.toHaveBeenCalled();
  });

  it("plays the element when the timeline plays at realtime", () => {
    mockState.currentTime = 2.0;
    render(
      <MediaPlayer
        src="asset://x.mp4"
        playheadMs={2000}
        rate={1}
        durationMs={60_000}
        fps={30}
      />,
    );
    pumpFrame();
    expect(playSpy).toHaveBeenCalled();
  });

  it("keeps playing to the timeline end when the element duration is shorter than the project metadata", () => {
    // Probe metadata gives durationMs=60_000, but the real container is 59.5s.
    // The timeline clock (authority) keeps advancing to 60s, so the element
    // must keep playing rather than pausing early at its own 59.5s end — else
    // the playhead desyncs from the frozen video near the clip end.
    mockState.duration = 59.5;
    mockState.currentTime = 59.4;
    mockState.paused = true;
    render(
      <MediaPlayer
        src="asset://x.mp4"
        playheadMs={59_600}
        rate={1}
        durationMs={60_000}
        fps={30}
      />,
    );
    pumpFrame();
    expect(playSpy).toHaveBeenCalled();
    expect(pauseSpy).not.toHaveBeenCalled();
  });

  it("pauses the element for reverse/shuttle (scrubs by seeking instead)", () => {
    mockState.paused = false;
    render(
      <MediaPlayer
        src="asset://x.mp4"
        playheadMs={3000}
        rate={-2}
        durationMs={60_000}
        fps={30}
      />,
    );
    pumpFrame();
    expect(pauseSpy).toHaveBeenCalled();
    expect(mockState.currentTime).toBeCloseTo(3.0);
  });

  it("follows a moving playhead across re-renders", () => {
    const { rerender } = render(
      <MediaPlayer
        src="asset://x.mp4"
        playheadMs={1000}
        rate={0}
        durationMs={60_000}
        fps={30}
      />,
    );
    pumpFrame();
    expect(mockState.currentTime).toBeCloseTo(1.0);
    rerender(
      <MediaPlayer
        src="asset://x.mp4"
        playheadMs={5000}
        rate={0}
        durationMs={60_000}
        fps={30}
      />,
    );
    pumpFrame();
    expect(mockState.currentTime).toBeCloseTo(5.0);
  });

  it("warns on a manual scrub gesture during playback", () => {
    const onConflict = vi.fn();
    const { container } = render(
      <MediaPlayer
        src="asset://x.mp4"
        playheadMs={1000}
        rate={1}
        durationMs={60_000}
        fps={30}
        onConflict={onConflict}
      />,
    );
    const video = container.querySelector("video")!;
    // No programmatic mutation just happened, and we're playing → user gesture.
    act(() => {
      video.dispatchEvent(new Event("seeking"));
    });
    expect(onConflict).toHaveBeenCalled();
  });

  it("does not warn on a seek while the timeline is stopped", () => {
    const onConflict = vi.fn();
    const { container } = render(
      <MediaPlayer
        src="asset://x.mp4"
        playheadMs={1000}
        rate={0}
        durationMs={60_000}
        fps={30}
        onConflict={onConflict}
      />,
    );
    const video = container.querySelector("video")!;
    act(() => {
      video.dispatchEvent(new Event("seeking"));
    });
    expect(onConflict).not.toHaveBeenCalled();
  });

  it("shows the unavailable overlay when the video errors", () => {
    const { container, queryByTestId } = render(
      <MediaPlayer
        src="asset://missing.mp4"
        playheadMs={0}
        rate={0}
        durationMs={60_000}
        fps={30}
      />,
    );
    expect(queryByTestId("media-unavailable")).toBeNull();
    const video = container.querySelector("video")!;
    act(() => {
      video.dispatchEvent(new Event("error"));
    });
    expect(queryByTestId("media-unavailable")).not.toBeNull();
  });
});

// ── Minimal NLE project factories (mirror previewMap.test.ts) ───────────────

function track(id: string, index: number): Track {
  return {
    id,
    kind: "video",
    name: id,
    index,
    enabled: true,
    locked: false,
    muted: false,
    solo: false,
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
  mediaId: string,
  startMs: number,
  inMs: number,
  outMs: number,
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
  };
}

function nleProject(
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

// Regression (state-mediaplayer-unavailable-never-reset /
// diff-unavailable-overlay-latched): the "media missing" overlay used to latch
// forever — onError set `unavailable` and nothing ever cleared it, so a single
// broken clip left a permanent banner over every subsequently playing source.
// Any source change (rAF clip swap, proxy load, legacy src change) must reset.
describe("MediaPlayer — unavailable overlay resets on source change", () => {
  it("clears the overlay when the NLE rAF loop swaps video.src to a healthy clip's media", () => {
    // Clip A (broken media) spans [0, 2000); clip B (fine) spans [2000, 4000).
    const p = nleProject(
      [track("t", 0)],
      [item("iA", "t", "mA", 0, 0, 2000), item("iB", "t", "mB", 2000, 0, 2000)],
      [media("mA", "/missing/deleted.mp4"), media("mB", "/ok/fine.mp4")],
    );

    const { container, queryByTestId, rerender } = render(
      <MediaPlayer
        project={p}
        playheadMs={1000}
        rate={1}
        durationMs={4000}
        fps={30}
      />,
    );
    const video = container.querySelector("video")!;

    // Frame under clip A → rAF loop loads clip A's media into the element.
    pumpFrame();
    expect(video.src).toContain("deleted.mp4");

    // Clip A's file is gone → the element errors → overlay appears (correct).
    act(() => {
      video.dispatchEvent(new Event("error"));
    });
    expect(queryByTestId("media-unavailable")).not.toBeNull();

    // Playhead reaches clip B → the rAF loop swaps the element to clip B's
    // media, which loads and plays fine. The stale overlay must not stay.
    rerender(
      <MediaPlayer
        project={p}
        playheadMs={3000}
        rate={1}
        durationMs={4000}
        fps={30}
      />,
    );
    pumpFrame();
    expect(video.src).toContain("fine.mp4");
    expect(queryByTestId("media-unavailable")).toBeNull();
  });

  it("clears the overlay when a freshly rendered preview proxy is loaded", () => {
    const p = nleProject(
      [track("t", 0)],
      [item("iA", "t", "mA", 0, 0, 2000)],
      [media("mA", "/gone/deleted.mp4")],
    );

    const { container, queryByTestId, rerender } = render(
      <MediaPlayer
        project={p}
        playheadMs={500}
        rate={0}
        durationMs={2000}
        fps={30}
      />,
    );
    const video = container.querySelector("video")!;
    pumpFrame();
    act(() => {
      video.dispatchEvent(new Event("error"));
    });
    expect(queryByTestId("media-unavailable")).not.toBeNull();

    // A preview proxy finishes rendering → the element is handed a brand-new
    // composited file. It must not play under a permanent "missing" banner.
    rerender(
      <MediaPlayer
        project={p}
        proxySrc="asset://localhost/proxy-composite.mp4"
        playheadMs={500}
        rate={0}
        durationMs={2000}
        fps={30}
      />,
    );
    pumpFrame();
    expect(queryByTestId("media-unavailable")).toBeNull();
  });

  it("clears the overlay when the legacy src prop changes to a different file", () => {
    const { container, queryByTestId, rerender } = render(
      <MediaPlayer
        src="asset://localhost/missing.mp4"
        playheadMs={0}
        rate={0}
        durationMs={60_000}
        fps={30}
      />,
    );
    const video = container.querySelector("video")!;
    act(() => {
      video.dispatchEvent(new Event("error"));
    });
    expect(queryByTestId("media-unavailable")).not.toBeNull();

    // The app points the player at a different, valid file.
    rerender(
      <MediaPlayer
        src="asset://localhost/present.mp4"
        playheadMs={0}
        rate={0}
        durationMs={60_000}
        fps={30}
      />,
    );
    pumpFrame();

    // New source → the old error no longer describes the element's state.
    expect(queryByTestId("media-unavailable")).toBeNull();
  });
});
