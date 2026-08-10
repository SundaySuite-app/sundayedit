/**
 * MediaPlayer (Phase 1.3 follow-up) — binds a real <video> to the timeline's
 * playhead clock so the user can watch while transcribing/editing captions.
 *
 * The timeline stays the source of truth: it owns `playheadMs` and the signed
 * shuttle `rate`. This component is a thin **executor** — every animation frame
 * it snapshots the element, asks the pure `reconcileMedia` policy what should
 * happen, and applies the returned `MediaAction[]`. Reverse and fast/slow
 * shuttle can't be done by a browser <video>, so those states scrub the element
 * frame-by-frame via seeks (it stays paused and just follows the playhead).
 *
 * ── Why `mediaReconcile` and not `mediaSync` (programme stage E2) ───────────
 * `mediaSync.intentFor`/`reconcile` decided seek-or-not from a single ±1-frame
 * epsilon and had no notion of a seek already in flight, so a slow decoder
 * could be handed a fresh `currentTime` every frame while it was still serving
 * the last one. `mediaReconcile` keeps the same one-frame budget but adds the
 * two things that were missing: a **seek-storm guard** (never issue a seek to
 * an element that reports `seeking`) and a **sub-frame rate nudge** (±2 % on
 * `playbackRate` corrects drift smaller than a frame by bending time instead of
 * cutting it — no visible jump, no decode restart). It also understands the
 * clip's `speed`, so a 2× clip now *plays* at 2× instead of being scrubbed
 * frame-by-frame. `mediaSync.isUserGesture` is still the gesture-window rule
 * and is still used below.
 *
 * Native transport controls are hidden, but a user can still scrub via the OS
 * media keys / picture-in-picture; if that happens during playback we raise a
 * warning rather than silently fighting them.
 */

import { lazy, Suspense, useEffect, useRef, useState } from "react";

import { convertFileSrc } from "@tauri-apps/api/core";

import type { Project } from "@/lib/bindings/Project";
import { useT } from "@/lib/i18n";
import { selectCompositorActive, useCompositorFlag } from "./compositor";
import { KaraokeOverlay } from "./KaraokeOverlay";
import { isUserGesture } from "./mediaSync";
import {
  reconcileMedia,
  type MediaAction,
  type MediaElementSnapshot,
} from "./mediaReconcile";
import { snapMsToFrame } from "./playbackClock";
import { activeVideoItem, sourceTimeSec } from "./previewMap";

interface Props {
  /**
   * Legacy single-source mode: an asset URL for the video (already run through
   * convertFileSrc by App). Used when no `project` timeline is supplied.
   */
  src?: string;
  /**
   * NLE multi-track mode: when supplied *and* it has `timeline_items`, the
   * preview picks the active video clip under the playhead each frame and drives
   * the element from that clip's source media instead of the `src` prop.
   */
  project?: Project;
  /**
   * Preview-render proxy: an already-composited file (asset URL) spanning the
   * whole timeline. When set it takes precedence over the per-clip NLE mapping —
   * the element plays this single flattened file against the timeline clock, so
   * the user sees the true composite. Cleared to return to the live per-clip
   * preview.
   */
  proxySrc?: string;
  /** Timeline playhead, ms — the position the element should show. */
  playheadMs: number;
  /** Signed shuttle rate: 0 stopped, <0 reverse, 1 realtime, doubling. */
  rate: number;
  /** Project duration, ms — the clamp bound when metadata isn't loaded yet. */
  durationMs: number;
  /** Frames per second, for snapping seeks to frame boundaries. */
  fps: number;
  /**
   * Preview quality ladder rung, as a percentage of full resolution (see
   * `previewQuality.ts`). 100 renders the surface at full size — anything less
   * composites a smaller surface and CSS-upscales it.
   */
  scalePct?: number;
  /** Raised when the user manually scrubs/pauses while the timeline plays. */
  onConflict?: () => void;
}

/**
 * The element's identity in the reconcile policy. There is exactly ONE preview
 * element today (ADR-009: HTML5 <video> now, a source-keyed pool once a real
 * compositor lands), so the id is only used to correlate actions with the
 * element that produced the snapshot.
 */
const PREVIEW_ITEM_ID = "preview";

