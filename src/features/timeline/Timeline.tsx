/**
 * NLE timeline — the multi-lane spatial canvas of the editor.
 *
 * A fixed-size viewport renders the visible time window (pan = scrollMs, zoom =
 * pxPerMs) rather than one enormous scrolled element, so a 90-minute project
 * stays smooth. A left gutter carries one header per track (name + mute/solo/
 * lock + reorder); the viewport stacks one lane per track. Caption tracks keep
 * rendering the flagship captions (virtualized, drag to move / edge-drag to
 * retime — unchanged). Video/Audio/Overlay tracks render their placed clips as
 * boxes; drag moves a clip along time and across tracks, edge-drag trims it, and
 * both commit through the pure backend ops on the shared undo stack.
 *
 * Transport is J/K/L shuttle (reverse/stop/forward, doubling on repeat) plus
 * Space, driven by a `PlaybackClock` (E2): the playhead is READ from a
 * monotonic audio/wall clock rather than accumulated from rAF deltas, so a
 * throttled background tab, a decode stall or a long session can no longer
 * make it drift away from the media. rAF only decides when we look. Drags snap to
 * neighbouring edges, the playhead and the bounds (S toggles snapping). B
 * blades the selected clip at the playhead; Delete/Backspace ripple-deletes
 * it. Media dragged from the bin drops onto a lane to become a new clip. ⌘D
 * duplicates the selected clip right after itself on the same track; ⌘C/⌘V
 * copy it and re-place a duplicate at the playhead, on the track the copied
 * clip is on now (or its own track if nothing else is selected).
 */

import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { appCacheDir, join } from "@tauri-apps/api/path";
import { convertFileSrc, isTauri } from "@tauri-apps/api/core";
import {
  ZoomIn,
  ZoomOut,
  Play,
  Pause,
  Magnet,
  Volume2,
  VolumeX,
  Eye,
  EyeOff,
  Lock,
  Unlock,
  Headphones,
  ChevronUp,
  ChevronDown,
  Clapperboard,
  Loader2,
  RotateCcw,
  FoldHorizontal,
  AlertTriangle,
  Type,
  X,
} from "lucide-react";

import type {
  Caption,
  MediaItem,
  Project,
  TimelineItem,
  Track,
  WaveformData,
} from "@/lib/bindings";
import { confidenceTier } from "@/lib/bindings";
import { ipc } from "@/lib/ipc";
import { useProjectStore } from "@/lib/useProjectStore";
import { useT } from "@/lib/i18n";
import { cn } from "@/lib/cn";
import * as tl from "./geometry";
import { clipDragToOp } from "./clipDrag";
import {
  itemSpan,
  stackedTracks,
  trackAtYSticky,
  timelineDurationMs,
} from "./laneLayout";
import { MediaPlayer } from "./MediaPlayer";
import { publishPlayheadMs } from "./playhead";
import { PlaybackClock, type PlaybackSnapshot } from "./playbackClock";
import { qualityFor, renderStride, shouldRenderFrame } from "./previewQuality";
import { renderPreviewProxy } from "@/lib/composeEngine";
import {
  duplicateTimelineItem,
  newestTimelineItemId,
} from "./timelineOpsExtra";
import { MEDIA_DND_MIME } from "@/features/media/MediaBin";
import { useThumbnail } from "@/features/media/thumbnails";
import { useMediaAvailability } from "@/features/media/useMediaAvailability";
import { useFilmstripTiles } from "./filmstrip";
import {
  GAIN_DB_MIN,
  GAIN_DB_MAX,
  applyGainDetent,
  formatDb,
} from "./audioLevels";
import { useCoalescedCommit } from "./useCoalescedCommit";

interface Props {
  project: Project;
  /** Asset URL for the source video, or undefined when none is attachable
   *  (browser/demo, no Tauri asset protocol). Legacy single-video fallback when
   *  the project has no placed clips; multi-track preview reads `project`. */
  videoSrc?: string;
  /** Notified with the selected clip's id (or null when cleared), so the host
   *  can show the clip inspector. Selection highlight stays local either way. */
  onSelectClip?: (itemId: string | null) => void;
}

const RULER_H = 22;
const WAVE_H = 72;
const LANE_H = 52;
// Widened from 150 (E3-UI): the "close gaps" button adds a 7th icon to an
// audible track's header row (chevrons + mute + solo + lock + pack + remove),
// and 150px squeezed the name label to zero width — invisible per Playwright,
// truncated-to-nothing for real users. Widened again from 184 (R2 audio): the
// compact track-fader slider is an 8th control on that same row.
const GUTTER_W = 276;

/** Default on-timeline length of a text overlay added from the toolbar. */
const DEFAULT_TEXT_MS = 3000;

/** Caption move/resize drag (flagship captions on a caption track). */
type CaptionDrag = {
  kind: "move" | "resize-start" | "resize-end";
  id: string;
  startClientX: number;
  origStart: number;
  origEnd: number;
  deltaMs: number;
};

/** Clip move/trim drag (timeline items on video/audio/overlay tracks). */
type ClipDrag = {
  kind: "move" | "resize-start" | "resize-end";
  id: string;
  trackId: string;
  trackKind: Track["kind"];
  speed: number;
  startClientX: number;
  origStart: number;
  origInMs: number;
  origOutMs: number;
  deltaMs: number;
  /** Cross-track target (move only); equals `trackId` until the pointer moves
   *  over a compatible lane. */
  targetTrackId: string;
};

/** Worst (highest) confidence tier across a caption's words → box tint. */
function worstTier(c: Caption): number {
  let worst = 1;
  for (const w of c.words) {
    const t = confidenceTier(w);
    if (t > worst) worst = t;
  }
  return worst;
}

const TIER_BORDER: Record<number, string> = {
  1: "var(--color-success)",
  2: "var(--color-warning)",
  3: "var(--color-danger)",
  4: "var(--color-danger)",
};

