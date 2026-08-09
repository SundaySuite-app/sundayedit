/**
 * NLE timeline (Task 2D) — the multi-lane spatial canvas of the editor.
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
 * Space; an internal playhead clock advances by the signed rate. Drags snap to
 * neighbouring edges, the playhead and the bounds (S toggles snapping). Media
 * dragged from the bin drops onto a lane to become a new clip.
 */

import {
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
  Lock,
  Unlock,
  Headphones,
  ChevronUp,
  ChevronDown,
  Clapperboard,
  Loader2,
  RotateCcw,
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
  trackAtY,
  timelineDurationMs,
} from "./laneLayout";
import { MediaPlayer } from "./MediaPlayer";
import { renderPreviewProxy } from "@/lib/composeEngine";
import { MEDIA_DND_MIME } from "@/features/media/MediaBin";

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
const GUTTER_W = 150;

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
  const [playheadMs, setPlayheadMs] = useState(0);
  // Signed playback rate (J/K/L shuttle): <0 reverse, 0 stopped, 1 realtime.
  const [rate, setRate] = useState(0);
  const playing = rate !== 0;
  const [snapEnabled, setSnapEnabled] = useState(true);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [selectedClipId, setSelectedClipId] = useState<string | null>(null);
  const [drag, setDrag] = useState<CaptionDrag | null>(null);
  const [clipDrag, setClipDrag] = useState<ClipDrag | null>(null);
  const [dropTrackId, setDropTrackId] = useState<string | null>(null);
  const [waveform, setWaveform] = useState<WaveformData | null>(null);
  // A transient warning when the user scrubs the native video control while the
  // timeline is driving playback (the two clocks would fight).
  const [scrubWarning, setScrubWarning] = useState(false);
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

  // Internal playback clock — advances the playhead by the shuttle `rate`
  // (direction + speed) in real time. Stops cleanly at either bound.
  useEffect(() => {
    if (rate === 0) return;
    let raf = 0;
    let last = performance.now();
    const tick = (now: number) => {
      const dt = now - last;
      last = now;
      setPlayheadMs((p) => {
        const next = p + dt * rate;
        if (next <= 0) {
          setRate(0);
          return 0;
        }
        if (next >= durationMs) {
          setRate(0);
          return durationMs;
        }
        return next;
      });
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [rate, durationMs]);

  // Auto-dismiss the scrub-conflict warning a few seconds after it appears.
  useEffect(() => {
    if (!scrubWarning) return;
    const id = setTimeout(() => setScrubWarning(false), 4000);
    return () => clearTimeout(id);
  }, [scrubWarning]);

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
  function seekToX(clientX: number) {
    const el = viewportRef.current;
    if (!el) return;
    const x = clientX - el.getBoundingClientRect().left;
    const ms = tl.snapToFrame(
      Math.max(0, Math.min(durationMs, tl.xToMs(x, view))),
      fps,
    );
    setPlayheadMs(ms);
  }

  /** ms under a client X, clamped to the timeline + frame-snapped. */
  function clientXToMs(clientX: number): number {
    const el = viewportRef.current;
    if (!el) return 0;
    const x = clientX - el.getBoundingClientRect().left;
    return tl.snapToFrame(
      Math.max(0, Math.min(durationMs, tl.xToMs(x, view))),
      fps,
    );
  }

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

  function zoomButton(factor: number) {
    setView((v) => {
      const z = tl.zoomAround(v, factor, v.widthPx / 2);
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
  function onCaptionPointerDown(
    e: React.PointerEvent,
    c: Caption,
    kind: CaptionDrag["kind"],
  ) {
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
  }

  // ── clip drag (move across tracks / trim) — preview locally, commit on release
  function onClipPointerDown(
    e: React.PointerEvent,
    track: Track,
    ti: TimelineItem,
    kind: ClipDrag["kind"],
  ) {
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
  }

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
    let targetTrackId = clipDrag.trackId;
    if (clipDrag.kind === "move" && lanesScrollRef.current) {
      const rect = lanesScrollRef.current.getBoundingClientRect();
      const y = e.clientY - rect.top + lanesScrollRef.current.scrollTop;
      const hit = trackAtY(y, project.tracks, LANE_H);
      if (hit && !hit.locked && hit.kind === clipDrag.trackKind) {
        targetTrackId = hit.id;
      }
    }
    setDropTrackId(clipDrag.kind === "move" ? targetTrackId : null);
    setClipDrag({ ...clipDrag, deltaMs, targetTrackId });
  }

  async function onPointerUp() {
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
    setDrag(null);
    setClipDrag(null);
    setDropTrackId(null);
  }

  // Drop a media row from the bin onto a lane → place it as a clip.
  async function onLaneDrop(e: React.DragEvent, track: Track) {
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
  }

  function onLaneDragOver(e: React.DragEvent, track: Track) {
    if (track.locked || track.kind === "caption") return;
    if (!e.dataTransfer.types.includes(MEDIA_DND_MIME)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
    if (dropTrackId !== track.id) setDropTrackId(track.id);
  }

  function selectAndSeek(c: Caption) {
    setSelectedId(c.id);
    selectClip(null);
    setPlayheadMs(c.start_ms);
  }

  function step(dir: -1 | 1, count: number) {
    const idx = captions.findIndex((c) => c.id === selectedId);
    if (idx === -1) {
      setPlayheadMs((p) =>
        tl.snapToFrame(
          Math.max(0, Math.min(durationMs, p + ((dir * 1000) / fps) * count)),
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
    // Modified chords (Cmd+K command palette, Cmd+S save, menu accelerators)
    // belong to app-level handlers — never treat them as timeline shortcuts.
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const lower = e.key.toLowerCase();
    if (lower === "j" || lower === "k" || lower === "l") {
      e.preventDefault();
      setRate((r) => tl.shuttleRate(r, lower));
      return;
    }
    if (lower === "s") {
      e.preventDefault();
      setSnapEnabled((s) => !s);
      return;
    }
    switch (e.key) {
      case " ":
        e.preventDefault();
        setRate((r) => (r !== 0 ? 0 : 1));
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

  // Toggle a track flag through the shared undo stack.
  function toggleFlag(track: Track, flag: "muted" | "solo" | "locked") {
    void run((p) =>
      ipc.timeline.setTrackFlags(p, track.id, { [flag]: !track[flag] }),
    ).catch(() => {});
  }

  // Move a lane up (toward the top = higher index) or down.
  function reorder(track: Track, dir: -1 | 1) {
    const next = track.index + dir;
    if (next < 0) return;
    void run((p) => ipc.timeline.reorderTrack(p, track.id, next)).catch(
      () => {},
    );
  }

  // ── preview render (proxy) ─────────────────────────────────────────────────
  // Flatten the timeline to a temp file and load it into the preview so the user
  // sees the true composite (transitions/overlays/PiP). Best-effort: off-Tauri /
  // if the compose engine can't run, it silently returns to the live preview.
  async function renderPreview() {
    if (!isTauri()) return;
    setPreviewState("rendering");
    try {
      const out = await join(await appCacheDir(), "sundayedit-preview.mp4");
      const ok = await renderPreviewProxy(project, out);
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
  const [visStart, visEnd] = tl.visibleRange(view);
  const visibleCaptionRows = tl.visibleCaptions(captions, visStart, visEnd);
  const ticks = tl.rulerTicks(view, 80);
  const playheadX = tl.msToX(playheadMs, view);

  return (
    <div
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
      </div>

      {/* Toolbar */}
      <div className="flex items-center gap-2 border-b border-[var(--color-border)] px-3 py-1.5">
        <button
          type="button"
          onClick={() => setRate((r) => (r !== 0 ? 0 : 1))}
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
            {stacked.map((track, i) => (
              <TrackHeader
                key={track.id}
                track={track}
                height={LANE_H}
                canMoveUp={i > 0}
                canMoveDown={i < stacked.length - 1}
                onToggle={(flag) => toggleFlag(track, flag)}
                onMove={(dir) => reorder(track, dir)}
                labels={{
                  mute: t("trackMute"),
                  solo: t("trackSolo"),
                  lock: t("trackLock"),
                  up: t("trackMoveUp"),
                  down: t("trackMoveDown"),
                }}
              />
            ))}
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
          <div
            className="relative border-b border-[var(--color-border)] bg-[var(--color-bg-elevated)]"
            style={{ height: RULER_H }}
            onPointerDown={(e) => seekToX(e.clientX)}
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
                onDragLeave={() =>
                  dropTrackId === track.id && setDropTrackId(null)
                }
                onDrop={(e) => onLaneDrop(e, track)}
                onPointerDown={(e) => {
                  seekToX(e.clientX);
                  setSelectedId(null);
                  selectClip(null);
                }}
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
                        onPointerDown={(e, kind) =>
                          onCaptionPointerDown(e, c, kind)
                        }
                        onSelect={() => selectAndSeek(c)}
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
                          view={view}
                          speed={Math.max(0.01, span.ti.speed)}
                          drag={clipDrag?.id === span.ti.id ? clipDrag : null}
                          selected={selectedClipId === span.ti.id}
                          locked={track.locked}
                          onPointerDown={(e, kind) =>
                            onClipPointerDown(e, track, span.ti, kind)
                          }
                          onSelect={() => {
                            selectClip(span.ti.id);
                            setSelectedId(null);
                            setPlayheadMs(span.ti.timeline_start_ms);
                          }}
                        />
                      ))}
              </div>
            ))}
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

function TrackHeader({
  track,
  height,
  canMoveUp,
  canMoveDown,
  onToggle,
  onMove,
  labels,
}: {
  track: Track;
  height: number;
  canMoveUp: boolean;
  canMoveDown: boolean;
  onToggle: (flag: "muted" | "solo" | "locked") => void;
  onMove: (dir: -1 | 1) => void;
  labels: {
    mute: string;
    solo: string;
    lock: string;
    up: string;
    down: string;
  };
}) {
  const audible = track.kind === "video" || track.kind === "audio";
  return (
    <div
      className="flex items-center gap-1 border-t border-[var(--color-border)] px-2"
      style={{ height }}
    >
      <div className="flex min-w-0 flex-1 flex-col justify-center">
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
          onClick={() => onMove(1)}
          title={labels.up}
          aria-label={labels.up}
          className="grid h-3.5 w-4 place-items-center text-[var(--color-fg-subtle)] hover:text-[var(--color-fg)] disabled:opacity-30"
        >
          <ChevronUp size={11} />
        </button>
        <button
          type="button"
          disabled={!canMoveDown}
          onClick={() => onMove(-1)}
          title={labels.down}
          aria-label={labels.down}
          className="grid h-3.5 w-4 place-items-center text-[var(--color-fg-subtle)] hover:text-[var(--color-fg)] disabled:opacity-30"
        >
          <ChevronDown size={11} />
        </button>
      </div>
      {audible && (
        <>
          <FlagToggle
            active={track.muted}
            onClick={() => onToggle("muted")}
            label={labels.mute}
            on={<VolumeX size={13} />}
            off={<Volume2 size={13} />}
            activeClass="text-[var(--color-danger)]"
          />
          <FlagToggle
            active={track.solo}
            onClick={() => onToggle("solo")}
            label={labels.solo}
            on={<Headphones size={13} />}
            off={<Headphones size={13} />}
            activeClass="text-[var(--color-accent-400)]"
          />
        </>
      )}
      <FlagToggle
        active={track.locked}
        onClick={() => onToggle("locked")}
        label={labels.lock}
        on={<Lock size={13} />}
        off={<Unlock size={13} />}
        activeClass="text-[var(--color-warning)]"
      />
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
}: {
  active: boolean;
  onClick: () => void;
  label: string;
  on: React.ReactNode;
  off: React.ReactNode;
  activeClass: string;
}) {
  return (
    <button
      type="button"
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
            className="absolute inset-y-0 left-0 w-1.5 cursor-ew-resize hover:bg-[var(--color-accent-500)]/40"
            onPointerDown={(e) => onPointerDown(e, "resize-start")}
          />
          <span
            className="absolute inset-y-0 right-0 w-1.5 cursor-ew-resize hover:bg-[var(--color-accent-500)]/40"
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
  view: tl.TimelineView;
  speed: number;
  drag: ClipDrag | null;
  selected: boolean;
  locked: boolean;
  onPointerDown: (e: React.PointerEvent, kind: ClipDrag["kind"]) => void;
  onSelect: () => void;
}) {
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
  const label =
    item.text?.text || media?.original_filename || media?.path || item.kind;
  return (
    <div
      className={cn(
        "absolute top-1 bottom-1 overflow-hidden rounded bg-[var(--color-accent-600)]/25 text-[var(--text-ui-xs)]",
        selected
          ? "ring-2 ring-[var(--color-accent-500)]"
          : "border border-[var(--color-accent-500)]/40",
        drag && "opacity-80",
      )}
      style={{ left, width }}
      onPointerDown={(e) => !locked && onPointerDown(e, "move")}
      onClick={onSelect}
      title={label}
    >
      {!locked && (
        <>
          <span
            className="absolute inset-y-0 left-0 z-10 w-1.5 cursor-ew-resize hover:bg-[var(--color-accent-500)]/60"
            onPointerDown={(e) => onPointerDown(e, "resize-start")}
          />
          <span
            className="absolute inset-y-0 right-0 z-10 w-1.5 cursor-ew-resize hover:bg-[var(--color-accent-500)]/60"
            onPointerDown={(e) => onPointerDown(e, "resize-end")}
          />
        </>
      )}
      <div className="truncate px-2 py-1 text-[var(--color-fg)]">
        {label}
        {speed !== 1 && (
          <span className="ml-1 text-[var(--color-accent-300)]">{speed}×</span>
        )}
      </div>
    </div>
  );
}