/**
 * The GPU compositor (E6), loaded ONLY when the capability flag is on —
 * `pixi.js` is 157 kB min+gzip (ADR-010) and must not reach a default install.
 * `React.lazy` keeps the import out of this module's dependency graph until
 * the element is actually rendered.
 */
const PixiCompositor = lazy(() =>
  import("./compositor/PixiCompositor").then((m) => ({
    default: m.PixiCompositor,
  })),
);

/** `HTMLMediaElement.HAVE_METADATA` — below this, seeking is meaningless. */
const READY_METADATA = 1;

/**
 * Whether the element knows enough to be seeked. `readyState` is the canonical
 * gate, but a finite `duration` says the same thing (a browser only reports one
 * once metadata has arrived) — accepting either means an environment that
 * exposes one and not the other still gets a driven preview instead of a frozen
 * black frame.
 */
function metadataReadyState(video: HTMLVideoElement): number {
  if (video.readyState >= READY_METADATA) return video.readyState;
  return Number.isFinite(video.duration) ? READY_METADATA : video.readyState;
}

/**
 * Apply one decided action. Returns whether it is a transport mutation the
 * element will echo back as a `play`/`pause`/`seeking` event — those tag the
 * gesture window so we can tell our own mutations from the user's. A
 * `playbackRate` change fires `ratechange`, which we don't listen for.
 */
function applyAction(video: HTMLVideoElement, action: MediaAction): boolean {
  switch (action.type) {
    case "seek":
      if (action.timeMs !== undefined) video.currentTime = action.timeMs / 1000;
      return true;
    case "play":
      void video.play().catch(() => {});
      return true;
    case "pause":
      video.pause();
      return true;
    case "setRate":
      if (action.rate !== undefined) video.playbackRate = action.rate;
      return false;
  }
}