export function Timeline({ project, videoSrc, onSelectClip }: Props) {
  const t = useT();
  // Every timeline edit commits through the SAME shared undo stack as caption
  // edits, so moves/trims/flags are undoable and never diverge from the editor.
  const run = useProjectStore((s) => s.run);
  const durationMs = Math.max(1, timelineDurationMs(project));
  const fps = project.video_fps > 0 ? project.video_fps : 30;

  const captions = useMemo(
    () => [...project.captions].sort((a, b) => a.start_ms - b.start_ms),
    [project.captions],
  );

  // Tracks in stacking order (top lane first) + a media lookup for clip labels.
  const stacked = useMemo(
    () => stackedTracks(project.tracks),
    [project.tracks],
  );
  const mediaById = useMemo(() => {
    const m = new Map<string, MediaItem>();
    for (const it of project.media) m.set(it.id, it);
    return m;
  }, [project.media]);
  // Media ids whose file is missing on disk — a stable Set reference (see the
  // hook doc) that LaneStack/ClipBox read straight off, so a clip box's
  // missing state costs one `Set.has` per box instead of a per-clip lookup
  // that would break the lane's memoization every render.
  const { missingIds: missingMediaIds } = useMediaAvailability(project);
  // Clips grouped by track, each start-sorted and carrying its timeline span.
  const clipsByTrack = useMemo(() => {
    const by = new Map<
      string,
      { start_ms: number; end_ms: number; ti: TimelineItem }[]
    >();
    for (const ti of project.timeline_items) {
      const arr = by.get(ti.track_id) ?? [];
      arr.push({ ...itemSpan(ti), ti });
      by.set(ti.track_id, arr);
    }
    for (const arr of by.values()) arr.sort((a, b) => a.start_ms - b.start_ms);
    return by;
  }, [project.timeline_items]);

  const [view, setView] = useState<tl.TimelineView>({
    pxPerMs: 0.05,
    scrollMs: 0,
    widthPx: 800,
  });
  // Both mirror the playback clock (the authority) into React state. Handlers
  // read the refs, which the clock's subscriber updates synchronously — React
  // state lags a render behind and a shuttle tap must never see a stale rate.
  const [playheadMs, setPlayheadMs] = useState(0);
  const playheadRef = useRef(0);
  // Signed playback rate (J/K/L shuttle): <0 reverse, 0 stopped, 1 realtime.
  const [rate, setRate] = useState(0);
  const rateRef = useRef(0);
  const playing = rate !== 0;
  const [snapEnabled, setSnapEnabled] = useState(true);
  // A pointer is down on the time surface (ruler / waveform / lane) — the
  // preview quality ladder's "interaction" rung, alongside the two drags.
  const [scrubbing, setScrubbing] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [selectedClipId, setSelectedClipId] = useState<string | null>(null);
  const [drag, setDrag] = useState<CaptionDrag | null>(null);
  const [clipDrag, setClipDrag] = useState<ClipDrag | null>(null);
  const [dropTrackId, setDropTrackId] = useState<string | null>(null);
  // ⌘C clipboard: just enough to find the source clip again at paste time —
  // NOT a frozen snapshot of its fields. ⌘V re-duplicates whatever that id
  // currently looks like (see `pasteClipboard`), so a paste after further
  // edits to the copied clip reflects those edits, same as most NLEs'
  // "duplicate" but keyed off a remembered id instead of the selection.
  const clipboardRef = useRef<{
    id: string;
    trackId: string;
    trackKind: Track["kind"];
  } | null>(null);
  const [waveform, setWaveform] = useState<WaveformData | null>(null);
  // A transient warning when the user scrubs the native video control while the
  // timeline is driving playback (the two clocks would fight).
  const [scrubWarning, setScrubWarning] = useState(false);
  // A backend rejection worth showing (remove-track on a non-empty track…).
  // Drag/trim rejections stay silent — the ghost snapping back IS the feedback
  // — but button-triggered ops have no visual echo, so surface the message.
  const [opError, setOpError] = useState<string | null>(null);
  // Preview-render proxy: a flattened composite the MediaPlayer plays instead of
  // the live per-clip mapping, so the user can see the true composite. Rendered
  // on demand through the compose engine; cleared to return to the live preview.
  const [proxySrc, setProxySrc] = useState<string | undefined>(undefined);
  const [previewState, setPreviewState] = useState<
    "idle" | "rendering" | "done"
  >("idle");

  // Notify the host of the selected clip while keeping the local highlight ring
  // in sync — one call site so selection and the inspector never diverge.
  const selectClip = useCallback(
    (id: string | null) => {
      setSelectedClipId(id);
      onSelectClip?.(id);
    },
    [onSelectClip],
  );

  const viewportRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const lanesScrollRef = useRef<HTMLDivElement | null>(null);
  const headerScrollRef = useRef<HTMLDivElement | null>(null);

  // Keep the visible window within [0, duration].
  const clampScroll = useCallback(
    (scrollMs: number, pxPerMs: number, widthPx: number) => {
      const span = widthPx / pxPerMs;
      return Math.max(0, Math.min(scrollMs, Math.max(0, durationMs - span)));
    },
    [durationMs],
  );

  // Measure the viewport width (drives the windowed render).
  useLayoutEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      const w = el.clientWidth;
      setView((v) => ({
        ...v,
        widthPx: w,
        scrollMs: clampScroll(v.scrollMs, v.pxPerMs, w),
      }));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [clampScroll]);

  // Fetch the real waveform once (no-op outside Tauri / without ffmpeg).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const cacheDir = await appCacheDir();
        const data = await ipc.project.waveform(project.video_path, cacheDir);
        if (!cancelled) setWaveform(data);
      } catch {
        // Browser/demo or no audio yet — render without a waveform track.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [project.video_path]);

  // ── playback clock ─────────────────────────────────────────────────────────
  // The clock owns the playhead; this component only publishes what it reads.
  // Duration/fps are handed in through refs so the clock is created ONCE per
  // mount (recreating it on a project edit would restart playback).
  const clockRef = useRef<PlaybackClock | null>(null);
  const durationRef = useRef(durationMs);
  durationRef.current = durationMs;
  const fpsRef = useRef(fps);
  fpsRef.current = fps;
  // Counts published frames of the current shuttle run — the frame-skip index.
  const frameCountRef = useRef(0);

  /** Publish a clock snapshot into React state. Called on every clock frame. */
  const applySnapshot = useCallback((snap: PlaybackSnapshot) => {
    const isPlaying = snap.status === "playing";

    // `rate` is the TRANSPORT rate the rest of the UI reads: 0 whenever we are
    // not actually rolling. (The clock keeps its last rate through a pause so
    // J/L can resume it, and pauses itself at either bound — where the badge
    // and the play/pause button must both read "stopped".)
    const nextRate = isPlaying ? snap.rate : 0;
    if (nextRate !== rateRef.current) frameCountRef.current = 0;
    rateRef.current = nextRate;
    setRate(nextRate);

    // Parked, the clock has already snapped to a frame boundary; re-apply
    // geometry's snap so the value lands on the exact integer-ms grid every
    // other timeline path uses (`clientXToMs`, `step`, the drag commits). The
    // two agree on the frame and differ only in rounding — clamped, because
    // the nearest frame to the very end can sit past it.
    const ms = isPlaying
      ? snap.timeMs
      : Math.min(snap.durationMs, tl.snapToFrame(snap.timeMs, snap.fps));

    // Frame skipping above 1×: the playhead covers |rate| frames of content per
    // displayed frame, so publishing every one of them would pile |rate|× the
    // React/preview work into the same wall-clock second. Transport changes
    // reset the counter (see above), so a stop/seek always lands.
    const index = frameCountRef.current++;
    if (!shouldRenderFrame(index, renderStride(nextRate))) return;

    playheadRef.current = ms;
    setPlayheadMs(ms);
  }, []);

  useEffect(() => {
    const clock = new PlaybackClock({
      durationMs: durationRef.current,
      fps: fpsRef.current,
      // The playhead line and the timecode are 60 fps UI, so take every frame;
      // the shuttle thinning happens in `applySnapshot`, by rate, not by time.
      notifyIntervalMs: 0,
    });
    clockRef.current = clock;
    // A StrictMode remount disposes the first clock; restore where we were.
    if (playheadRef.current > 0) clock.seek(playheadRef.current);
    const unsubscribe = clock.subscribe(applySnapshot);
    return () => {
      unsubscribe();
      clock.dispose();
      clockRef.current = null;
    };
  }, [applySnapshot]);

  // Project edits move the end of the timeline and can change the frame rate;
  // push both into the clock instead of rebuilding it.
  useEffect(() => {
    clockRef.current?.setDurationMs(durationMs);
  }, [durationMs]);
  useEffect(() => {
    clockRef.current?.setFps(fps);
  }, [fps]);

  /** Park the playhead at `ms` (the clock clamps and frame-snaps it). */
  const seekTo = useCallback((ms: number) => {
    // A deliberate jump must be seen at once. It is the one transport change
    // that leaves the RATE alone, so reset the frame-skip counter by hand —
    // else a seek mid-shuttle could sit invisible for up to a stride.
    frameCountRef.current = 0;
    clockRef.current?.seek(ms);
  }, []);

  /** The live playhead — the clock's own reading, not the published state. */
  const playheadNow = useCallback(
    () => clockRef.current?.timeMs ?? playheadRef.current,
    [],
  );

  /** J/K/L: next shuttle rate, then roll (or stop) at it. */
  const shuttle = useCallback((key: "j" | "k" | "l") => {
    const clock = clockRef.current;
    if (!clock) return;
    const next = tl.shuttleRate(rateRef.current, key);
    // `setRate(0)` both pauses and zeroes, so the next J/L tap starts from a
    // stop exactly as the old `setRate(0)` state transition did.
    clock.setRate(next);
    if (next !== 0) clock.play();
  }, []);

  /** Space / the toolbar button: stop if rolling, else roll forward at 1×. */
  const togglePlay = useCallback(() => {
    const clock = clockRef.current;
    if (!clock) return;
    if (rateRef.current !== 0) {
      clock.setRate(0);
      return;
    }
    clock.setRate(1);
    clock.play();
  }, []);

  // ── preview quality ladder ─────────────────────────────────────────────────
  // One rung for the whole preview stack: the live surface and any flatten
  // asked for from this state read the same number. A render in flight is the
  // one moment the user is judging OUTPUT, so it pins the un-degraded rung.
  const quality = qualityFor({
    rate,
    interacting: scrubbing || drag !== null || clipDrag !== null,
    exporting: previewState === "rendering",
  });

  // Auto-dismiss the scrub-conflict warning a few seconds after it appears.
  useEffect(() => {
    if (!scrubWarning) return;
    const id = setTimeout(() => setScrubWarning(false), 4000);
    return () => clearTimeout(id);
  }, [scrubWarning]);

  // Auto-dismiss surfaced op rejections (same pattern as the scrub warning).
  useEffect(() => {
    if (!opError) return;
    const id = setTimeout(() => setOpError(null), 6000);
    return () => clearTimeout(id);
  }, [opError]);

  // Mirror the playhead into the shared external store so the clip inspector
  // (rendered by App, outside this tree) can split at the current position
  // without App re-rendering on every playback frame.
  useEffect(() => {
    publishPlayheadMs(playheadMs);
  }, [playheadMs]);

  // Draw the ruler-aligned waveform window into the canvas.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    const width = view.widthPx;
    canvas.width = width * dpr;
    canvas.height = WAVE_H * dpr;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.scale(dpr, dpr);
    ctx.clearRect(0, 0, width, WAVE_H);
    ctx.fillStyle = "rgba(255,255,255,0.02)";
    ctx.fillRect(0, 0, width, WAVE_H);

    if (!waveform || waveform.levels.length === 0) return;
    // The waveform spans the primary source video, not the whole timeline.
    const sourceDurationMs = Math.max(1, project.video_duration_ms);
    // Pick the pyramid level matching the source content width.
    const targetBuckets = sourceDurationMs * view.pxPerMs;
    let level = waveform.levels[waveform.levels.length - 1];
    for (const lv of waveform.levels) {
      if (lv.length >= targetBuckets) {
        level = lv;
        break;
      }
    }
    if (!level || level.length === 0) return;

    const mid = WAVE_H / 2;
    const amp = WAVE_H / 2 - 2;
    ctx.strokeStyle = "rgba(79,209,197,0.85)";
    ctx.lineWidth = 1;
    ctx.beginPath();
    for (let x = 0; x < width; x++) {
      const ms = tl.xToMs(x, view);
      if (ms < 0 || ms > sourceDurationMs) continue;
      const frac = ms / sourceDurationMs;
      const peak =
        level[Math.min(level.length - 1, Math.floor(frac * level.length))];
      if (!peak) continue;
      ctx.moveTo(x + 0.5, mid - peak.max * amp);
      ctx.lineTo(x + 0.5, mid - peak.min * amp);
    }
    ctx.stroke();
  }, [waveform, view, project.video_duration_ms]);

  // ── interactions ───────────────────────────────────────────────────────────
  // Stable (useCallback) so the memoized ruler/lane subtrees they are passed to
  // don't re-render on every playhead tick — their deps change on pan/zoom or
  // project edits, never per animation frame.

  /** ms under a client X, clamped to the timeline + frame-snapped. */
  const clientXToMs = useCallback(
    (clientX: number): number => {
      const el = viewportRef.current;
      if (!el) return 0;
      const x = clientX - el.getBoundingClientRect().left;
      return tl.snapToFrame(
        Math.max(0, Math.min(durationMs, tl.xToMs(x, view))),
        fps,
      );
    },
    [durationMs, view, fps],
  );

  // Pointer-down on the ruler/waveform/lane: seek there AND enter the quality
  // ladder's interaction rung until the pointer is released (see `endScrub`).
  const seekToX = useCallback(
    (clientX: number) => {
      setScrubbing(true);
      seekTo(clientXToMs(clientX));
    },
    [clientXToMs, seekTo],
  );

  function onWheel(e: React.WheelEvent) {
    const el = viewportRef.current;
    if (!el) return;
    if (e.ctrlKey || e.metaKey) {
      const anchorX = e.clientX - el.getBoundingClientRect().left;
      setView((v) => {
        const z = tl.zoomAround(v, e.deltaY < 0 ? 1.15 : 1 / 1.15, anchorX);
        return {
          ...z,
          scrollMs: clampScroll(z.scrollMs, z.pxPerMs, v.widthPx),
        };
      });
    } else {
      setView((v) => ({
        ...v,
        scrollMs: clampScroll(
          v.scrollMs + (e.deltaX || e.deltaY) / v.pxPerMs,
          v.pxPerMs,
          v.widthPx,
        ),
      }));
    }
  }

  // Buttons pin the PLAYHEAD (not the viewport centre) — the standard "zoom
  // toward what I'm looking at" behaviour when there's no pointer to anchor
  // on. Off-screen playhead clamps to the nearest edge instead of anchoring
  // outside the viewport (which would pan the whole timeline off-window).
  function zoomButton(factor: number) {
    setView((v) => {
      const anchorX = Math.min(Math.max(tl.msToX(playheadMs, v), 0), v.widthPx);
      const z = tl.zoomAround(v, factor, anchorX);
      return { ...z, scrollMs: clampScroll(z.scrollMs, z.pxPerMs, v.widthPx) };
    });
  }

  // Keep the gutter's header column vertically aligned with the lanes as they
  // scroll (both grow with the track count).
  function syncHeaderScroll() {
    if (headerScrollRef.current && lanesScrollRef.current) {
      headerScrollRef.current.scrollTop = lanesScrollRef.current.scrollTop;
    }
  }

  // ── caption drag (move / resize) — preview locally, commit on release ──────
  const onCaptionPointerDown = useCallback(
    (e: React.PointerEvent, c: Caption, kind: CaptionDrag["kind"]) => {
      e.stopPropagation();
      (e.target as Element).setPointerCapture?.(e.pointerId);
      setSelectedId(c.id);
      selectClip(null);
      setDrag({
        kind,
        id: c.id,
        startClientX: e.clientX,
        origStart: c.start_ms,
        origEnd: c.end_ms,
        deltaMs: 0,
      });
    },
    [selectClip],
  );

  // ── clip drag (move across tracks / trim) — preview locally, commit on release
  const onClipPointerDown = useCallback(
    (
      e: React.PointerEvent,
      track: Track,
      ti: TimelineItem,
      kind: ClipDrag["kind"],
    ) => {
      e.stopPropagation();
      if (track.locked || ti.locked) return;
      (e.target as Element).setPointerCapture?.(e.pointerId);
      selectClip(ti.id);
      setSelectedId(null);
      setClipDrag({
        kind,
        id: ti.id,
        trackId: track.id,
        trackKind: track.kind,
        speed: Math.max(0.01, ti.speed),
        startClientX: e.clientX,
        origStart: ti.timeline_start_ms,
        origInMs: ti.in_ms,
        origOutMs: ti.out_ms,
        deltaMs: 0,
        targetTrackId: track.id,
      });
    },
    [selectClip],
  );

  function onPointerMove(e: React.PointerEvent) {
    if (drag) {
      moveCaptionDrag(e);
      return;
    }
    if (clipDrag) moveClipDrag(e);
  }

  function moveCaptionDrag(e: React.PointerEvent) {
    if (!drag) return;
    const rawDelta = (e.clientX - drag.startClientX) / view.pxPerMs;
    const base = drag.kind === "resize-end" ? drag.origEnd : drag.origStart;
    let edge = base + rawDelta;
    if (snapEnabled) {
      const [vs, ve] = tl.visibleRange(view);
      const targets = [0, durationMs, playheadMs];
      for (const { item } of tl.visibleCaptions(captions, vs, ve)) {
        if (item.id === drag.id) continue;
        targets.push(item.start_ms, item.end_ms);
      }
      edge = tl.snap(edge, targets, view.pxPerMs);
    }
    const deltaMs = tl.snapToFrame(edge, fps) - base;
    setDrag({ ...drag, deltaMs });
  }

  function moveClipDrag(e: React.PointerEvent) {
    if (!clipDrag) return;
    const rawDelta = (e.clientX - clipDrag.startClientX) / view.pxPerMs;
    // The edge being dragged, expressed on the timeline. resize-end targets the
    // clip's trailing edge (source-out mapped through speed); move/resize-start
    // target the leading edge at timeline_start_ms.
    const timelineBase =
      clipDrag.kind === "resize-end"
        ? clipDrag.origStart +
          (clipDrag.origOutMs - clipDrag.origInMs) / clipDrag.speed
        : clipDrag.origStart;
    let edge = timelineBase + rawDelta;
    if (snapEnabled) {
      const targets = [0, durationMs, playheadMs];
      for (const arr of clipsByTrack.values()) {
        for (const s of arr) {
          if (s.ti.id === clipDrag.id) continue;
          targets.push(s.start_ms, s.end_ms);
        }
      }
      edge = tl.snap(edge, targets, view.pxPerMs);
    }
    const deltaMs = tl.snapToFrame(edge, fps) - timelineBase;

    // Vertical hit-test → cross-track target (move only, compatible kind).
    // Sticky against `clipDrag.targetTrackId` (the currently-committed target,
    // not the clip's origin) so repeated crossings don't overshoot the band
    // on every frame — see TRACK_SWITCH_HYSTERESIS_PX.
    let targetTrackId = clipDrag.trackId;
    if (clipDrag.kind === "move" && lanesScrollRef.current) {
      const rect = lanesScrollRef.current.getBoundingClientRect();
      const y = e.clientY - rect.top + lanesScrollRef.current.scrollTop;
      const hit = trackAtYSticky(
        y,
        project.tracks,
        LANE_H,
        clipDrag.targetTrackId,
      );
      if (hit && !hit.locked && hit.kind === clipDrag.trackKind) {
        targetTrackId = hit.id;
      }
    }
    setDropTrackId(clipDrag.kind === "move" ? targetTrackId : null);
    setClipDrag({ ...clipDrag, deltaMs, targetTrackId });
  }

  async function onPointerUp() {
    setScrubbing(false);
    if (drag) {
      const d = drag;
      setDrag(null);
      if (d.deltaMs === 0) return;
      try {
        await run((p) =>
          d.kind === "move"
            ? ipc.ops.moveCaption(p, d.id, d.deltaMs)
            : ipc.ops.resizeCaption(
                p,
                d.id,
                d.kind === "resize-start"
                  ? d.origStart + d.deltaMs
                  : d.origStart,
                d.kind === "resize-end" ? d.origEnd + d.deltaMs : d.origEnd,
              ),
        );
      } catch {
        // Clamped/invalid drag — leave the project untouched.
      }
      return;
    }
    if (clipDrag) {
      const d = clipDrag;
      setClipDrag(null);
      setDropTrackId(null);
      // Pure commit math (timeline↔source domain mapping, left-edge clamping,
      // no-op detection) lives in clipDrag.ts — unit-tested in isolation.
      const op = clipDragToOp(
        {
          id: d.id,
          track_id: d.trackId,
          in_ms: d.origInMs,
          out_ms: d.origOutMs,
          timeline_start_ms: d.origStart,
          speed: d.speed,
        },
        d.kind,
        d.deltaMs,
        d.targetTrackId,
      );
      if (op.op === "none") return;
      try {
        await run((p) => {
          if (op.op === "move") {
            return ipc.timeline.moveTimelineItem(
              p,
              op.itemId,
              op.trackId,
              op.timelineStartMs,
            );
          }
          if (op.op === "trim-start") {
            return ipc.timeline.trimTimelineItem(p, op.itemId, {
              newInMs: op.newInMs,
              newTimelineStartMs: op.newTimelineStartMs,
            });
          }
          // trim-end: only the source-out edge moves.
          return ipc.timeline.trimTimelineItem(p, op.itemId, {
            newOutMs: op.newOutMs,
          });
        });
      } catch {
        // Clamped/invalid trim/move — leave the project untouched.
      }
    }
  }

  // A pointercancel (touch/pen gesture takeover, OS-level interruption)
  // releases the pointer capture WITHOUT a pointerup — abort both drags
  // without committing so the ghost doesn't stay glued to later hover moves.
  function onPointerCancel() {
    setScrubbing(false);
    setDrag(null);
    setClipDrag(null);
    setDropTrackId(null);
  }

  // Drop a media row from the bin onto a lane → place it as a clip.
  const onLaneDrop = useCallback(
    async (e: React.DragEvent, track: Track) => {
      const mediaId = e.dataTransfer.getData(MEDIA_DND_MIME);
      setDropTrackId(null);
      if (!mediaId || track.locked || track.kind === "caption") return;
      e.preventDefault();
      const media = mediaById.get(mediaId);
      if (!media) return;
      // Audio-only media carries no visuals — compose export ignores it on a
      // video track (`is_visual`), so placing it there would make preview and
      // export disagree. It belongs on an audio track.
      if (media.kind === "audio_only" && track.kind === "video") return;
      const dropTimeMs = clientXToMs(e.clientX);
      try {
        await run((p) =>
          ipc.timeline.addTimelineItem(
            p,
            track.id,
            mediaId,
            0,
            media.duration_ms,
            dropTimeMs,
            "av",
          ),
        );
      } catch {
        // Overlapping/invalid placement — the backend clamps or rejects.
      }
    },
    [mediaById, clientXToMs, run],
  );

  const onLaneDragOver = useCallback((e: React.DragEvent, track: Track) => {
    if (track.locked || track.kind === "caption") return;
    if (!e.dataTransfer.types.includes(MEDIA_DND_MIME)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
    // Functional identity bailout — returning the same reference skips the
    // re-render entirely when the pointer stays over the same lane.
    setDropTrackId((prev) => (prev === track.id ? prev : track.id));
  }, []);

  const onLaneDragLeave = useCallback((track: Track) => {
    setDropTrackId((prev) => (prev === track.id ? null : prev));
  }, []);

  // Clicking empty lane space seeks + clears both selections.
  const onLanePointerDown = useCallback(
    (e: React.PointerEvent) => {
      seekToX(e.clientX);
      setSelectedId(null);
      selectClip(null);
    },
    [seekToX, selectClip],
  );

  const selectAndSeek = useCallback(
    (c: Caption) => {
      setSelectedId(c.id);
      selectClip(null);
      seekTo(c.start_ms);
    },
    [selectClip, seekTo],
  );

  // Clicking a clip selects it and parks the playhead at its start.
  const onClipSelect = useCallback(
    (ti: TimelineItem) => {
      selectClip(ti.id);
      setSelectedId(null);
      seekTo(ti.timeline_start_ms);
    },
    [selectClip, seekTo],
  );

  function step(dir: -1 | 1, count: number) {
    const idx = captions.findIndex((c) => c.id === selectedId);
    if (idx === -1) {
      seekTo(
        tl.snapToFrame(
          Math.max(
            0,
            Math.min(durationMs, playheadNow() + ((dir * 1000) / fps) * count),
          ),
          fps,
        ),
      );
      return;
    }
    const next =
      captions[Math.max(0, Math.min(captions.length - 1, idx + dir * count))];
    if (next) selectAndSeek(next);
  }

  function onKeyDown(e: React.KeyboardEvent) {
    // Keystrokes typed into a focused field are never timeline shortcuts —
    // mirrors the app-wide typing guard in useUndoHotkeys. Checked FIRST, so
    // Cmd+C/Cmd+V below still reach a focused text field's own native
    // copy/paste instead of the timeline's clipboard.
    const target = e.target as HTMLElement | null;
    if (
      target &&
      (target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA" ||
        target.isContentEditable)
    ) {
      return;
    }
    // ⌘D duplicate, ⌘C/⌘V copy-paste — the only modified chords the timeline
    // claims. Every OTHER modified chord (Cmd+K command palette, Cmd+S save,
    // menu accelerators) belongs to an app-level handler and must be left
    // alone, unprevented — the fallthrough bail below is unchanged for those.
    if ((e.metaKey || e.ctrlKey) && !e.altKey) {
      const modKey = e.key.toLowerCase();
      if (modKey === "d") {
        e.preventDefault();
        duplicateSelectedClip();
        return;
      }
      if (modKey === "c") {
        e.preventDefault();
        copySelectedClip();
        return;
      }
      if (modKey === "v") {
        e.preventDefault();
        pasteClipboard();
        return;
      }
    }
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const lower = e.key.toLowerCase();
    if (lower === "j" || lower === "k" || lower === "l") {
      e.preventDefault();
      shuttle(lower);
      return;
    }
    if (lower === "s") {
      e.preventDefault();
      setSnapEnabled((s) => !s);
      return;
    }
    if (lower === "b") {
      // Blade — split the selected clip at the playhead. B (not S, which is
      // taken by snap) matches the standard NLE blade shortcut.
      e.preventDefault();
      splitSelectedAtPlayhead();
      return;
    }
    switch (e.key) {
      case " ":
        e.preventDefault();
        togglePlay();
        break;
      case "Delete":
      case "Backspace":
        e.preventDefault();
        deleteSelectedClip();
        break;
      case "ArrowLeft":
        e.preventDefault();
        step(-1, e.shiftKey ? 5 : 1);
        break;
      case "ArrowRight":
        e.preventDefault();
        step(1, e.shiftKey ? 5 : 1);
        break;
      case "Home":
        e.preventDefault();
        if (captions[0]) selectAndSeek(captions[0]);
        break;
      case "End":
        e.preventDefault();
        if (captions.length) selectAndSeek(captions[captions.length - 1]);
        break;
    }
  }

  // The selected clip's live item — or null when the selection points at a
  // deleted item, a caption is selected instead, or its track is locked.
  function selectedEditableClip(): TimelineItem | null {
    if (!selectedClipId) return null;
    const ti = project.timeline_items.find((i) => i.id === selectedClipId);
    if (!ti || ti.locked) return null;
    const track = project.tracks.find((tr) => tr.id === ti.track_id);
    return track?.locked ? null : ti;
  }

  // Blade (B key + the inspector's split button drives the same op): split the
  // selected clip where the playhead stands. Only meaningful strictly inside
  // the clip's span — an edge cut would leave a zero-length half, which the
  // backend rejects anyway.
  function splitSelectedAtPlayhead() {
    const ti = selectedEditableClip();
    if (!ti) return;
    const { start_ms, end_ms } = itemSpan(ti);
    if (playheadMs <= start_ms || playheadMs >= end_ms) return;
    const at = playheadMs;
    void run((p) => ipc.timeline.splitTimelineItem(p, ti.id, at)).catch(
      () => {},
    );
  }

  // Delete/Backspace: ripple-delete the selected clip (later same-track clips
  // slide left), then clear the selection so the inspector closes with it.
  function deleteSelectedClip() {
    const ti = selectedEditableClip();
    if (!ti) return;
    void run((p) => ipc.timeline.rippleDeleteItem(p, ti.id))
      .then(() => selectClip(null))
      .catch(() => {});
  }

  // ⌘D: duplicate the selected clip. `duplicate_timeline_item` (Rust) places
  // the copy immediately after the original on the same track — see its
  // doc comment for the placement clamp (slots into a gap, trims to fit a
  // small one, or falls back to the end of the track).
  function duplicateSelectedClip() {
    const ti = selectedEditableClip();
    if (!ti) return;
    void run((p) => duplicateTimelineItem(p, ti.id)).catch(() => {});
  }

  // ⌘C: remember the selected clip's id + its track's kind (not a frozen copy
  // of its fields — see `clipboardRef`'s own comment). Works even on a locked
  // clip/track: copying doesn't mutate anything, only pasting does.
  function copySelectedClip() {
    const ti = project.timeline_items.find((i) => i.id === selectedClipId);
    if (!ti) return;
    const track = project.tracks.find((t) => t.id === ti.track_id);
    if (!track) return;
    clipboardRef.current = {
      id: ti.id,
      trackId: ti.track_id,
      trackKind: track.kind,
    };
  }

  // ⌘V: duplicate the copied clip, then move that duplicate to the playhead —
  // on the currently selected (unlocked) clip's track when there is one,
  // else back onto the track the copy came from. Same "same track kind" rule
  // the pointer drag/drop path enforces (a video clip can't land on a
  // caption/audio lane); a locked or vanished target track is a silent no-op,
  // matching every other keyboard op in this file.
  function pasteClipboard() {
    const copied = clipboardRef.current;
    if (!copied) return;
    const selected = selectedEditableClip();
    const targetTrackId = selected ? selected.track_id : copied.trackId;
    const targetTrack = project.tracks.find((t) => t.id === targetTrackId);
    if (
      !targetTrack ||
      targetTrack.locked ||
      targetTrack.kind !== copied.trackKind
    ) {
      return;
    }
    const at = playheadMs;
    void run(async (p) => {
      const duplicated = await duplicateTimelineItem(p, copied.id);
      const newId = newestTimelineItemId(p, duplicated);
      if (!newId) return duplicated;
      return ipc.timeline.moveTimelineItem(
        duplicated,
        newId,
        targetTrackId,
        at,
      );
    }).catch(() => {});
  }

  /**
   * Add a TEXT OVERLAY at the playhead (R5-C).
   *
   * Everything happens inside ONE `run` callback, so creating the Overlay
   * track, placing the item and nudging it off the frame corner land as a
   * SINGLE undo entry — three separate `run` calls would make the user press
   * ⌘Z three times to undo one button press.
   *
   * The nudge is not cosmetic: `Transform::default()` is `x: 0, y: 0`, and the
   * export anchors a text overlay's TOP-LEFT at `(width*x, height*y)` — the
   * same fractions `overlay=` uses for a picture clip — so an untouched item
   * would render hard against the frame's corner. `0.08 / 0.78` is where a
   * lower third goes; the inspector's X/Y sliders move it from there.
   */
  function addTextOverlay() {
    setOpError(null);
    const at = playheadMs;
    void run(async (p) => {
      let base = p;
      let trackId = p.tracks.find(
        (tr) => tr.kind === "overlay" && !tr.locked,
      )?.id;
      if (!trackId) {
        base = await ipc.timeline.addTrack(
          p,
          "overlay",
          t("mediaBinAddOverlayTrack"),
        );
        trackId = base.tracks.find(
          (tr) => !p.tracks.some((old) => old.id === tr.id),
        )?.id;
        if (!trackId) return p;
      }
      const added = await ipc.timeline.addTextItem(
        base,
        trackId,
        at,
        DEFAULT_TEXT_MS,
        t("timelineAddTextPlaceholder"),
      );
      const newId = newestTimelineItemId(base, added);
      const placed = added.timeline_items.find((i) => i.id === newId);
      if (!newId || !placed) return added;
      return ipc.timeline.setTransform(added, newId, {
        ...placed.transform,
        x: 0.08,
        y: 0.78,
      });
    }).catch((e) => setOpError((e as Error).message));
  }

  // Remove an (empty) track. The backend rejects a non-empty one — surface
  // that message instead of failing silently (there is no ghost to snap back).
  const removeTrack = useCallback(
    (track: Track) => {
      setOpError(null);
      void run((p) => ipc.timeline.removeTrack(p, track.id)).catch((e) =>
        setOpError((e as Error).message),
      );
    },
    [run],
  );

  // Toggle a track flag through the shared undo stack. `enabled` is the
  // visibility/audibility switch honoured by both the preview (`previewMap.ts`)
  // and the export (`compose.rs`'s track_visible/has_audio) for EVERY track
  // kind — not just audio's mute/solo — so it lives here beside them rather
  // than the audible-only pair below.
  const toggleFlag = useCallback(
    (track: Track, flag: "enabled" | "muted" | "solo" | "locked") => {
      void run((p) =>
        ipc.timeline.setTrackFlags(p, track.id, { [flag]: !track[flag] }),
      ).catch(() => {});
    },
    [run],
  );

  // Track fader (R2 audio) — the backend clamps, so this round-trips through
  // Rust like every other timeline op, but a slider is dragged in bursts: land
  // it with the coalescing commit (one undo step per drag) instead of `run`
  // (one per tick). `useCallback` keeps this identity stable across renders —
  // `LaneHeaders`/`TrackHeader` sit in a memoized subtree the playback clock
  // re-renders ~60×/s, and a prop that changed identity every tick would
  // defeat that memoization for every track header, every frame.
  const coalescedCommit = useCoalescedCommit();
  const setTrackVolume = useCallback(
    (track: Track, volumeDb: number) => {
      coalescedCommit(`track-volume:${track.id}`, (p) =>
        ipc.timeline.setTrackVolume(p, track.id, volumeDb),
      );
    },
    [coalescedCommit],
  );

  // Close every gap on a track: each clip slides back against its
  // predecessor (locked clips stay anchored — see services::timeline_ops's
  // gap engine). A no-op on a gapless track lands harmlessly on the undo
  // stack same as any other clamped op.
  const packTrackGaps = useCallback(
    (track: Track) => {
      void run((p) => ipc.timeline.packTrack(p, track.id)).catch(() => {});
    },
    [run],
  );

  // Move a lane up (toward the top = higher index) or down.
  const reorder = useCallback(
    (track: Track, dir: -1 | 1) => {
      const next = track.index + dir;
      if (next < 0) return;
      void run((p) => ipc.timeline.reorderTrack(p, track.id, next)).catch(
        () => {},
      );
    },
    [run],
  );

  // ── preview render (proxy) ─────────────────────────────────────────────────
  // Flatten the timeline to a temp file and load it into the preview so the user
  // sees the true composite (transitions/overlays/PiP). Best-effort: off-Tauri /
  // if the compose engine can't run, it silently returns to the live preview.
  async function renderPreview() {
    if (!isTauri()) return;
    setPreviewState("rendering");
    try {
      const out = await join(await appCacheDir(), "sundayedit-preview.mp4");
      // The flatten inherits the ladder rung that is live when it is asked for:
      // parked (the normal case) it renders at full proxy geometry, exactly as
      // before; asked for mid-playback or mid-drag — where the user wants it
      // FAST — it renders a proportionally smaller proxy. Export is a different
      // command (`compose.render`) and never comes through here.
      const ok = await renderPreviewProxy(project, out, quality.scalePct);
      if (ok) {
        setProxySrc(`${convertFileSrc(out)}?t=${Date.now()}`);
        setPreviewState("done");
      } else {
        setPreviewState("idle");
      }
    } catch {
      setPreviewState("idle");
    }
  }

  function clearPreview() {
    setProxySrc(undefined);
    setPreviewState("idle");
  }

  // Any project change (drag commit, trim, add/delete, undo/redo…) makes a
  // rendered proxy stale: it composites the PRE-edit timeline. Drop it so the
  // preview returns to the live per-clip mapping instead of silently playing
  // an outdated flattened file. (renderPreview itself never changes `project`,
  // so a fresh render survives; the initial mount is a no-op.)
  useEffect(() => {
    setProxySrc(undefined);
    setPreviewState("idle");
  }, [project]);

  // ── render ───────────────────────────────────────────────────────────────
  // Memoized on [view]/[items] — NOT playhead — so the ~60 fps playback clock
  // re-renders only the toolbar timecode, the playhead line and the player;
  // the memoized ruler/header/lane subtrees below bail out per tick.
  const [visStart, visEnd] = useMemo(() => tl.visibleRange(view), [view]);
  const visibleCaptionRows = useMemo(
    () => tl.visibleCaptions(captions, visStart, visEnd),
    [captions, visStart, visEnd],
  );
  const ticks = useMemo(() => tl.rulerTicks(view, 80), [view]);
  const playheadX = tl.msToX(playheadMs, view);

  return (
    <div
      data-testid="timeline"
      className="flex h-full flex-col bg-[var(--color-bg)] text-[var(--color-fg)] outline-none"
      tabIndex={0}
      onKeyDown={onKeyDown}
    >
      {/* Preview — a real <video> bound to the playhead clock; multi-track when
          the project has placed clips, else the legacy single-source src. */}
      <div className="relative flex flex-[3] items-center justify-center border-b border-[var(--color-border)] bg-black/40">
        {videoSrc || (isTauri() && project.timeline_items.length > 0) ? (
          <MediaPlayer
            src={videoSrc}
            project={project}
            proxySrc={proxySrc}
            playheadMs={playheadMs}
            rate={rate}
            durationMs={durationMs}
            fps={fps}
            scalePct={quality.scalePct}
            onConflict={() => setScrubWarning(true)}
          />
        ) : (
          <div className="text-center">
            <div className="font-mono text-[var(--text-ui-2xl)] tabular-nums">
              {tl.formatTimecode(playheadMs, fps)}
            </div>
            <div className="mt-1 text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
              {project.name} · {tl.formatTimecode(durationMs, fps)}{" "}
              {t("timelineTotalSuffix")}
            </div>
          </div>
        )}
        {scrubWarning && (
          <div
            role="alert"
            className="absolute inset-x-0 bottom-0 bg-[var(--color-warning)]/90 px-3 py-1 text-center text-[var(--text-ui-xs)] text-[var(--color-neutral-950)]"
          >
            {t("mediaPlayerScrubWarning")}
          </div>
        )}
        {opError && (
          <div
            role="alert"
            className="absolute inset-x-0 top-0 bg-[var(--color-danger,#b3261e)]/90 px-3 py-1 text-center text-[var(--text-ui-xs)] text-white"
          >
            {opError}
          </div>
        )}
      </div>

      {/* Toolbar */}
      <div className="flex items-center gap-2 border-b border-[var(--color-border)] px-3 py-1.5">
        <button
          type="button"
          onClick={togglePlay}
          className="grid h-7 w-7 place-items-center rounded-md hover:bg-[var(--color-bg-surface)]"
          aria-label={playing ? t("timelinePause") : t("timelinePlay")}
        >
          {playing ? <Pause size={15} /> : <Play size={15} />}
        </button>
        <span className="font-mono text-[var(--text-ui-xs)] tabular-nums text-[var(--color-fg-muted)]">
          {tl.formatTimecode(playheadMs, fps)}
        </span>
        {rate !== 0 && rate !== 1 && (
          <span className="font-mono text-[var(--text-ui-xs)] tabular-nums text-[var(--color-accent-400)]">
            {rate < 0 ? `◂ ${-rate}×` : `${rate}× ▸`}
          </span>
        )}

        {/* Preview-render (proxy): flatten the timeline into the preview. */}
        {isTauri() && project.timeline_items.length > 0 && (
          <div className="ml-2 flex items-center gap-1.5">
            <button
              type="button"
              onClick={() => void renderPreview()}
              disabled={previewState === "rendering"}
              className={cn(
                "inline-flex items-center gap-1.5 rounded-md border px-2 py-1 text-[var(--text-ui-xs)] font-medium transition-colors disabled:opacity-60",
                previewState === "done"
                  ? "border-[var(--color-accent-500)]/50 text-[var(--color-accent-300)]"
                  : "border-[var(--color-border)] text-[var(--color-fg-muted)] hover:border-[var(--color-accent-600)] hover:text-[var(--color-fg)]",
              )}
            >
              {previewState === "rendering" ? (
                <Loader2 size={13} className="animate-spin" />
              ) : (
                <Clapperboard size={13} />
              )}
              {previewState === "rendering"
                ? t("timelinePreviewRendering")
                : previewState === "done"
                  ? t("timelinePreviewDone")
                  : t("timelinePreviewRender")}
            </button>
            {previewState === "done" && (
              <button
                type="button"
                onClick={clearPreview}
                title={t("timelinePreviewLive")}
                aria-label={t("timelinePreviewLive")}
                className="grid h-6 w-6 place-items-center rounded-md text-[var(--color-fg-subtle)] hover:bg-[var(--color-bg-surface)] hover:text-[var(--color-fg)]"
              >
                <RotateCcw size={13} />
              </button>
            )}
          </div>
        )}

        <div className="flex-1" />
        <button
          type="button"
          onClick={() => setSnapEnabled((s) => !s)}
          aria-pressed={snapEnabled}
          className={cn(
            "grid h-7 w-7 place-items-center rounded-md hover:bg-[var(--color-bg-surface)]",
            snapEnabled
              ? "text-[var(--color-accent-400)]"
              : "text-[var(--color-fg-subtle)]",
          )}
          aria-label={t("timelineSnap")}
          title={t("timelineSnap")}
        >
          <Magnet size={15} />
        </button>
        <button
          type="button"
          data-testid="timeline-add-text"
          onClick={addTextOverlay}
          className="grid h-7 w-7 place-items-center rounded-md text-[var(--color-fg-subtle)] hover:bg-[var(--color-bg-surface)] hover:text-[var(--color-fg)]"
          aria-label={t("timelineAddText")}
          title={t("timelineAddText")}
        >
          <Type size={15} />
        </button>
        <span className="text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
          {(view.pxPerMs * 1000).toFixed(1)} px/s
        </span>
        <button
          type="button"
          onClick={() => zoomButton(1 / 1.3)}
          className="grid h-7 w-7 place-items-center rounded-md hover:bg-[var(--color-bg-surface)]"
          aria-label={t("timelineZoomOut")}
        >
          <ZoomOut size={15} />
        </button>
        <button
          type="button"
          onClick={() => zoomButton(1.3)}
          className="grid h-7 w-7 place-items-center rounded-md hover:bg-[var(--color-bg-surface)]"
          aria-label={t("timelineZoomIn")}
        >
          <ZoomIn size={15} />
        </button>
      </div>

      {/* Body: track-header gutter + time viewport */}
      <div className="flex min-h-0 flex-[2]">
        {/* Left gutter — one header per track. */}
        <div
          className="flex shrink-0 flex-col border-r border-[var(--color-border)] bg-[var(--color-bg-elevated)]"
          style={{ width: GUTTER_W }}
        >
          <div
            className="shrink-0 border-b border-[var(--color-border)]"
            style={{ height: RULER_H + WAVE_H }}
          />
          <div ref={headerScrollRef} className="min-h-0 flex-1 overflow-hidden">
            <LaneHeaders
              stacked={stacked}
              onToggle={toggleFlag}
              onMove={reorder}
              onRemove={removeTrack}
              onPackGaps={packTrackGaps}
              onVolumeChange={setTrackVolume}
              // Individual strings (not an object) so memo's shallow compare
              // holds even though `t` is a fresh closure every render.
              labelEnabled={t("trackEnabled")}
              labelMute={t("trackMute")}
              labelSolo={t("trackSolo")}
              labelLock={t("trackLock")}
              labelUp={t("trackMoveUp")}
              labelDown={t("trackMoveDown")}
              labelRemove={t("trackRemove")}
              labelCloseGaps={t("trackCloseGaps")}
              labelVolume={t("trackVolume")}
            />
          </div>
        </div>

        {/* Time viewport */}
        <div
          ref={viewportRef}
          className="relative min-w-0 flex-1 select-none overflow-hidden"
          onWheel={onWheel}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerCancel}
          onPointerLeave={() => (drag || clipDrag) && onPointerUp()}
        >
          {/* Ruler */}
          <RulerBar ticks={ticks} view={view} fps={fps} onSeek={seekToX} />

          {/* Waveform (primary source) */}
          <canvas
            ref={canvasRef}
            className="block w-full cursor-text"
            style={{ height: WAVE_H }}
            onPointerDown={(e) => seekToX(e.clientX)}
          />

          {/* Stacked track lanes */}
          <div
            ref={lanesScrollRef}
            onScroll={syncHeaderScroll}
            className="overflow-y-auto"
            style={{ height: `calc(100% - ${RULER_H + WAVE_H}px)` }}
          >
            <LaneStack
              stacked={stacked}
              view={view}
              visStart={visStart}
              visEnd={visEnd}
              visibleCaptionRows={visibleCaptionRows}
              clipsByTrack={clipsByTrack}
              mediaById={mediaById}
              missingMediaIds={missingMediaIds}
              drag={drag}
              clipDrag={clipDrag}
              selectedId={selectedId}
              selectedClipId={selectedClipId}
              dropTrackId={dropTrackId}
              onCaptionPointerDown={onCaptionPointerDown}
              onCaptionSelect={selectAndSeek}
              onClipPointerDown={onClipPointerDown}
              onClipSelect={onClipSelect}
              onLaneDragOver={onLaneDragOver}
              onLaneDragLeave={onLaneDragLeave}
              onLaneDrop={onLaneDrop}
              onLanePointerDown={onLanePointerDown}
            />
          </div>

          {/* Playhead across the viewport */}
          {playheadX >= 0 && playheadX <= view.widthPx && (
            <div
              className="pointer-events-none absolute top-0 bottom-0 w-px bg-white/90"
              style={{ left: playheadX }}
            />
          )}
        </div>
      </div>

      <div className="border-t border-[var(--color-border)] px-3 py-1 text-[10px] text-[var(--color-fg-subtle)]">
        {t("timelineHelp")}
      </div>
    </div>
  );
}

// ── lane sub-components ───────────────────────────────────────────────────────
// RulerBar / LaneHeaders / LaneStack are React.memo subtrees: the playback
// clock re-renders Timeline ~60×/s (playheadMs is its state), but every prop
// these receive is playhead-independent and referentially stable per tick
// (useMemo'd arrays/maps, useCallback handlers, primitives) — so the whole
// header/lane forest reconciles ZERO nodes per frame. They re-render only on
// pan/zoom (view), project edits (items), drags and selection changes.

const RulerBar = memo(function RulerBar({
  ticks,
  view,
  fps,
  onSeek,
}: {
  ticks: number[];
  view: tl.TimelineView;
  fps: number;
  onSeek: (clientX: number) => void;
}) {
  return (
    <div
      className="relative border-b border-[var(--color-border)] bg-[var(--color-bg-elevated)]"
      style={{ height: RULER_H }}
      onPointerDown={(e) => onSeek(e.clientX)}
    >
      {ticks.map((ms) => (
        <div
          key={ms}
          className="absolute top-0 flex h-full items-center"
          style={{ left: tl.msToX(ms, view) }}
        >
          <span className="border-l border-[var(--color-border)] pl-1 font-mono text-[9px] text-[var(--color-fg-subtle)]">
            {tl.formatTimecode(ms, fps)}
          </span>
        </div>
      ))}
    </div>
  );
});