export function MediaPlayer({
  src,
  project,
  proxySrc,
  playheadMs,
  rate,
  durationMs,
  fps,
  scalePct = 100,
  onConflict,
}: Props) {
  const t = useT();
  const videoRef = useRef<HTMLVideoElement | null>(null);
  // Timestamp of our most recent programmatic mutation — lets event handlers
  // tell our own seeks/play/pause apart from the user's.
  const lastProgrammaticAtMs = useRef(0);
  // The video file may not be on disk (dev/demo) or may be an unsupported
  // codec — fall back to the timeline-only experience and say why. The flag is
  // cleared whenever the element's SOURCE changes (rAF clip swap, proxy load,
  // legacy src change) so a stale error never covers a healthy source.
  const [unavailable, setUnavailable] = useState(false);

  // ── GPU compositor (E6), default OFF ───────────────────────────────────────
  // `compositorRequested` is the flag (user setting AND nothing has
  // disqualified it this session); `compositorDrawing` only goes true once the
  // compositor has actually put a canvas up. Until then — and forever, with the
  // flag off — everything below is byte-for-byte the pre-E6 preview.
  const compositorRequested = useCompositorFlag(selectCompositorActive);
  const [compositorDrawing, setCompositorDrawing] = useState(false);
  // A flag turned off mid-session must not leave the element hidden behind a
  // canvas that is on its way out.
  useEffect(() => {
    if (!compositorRequested) setCompositorDrawing(false);
  }, [compositorRequested]);

  // NLE mode is active only when a project with placed clips is supplied AND no
  // preview proxy is loaded; else we fall back to the single-source element
  // (either the rendered proxy composite or the legacy single `src`).
  const singleSrc = proxySrc ?? src;
  const mappingMode =
    !proxySrc && !!(project && project.timeline_items.length > 0);

  // The media path currently loaded into the element (NLE mode). Swapping
  // <video>.src reloads decode, so we only reassign it when the path changes.
  const loadedMediaPath = useRef<string | null>(null);
  // Whether THIS source has been seeked yet. Browsers decode lazily: an element
  // that has never been seeked presents nothing, so the policy forces a first
  // seek even at zero drift. Every source swap starts over.
  const hasBeenSeeked = useRef(false);
  // Toggling the proxy on/off swaps the element between the single flattened
  // file and the per-clip NLE mapping. Forget the last-loaded clip path so the
  // NLE branch re-syncs the element the next frame after we return to live.
  useEffect(() => {
    loadedMediaPath.current = null;
    hasBeenSeeked.current = false;
  }, [proxySrc]);

  // A new single source (rendered proxy loaded/cleared, legacy src change)
  // invalidates any sticky error from the previous source.
  useEffect(() => {
    setUnavailable(false);
    hasBeenSeeked.current = false;
  }, [singleSrc]);

  // Reconcile the element to the playhead every frame. We read the latest props
  // off refs so the rAF loop doesn't restart on every playhead tick.
  const stateRef = useRef({
    playheadMs,
    rate,
    durationMs,
    fps,
    project,
    proxySrc,
  });
  stateRef.current = { playheadMs, rate, durationMs, fps, project, proxySrc };

  useEffect(() => {
    let raf = 0;

    /**
     * Resolve what this element should be showing this frame, as the snapshot
     * the pure policy consumes — or `null` when the element is between clips
     * and must simply hold its last frame.
     */
    const snapshotFor = (
      video: HTMLVideoElement,
    ): { snapshot: MediaElementSnapshot; rate: number } => {
      const s = stateRef.current;

      // Source time the element should show, the duration that bounds it, and
      // the clip's speed — the three things the two modes disagree about.
      let rawMs: number;
      let boundMs: number;
      let speed = 1;

      if (!s.proxySrc && s.project && s.project.timeline_items.length > 0) {
        // ── NLE multi-track: drive the active clip's source media. ──
        const active = activeVideoItem(s.project, s.playheadMs);
        if (!active) {
          // No clip under the playhead — hold the element paused (keep the
          // last-loaded frame; don't fight an empty gap by seeking). A null
          // target IS the policy's "inactive" branch; no prewarm target means
          // it only makes sure the element is quiet.
          return {
            rate: s.rate,
            snapshot: {
              itemId: PREVIEW_ITEM_ID,
              targetTimeMs: null,
              currentTimeMs: video.currentTime * 1000,
              paused: video.paused,
              seeking: video.seeking,
              readyState: metadataReadyState(video),
              playbackRate: video.playbackRate,
            },
          };
        }
        // Swap decode only when the active media path actually changes.
        if (loadedMediaPath.current !== active.media.path) {
          loadedMediaPath.current = active.media.path;
          hasBeenSeeked.current = false;
          video.src = convertFileSrc(active.media.path);
          // The error belonged to the previous clip's media — a fresh source
          // starts clean (its own onError re-raises if broken).
          setUnavailable(false);
        }
        // Map the playhead into this clip's source time; clamp/pace against the
        // media's OWN duration (this clip, not the whole timeline).
        rawMs = sourceTimeSec(active.item, s.playheadMs) * 1000;
        boundMs = active.media.duration_ms;
        speed = active.item.speed;
      } else {
        // ── Legacy single-source: element spans the whole timeline. ──
        rawMs = s.playheadMs;
        boundMs = s.durationMs;
      }

      // The timeline's duration is the authority for when playback ENDS (same
      // domain we export against). The element's own duration may disagree
      // slightly (probe metadata vs container length); it only clamps the seek
      // TARGET so we never seek past real footage. Keeping the two separate
      // stops the preview pausing early (element shorter) or stopping late with
      // footage left (element longer) relative to the timeline.
      const elementMs = Number.isFinite(video.duration)
        ? video.duration * 1000
        : boundMs;
      const seekBoundMs = Math.min(
        Math.max(0, boundMs),
        Math.max(0, elementMs),
      );
      const targetTimeMs = snapMsToFrame(
        Math.max(0, Math.min(seekBoundMs, rawMs)),
        s.fps,
      );

      // Past the end of its own domain the element must stop even though the
      // transport still says 1× — hand the policy rate 0 so it pins the frame
      // instead of rolling (`mediaSync.intentFor`'s `shouldPlay` rule).
      const transportRate =
        s.rate === 1 && rawMs >= Math.max(0, boundMs) ? 0 : s.rate;

      return {
        rate: transportRate,
        snapshot: {
          itemId: PREVIEW_ITEM_ID,
          targetTimeMs,
          currentTimeMs: video.currentTime * 1000,
          paused: video.paused,
          seeking: video.seeking,
          readyState: metadataReadyState(video),
          playbackRate: video.playbackRate,
          speed,
          hasBeenSeeked: hasBeenSeeked.current,
        },
      };
    };

    const tick = () => {
      const video = videoRef.current;
      if (video) {
        const s = stateRef.current;
        const { snapshot, rate: transportRate } = snapshotFor(video);
        const actions = reconcileMedia(
          { playheadMs: s.playheadMs, rate: transportRate, fps: s.fps },
          [snapshot],
        );
        for (const action of actions) {
          if (action.type === "seek") hasBeenSeeked.current = true;
          if (applyAction(video, action)) {
            lastProgrammaticAtMs.current = performance.now();
          }
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, []);

  // A play/pause/seeking event outside our programmatic window, while the
  // timeline is playing, is a conflicting manual gesture — warn the user.
  function onMaybeUserGesture() {
    if (stateRef.current.rate === 0) return;
    if (isUserGesture(performance.now(), lastProgrammaticAtMs.current)) {
      onConflict?.();
    }
  }

  // Preview quality ladder: below full scale the element is laid out in a box
  // that is `scalePct` of the stage and CSS-upscaled back over it, so the
  // browser composites (and resamples) fewer pixels per frame while the picture
  // keeps the same visual size. At 100 the wrapper carries no transform at all,
  // so a parked playhead is pixel-identical to an unladdered preview.
  const scaled = Number.isFinite(scalePct) && scalePct > 0 && scalePct < 100;
  const stageStyle = scaled
    ? {
        width: `${scalePct}%`,
        height: `${scalePct}%`,
        transform: `scale(${100 / scalePct})`,
        transformOrigin: "center",
      }
    : undefined;

  return (
    <div className="relative grid h-full place-items-center overflow-hidden">
      <div
        data-testid="preview-stage"
        data-preview-scale={scaled ? String(scalePct) : undefined}
        className="grid h-full w-full place-items-center overflow-hidden"
        style={stageStyle}
      >
        {/* This is a driven preview surface, not delivered media, so the
            <video> itself carries no <track>. The karaoke overlay below is a
            separate DOM layer sharing this grid cell (E4a) — off by default,
            and rendered only when the project's Style has karaoke enabled. */}
        <video
          ref={videoRef}
          // In NLE mode the rAF loop sets `.src` imperatively (per active clip),
          // so we leave the React attribute unbound to avoid clobbering it. In
          // single-source mode this is either the rendered proxy or the legacy src.
          src={mappingMode ? undefined : singleSrc}
          className="max-h-full max-w-full"
          // While the compositor draws, the element becomes a pure TEXTURE
          // SOURCE: taken out of flow and made transparent, but still laid out,
          // decoded and audible — `display:none` would let the browser stop
          // decoding, and there would be nothing to upload. With the flag off
          // this is `undefined`, i.e. no style attribute at all.
          style={
            compositorDrawing
              ? { position: "absolute", opacity: 0, pointerEvents: "none" }
              : undefined
          }
          // Timeline owns transport; the element is driven, not user-controlled.
          controls={false}
          muted={false}
          playsInline
          preload="auto"
          onError={() => setUnavailable(true)}
          // Any successful load dismisses a stale overlay (belt-and-braces for
          // paths that swap the source without going through the resets above).
          onLoadedData={() => setUnavailable(false)}
          onPlay={onMaybeUserGesture}
          onPause={onMaybeUserGesture}
          onSeeking={onMaybeUserGesture}
        />
        {/* E6: the GPU stage sits between the element and the caption overlay,
            so karaoke keeps rendering ON TOP of whichever surface is showing
            the picture. Nothing here exists with the flag off. */}
        {compositorRequested && (
          <Suspense fallback={null}>
            <PixiCompositor
              videoRef={videoRef}
              project={project}
              onActiveChange={setCompositorDrawing}
            />
          </Suspense>
        )}
        <KaraokeOverlay project={project} />
      </div>
      {unavailable && (
        <div
          className="pointer-events-none absolute inset-0 grid place-items-center bg-black/60 px-4 text-center text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]"
          role="status"
        >
          <span data-testid="media-unavailable">{t("mediaPlayerMissing")}</span>
        </div>
      )}
    </div>
  );
}