const LaneHeaders = memo(function LaneHeaders({
  stacked,
  onToggle,
  onMove,
  onRemove,
  onPackGaps,
  onVolumeChange,
  labelEnabled,
  labelMute,
  labelSolo,
  labelLock,
  labelUp,
  labelDown,
  labelRemove,
  labelCloseGaps,
  labelVolume,
}: {
  stacked: Track[];
  onToggle: (
    track: Track,
    flag: "enabled" | "muted" | "solo" | "locked",
  ) => void;
  onMove: (track: Track, dir: -1 | 1) => void;
  onRemove: (track: Track) => void;
  onPackGaps: (track: Track) => void;
  onVolumeChange: (track: Track, volumeDb: number) => void;
  labelEnabled: string;
  labelMute: string;
  labelSolo: string;
  labelLock: string;
  labelUp: string;
  labelDown: string;
  labelRemove: string;
  labelCloseGaps: string;
  labelVolume: string;
}) {
  return (
    <>
      {stacked.map((track, i) => (
        <TrackHeader
          key={track.id}
          track={track}
          height={LANE_H}
          canMoveUp={i > 0}
          canMoveDown={i < stacked.length - 1}
          onToggle={onToggle}
          onMove={onMove}
          onRemove={onRemove}
          onPackGaps={onPackGaps}
          onVolumeChange={onVolumeChange}
          labels={{
            enabled: labelEnabled,
            mute: labelMute,
            solo: labelSolo,
            lock: labelLock,
            up: labelUp,
            down: labelDown,
            remove: labelRemove,
            closeGaps: labelCloseGaps,
            volume: labelVolume,
          }}
        />
      ))}
    </>
  );
});

const LaneStack = memo(function LaneStack({
  stacked,
  view,
  visStart,
  visEnd,
  visibleCaptionRows,
  clipsByTrack,
  mediaById,
  missingMediaIds,
  drag,
  clipDrag,
  selectedId,
  selectedClipId,
  dropTrackId,
  onCaptionPointerDown,
  onCaptionSelect,
  onClipPointerDown,
  onClipSelect,
  onLaneDragOver,
  onLaneDragLeave,
  onLaneDrop,
  onLanePointerDown,
}: {
  stacked: Track[];
  view: tl.TimelineView;
  visStart: number;
  visEnd: number;
  visibleCaptionRows: { index: number; item: Caption }[];
  clipsByTrack: Map<
    string,
    { start_ms: number; end_ms: number; ti: TimelineItem }[]
  >;
  mediaById: Map<string, MediaItem>;
  missingMediaIds: Set<string>;
  drag: CaptionDrag | null;
  clipDrag: ClipDrag | null;
  selectedId: string | null;
  selectedClipId: string | null;
  dropTrackId: string | null;
  onCaptionPointerDown: (
    e: React.PointerEvent,
    c: Caption,
    kind: CaptionDrag["kind"],
  ) => void;
  onCaptionSelect: (c: Caption) => void;
  onClipPointerDown: (
    e: React.PointerEvent,
    track: Track,
    ti: TimelineItem,
    kind: ClipDrag["kind"],
  ) => void;
  onClipSelect: (ti: TimelineItem) => void;
  onLaneDragOver: (e: React.DragEvent, track: Track) => void;
  onLaneDragLeave: (track: Track) => void;
  onLaneDrop: (e: React.DragEvent, track: Track) => void;
  onLanePointerDown: (e: React.PointerEvent) => void;
}) {
  return (
    <>
      {stacked.map((track) => (
        <div
          key={track.id}
          className={cn(
            "relative border-t border-[var(--color-border)]",
            dropTrackId === track.id &&
              "bg-[var(--color-accent-500)]/10 ring-1 ring-inset ring-[var(--color-accent-500)]/50",
            !track.enabled && "opacity-50",
          )}
          style={{ height: LANE_H }}
          onDragOver={(e) => onLaneDragOver(e, track)}
          onDragLeave={() => onLaneDragLeave(track)}
          onDrop={(e) => void onLaneDrop(e, track)}
          onPointerDown={onLanePointerDown}
        >
          {track.kind === "caption"
            ? visibleCaptionRows.map(({ item: c }) => (
                <CaptionBox
                  key={c.id}
                  caption={c}
                  view={view}
                  drag={drag?.id === c.id ? drag : null}
                  selected={selectedId === c.id}
                  locked={track.locked}
                  onPointerDown={(e, kind) => onCaptionPointerDown(e, c, kind)}
                  onSelect={() => onCaptionSelect(c)}
                />
              ))
            : tl
                .visibleCaptions(
                  clipsByTrack.get(track.id) ?? [],
                  visStart,
                  visEnd,
                )
                .map(({ item: span }) => (
                  <ClipBox
                    key={span.ti.id}
                    item={span.ti}
                    media={
                      span.ti.source_media_id
                        ? mediaById.get(span.ti.source_media_id)
                        : undefined
                    }
                    missing={
                      !!span.ti.source_media_id &&
                      missingMediaIds.has(span.ti.source_media_id)
                    }
                    view={view}
                    speed={Math.max(0.01, span.ti.speed)}
                    drag={clipDrag?.id === span.ti.id ? clipDrag : null}
                    selected={selectedClipId === span.ti.id}
                    locked={track.locked}
                    onPointerDown={(e, kind) =>
                      onClipPointerDown(e, track, span.ti, kind)
                    }
                    onSelect={() => onClipSelect(span.ti)}
                  />
                ))}
        </div>
      ))}
    </>
  );
});

function TrackHeader({
  track,
  height,
  canMoveUp,
  canMoveDown,
  onToggle,
  onMove,
  onRemove,
  onPackGaps,
  onVolumeChange,
  labels,
}: {
  track: Track;
  height: number;
  canMoveUp: boolean;
  canMoveDown: boolean;
  onToggle: (
    track: Track,
    flag: "enabled" | "muted" | "solo" | "locked",
  ) => void;
  onMove: (track: Track, dir: -1 | 1) => void;
  onRemove: (track: Track) => void;
  onPackGaps: (track: Track) => void;
  onVolumeChange: (track: Track, volumeDb: number) => void;
  labels: {
    enabled: string;
    mute: string;
    solo: string;
    lock: string;
    up: string;
    down: string;
    remove: string;
    closeGaps: string;
    volume: string;
  };
}) {
  const audible = track.kind === "video" || track.kind === "audio";
  // The gap engine works on TimelineItems; caption tracks store their content
  // in `project.captions` instead, so "close gaps" has nothing to act on
  // there. Hidden on a locked track too — same convention as clip drag/trim.
  const canPackGaps = track.kind !== "caption" && !track.locked;
  return (
    <div
      data-testid={`track-header-${track.id}`}
      className="flex items-center gap-1 border-t border-[var(--color-border)] px-2"
      style={{ height }}
    >
      {/* min-w floor: the fixed controls to the right must never squeeze the
          track name to zero width — it truncates, it does not disappear. */}
      <div className="flex min-w-14 flex-1 flex-col justify-center overflow-hidden">
        <span className="truncate text-[var(--text-ui-xs)] font-medium">
          {track.name}
        </span>
        <span className="text-[9px] uppercase tracking-wide text-[var(--color-fg-subtle)]">
          {track.kind}
        </span>
      </div>
      <div className="flex flex-col">
        <button
          type="button"
          disabled={!canMoveUp}
          onClick={() => onMove(track, 1)}
          title={labels.up}
          aria-label={labels.up}
          className="grid h-3.5 w-4 place-items-center text-[var(--color-fg-subtle)] hover:text-[var(--color-fg)] disabled:opacity-30"
        >
          <ChevronUp size={11} />
        </button>
        <button
          type="button"
          disabled={!canMoveDown}
          onClick={() => onMove(track, -1)}
          title={labels.down}
          aria-label={labels.down}
          className="grid h-3.5 w-4 place-items-center text-[var(--color-fg-subtle)] hover:text-[var(--color-fg)] disabled:opacity-30"
        >
          <ChevronDown size={11} />
        </button>
      </div>
      {/* Visibility/audibility switch (`Track.enabled`) — honoured by the
          preview and the export for EVERY track kind, unlike mute/solo which
          are audio-only. Offered here regardless of `audible` so a Caption or
          Overlay track can be hidden too. */}
      <FlagToggle
        active={!track.enabled}
        onClick={() => onToggle(track, "enabled")}
        label={labels.enabled}
        on={<EyeOff size={13} />}
        off={<Eye size={13} />}
        activeClass="text-[var(--color-danger)]"
        testId={`track-enabled-${track.id}`}
      />
      {audible && (
        <>
          <FlagToggle
            active={track.muted}
            onClick={() => onToggle(track, "muted")}
            label={labels.mute}
            on={<VolumeX size={13} />}
            off={<Volume2 size={13} />}
            activeClass="text-[var(--color-danger)]"
          />
          <FlagToggle
            active={track.solo}
            onClick={() => onToggle(track, "solo")}
            label={labels.solo}
            on={<Headphones size={13} />}
            off={<Headphones size={13} />}
            activeClass="text-[var(--color-accent-400)]"
          />
          {/* Track fader (R2 audio). Compact: no visible label/readout (the
              184px gutter has no room), but the title carries the exact dB
              and a double-click resets to 0 — same detent-at-unity contract
              as the inspector's gain slider, just without its own button. */}
          <input
            type="range"
            data-testid={`track-volume-${track.id}`}
            aria-label={`${labels.volume} ${formatDb(track.volume_db)}`}
            title={`${labels.volume}: ${formatDb(track.volume_db)}`}
            min={GAIN_DB_MIN}
            max={GAIN_DB_MAX}
            step={0.5}
            value={track.volume_db}
            onChange={(e) =>
              onVolumeChange(track, applyGainDetent(Number(e.target.value)))
            }
            onDoubleClick={() => onVolumeChange(track, 0)}
            className="h-1 w-8 shrink-0 accent-[var(--color-accent-500)]"
          />
        </>
      )}
      <FlagToggle
        active={track.locked}
        onClick={() => onToggle(track, "locked")}
        label={labels.lock}
        on={<Lock size={13} />}
        off={<Unlock size={13} />}
        activeClass="text-[var(--color-warning)]"
      />
      {canPackGaps && (
        <button
          type="button"
          data-testid={`pack-track-${track.id}`}
          onClick={() => onPackGaps(track)}
          title={labels.closeGaps}
          aria-label={labels.closeGaps}
          className="grid h-6 w-6 shrink-0 place-items-center rounded text-[var(--color-fg-subtle)] hover:bg-[var(--color-bg-surface)] hover:text-[var(--color-fg)]"
        >
          <FoldHorizontal size={13} />
        </button>
      )}
      <button
        type="button"
        data-testid="remove-track"
        onClick={() => onRemove(track)}
        title={labels.remove}
        aria-label={labels.remove}
        className="grid h-6 w-6 shrink-0 place-items-center rounded text-[var(--color-fg-subtle)] hover:bg-[var(--color-bg-surface)] hover:text-[var(--color-danger,#b3261e)]"
      >
        <X size={13} />
      </button>
    </div>
  );
}

function FlagToggle({
  active,
  onClick,
  label,
  on,
  off,
  activeClass,
  testId,
}: {
  active: boolean;
  onClick: () => void;
  label: string;
  on: React.ReactNode;
  off: React.ReactNode;
  activeClass: string;
  testId?: string;
}) {
  return (
    <button
      type="button"
      data-testid={testId}
      onClick={onClick}
      aria-pressed={active}
      title={label}
      aria-label={label}
      className={cn(
        "grid h-6 w-6 shrink-0 place-items-center rounded hover:bg-[var(--color-bg-surface)]",
        active ? activeClass : "text-[var(--color-fg-subtle)]",
      )}
    >
      {active ? on : off}
    </button>
  );
}

function CaptionBox({
  caption: c,
  view,
  drag,
  selected,
  locked,
  onPointerDown,
  onSelect,
}: {
  caption: Caption;
  view: tl.TimelineView;
  drag: CaptionDrag | null;
  selected: boolean;
  locked: boolean;
  onPointerDown: (e: React.PointerEvent, kind: CaptionDrag["kind"]) => void;
  onSelect: () => void;
}) {
  const start =
    c.start_ms + (drag && drag.kind !== "resize-end" ? drag.deltaMs : 0);
  const end =
    c.end_ms + (drag && drag.kind !== "resize-start" ? drag.deltaMs : 0);
  const left = tl.msToX(start, view);
  const width = Math.max(2, (end - start) * view.pxPerMs);
  const edgePx = tl.edgeHitWidthPx(width);
  const tier = worstTier(c);
  const text = c.words.map((w) => w.text).join(" ");
  return (
    <div
      className={cn(
        "absolute top-1 bottom-1 overflow-hidden rounded border-l-2 bg-[var(--color-bg-surface)] text-[var(--text-ui-xs)]",
        selected
          ? "ring-2 ring-[var(--color-accent-500)]"
          : "border-[var(--color-border)]",
      )}
      style={{ left, width, borderLeftColor: TIER_BORDER[tier] }}
      onPointerDown={(e) => !locked && onPointerDown(e, "move")}
      onClick={onSelect}
      title={text}
    >
      {!locked && (
        <>
          <span
            className="absolute inset-y-0 left-0 cursor-ew-resize hover:bg-[var(--color-accent-500)]/40"
            style={{ width: edgePx }}
            onPointerDown={(e) => onPointerDown(e, "resize-start")}
          />
          <span
            className="absolute inset-y-0 right-0 cursor-ew-resize hover:bg-[var(--color-accent-500)]/40"
            style={{ width: edgePx }}
            onPointerDown={(e) => onPointerDown(e, "resize-end")}
          />
        </>
      )}
      <div className="truncate px-2 py-1 text-[var(--color-fg-muted)]">
        {text || "—"}
      </div>
    </div>
  );
}

function ClipBox({
  item,
  media,
  missing,
  view,
  speed,
  drag,
  selected,
  locked,
  onPointerDown,
  onSelect,
}: {
  item: TimelineItem;
  media: MediaItem | undefined;
  /** The clip's source media is missing on disk — see `useMediaAvailability`.
   *  A plain boolean (not the media item / a lookup) so this prop's identity
   *  never breaks LaneStack's memoization. */
  missing: boolean;
  view: tl.TimelineView;
  speed: number;
  drag: ClipDrag | null;
  selected: boolean;
  locked: boolean;
  onPointerDown: (e: React.PointerEvent, kind: ClipDrag["kind"]) => void;
  onSelect: () => void;
}) {
  const t = useT();
  const span = itemSpan(item);
  // Live preview of the active drag.
  let start = span.start_ms;
  let end = span.end_ms;
  if (drag) {
    if (drag.kind === "move") {
      start = drag.origStart + drag.deltaMs;
      end = start + (span.end_ms - span.start_ms);
    } else if (drag.kind === "resize-start") {
      start = drag.origStart + drag.deltaMs;
    } else {
      end = span.end_ms + drag.deltaMs;
    }
  }
  const left = tl.msToX(start, view);
  const width = Math.max(2, (end - start) * view.pxPerMs);
  const edgePx = tl.edgeHitWidthPx(width);
  const label =
    item.text?.text || media?.original_filename || media?.path || item.kind;
  const title = missing ? `${label} — ${t("timelineClipMissingMedia")}` : label;
  // A dimmed source-frame backdrop behind the label for video clips. Extraction
  // is memoized per media id and no-ops outside Tauri (browser/e2e keeps the
  // text-only look). Skipped entirely once the source is known missing —
  // there's nothing on disk for ffmpeg to grab a frame from.
  const thumb = useThumbnail(
    !missing && media?.kind === "video" ? media : undefined,
  );
  // Multi-frame filmstrip backdrop (E3-UI), addressed on the fixed per-zoom-
  // tier grid so panning/zooming reuse already-rendered tiles. Suppressed
  // while THIS clip is being dragged — the box is sliding but `item` (what
  // the tile geometry is keyed to) hasn't committed its new position yet, so
  // tiles would visibly lag the ghost. `thumb` above covers that gap and the
  // off-Tauri/non-video/no-tiles-yet/missing-media cases (the hook resolves
  // empty there).
  const [visStartMs, visEndMs] = tl.visibleRange(view);
  const filmstripTiles = useFilmstripTiles(
    !drag && !missing && media?.kind === "video" ? media : undefined,
    item,
    view.pxPerMs,
    Math.max(span.start_ms, visStartMs),
    Math.min(span.end_ms, visEndMs),
  );
  return (
    <div
      className={cn(
        "absolute top-1 bottom-1 overflow-hidden rounded bg-[var(--color-accent-600)]/25 text-[var(--text-ui-xs)]",
        selected
          ? "ring-2 ring-[var(--color-accent-500)]"
          : "border border-[var(--color-accent-500)]/40",
        missing &&
          "border-dashed border-[var(--color-danger,#b3261e)] bg-[var(--color-danger,#b3261e)]/15",
        drag && "opacity-80",
      )}
      style={{ left, width }}
      onPointerDown={(e) => !locked && onPointerDown(e, "move")}
      onClick={onSelect}
      title={title}
    >
      {filmstripTiles.length > 0
        ? filmstripTiles.map((tile) => (
            <img
              key={tile.key}
              src={tile.url}
              alt=""
              aria-hidden="true"
              draggable={false}
              className={cn(
                "pointer-events-none absolute inset-y-0 h-full",
                tile.stale ? "opacity-25" : "opacity-40",
              )}
              style={{ left: tile.leftPx, width: tile.widthPx }}
            />
          ))
        : thumb && (
            <img
              src={thumb}
              alt=""
              aria-hidden="true"
              draggable={false}
              className="pointer-events-none absolute inset-0 h-full w-full object-cover opacity-40"
            />
          )}
      {missing && (
        <AlertTriangle
          size={12}
          data-testid="clip-missing-badge"
          className="pointer-events-none absolute right-1 top-1 z-10 text-[var(--color-danger,#b3261e)]"
          aria-hidden="true"
        />
      )}
      {!locked && (
        <>
          <span
            className="absolute inset-y-0 left-0 z-10 cursor-ew-resize hover:bg-[var(--color-accent-500)]/60"
            style={{ width: edgePx }}
            onPointerDown={(e) => onPointerDown(e, "resize-start")}
          />
          <span
            className="absolute inset-y-0 right-0 z-10 cursor-ew-resize hover:bg-[var(--color-accent-500)]/60"
            style={{ width: edgePx }}
            onPointerDown={(e) => onPointerDown(e, "resize-end")}
          />
        </>
      )}
      <div className="relative truncate px-2 py-1 text-[var(--color-fg)]">
        {label}
        {speed !== 1 && (
          <span className="ml-1 text-[var(--color-accent-300)]">{speed}×</span>
        )}
      </div>
    </div>
  );
}
