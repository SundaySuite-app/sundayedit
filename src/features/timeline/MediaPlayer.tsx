/**
 * MediaPlayer (Phase 1.3 follow-up) — binds a real <video> to the timeline's
 * playhead clock so the user can watch while transcribing/editing captions.
 *
 * The timeline stays the source of truth: it owns `playheadMs` and the signed
 * shuttle `rate`. This component is a thin reconciler — every animation frame
 * it asks `mediaSync` what the element should be doing and applies only the
 * needed mutations (seek / play / pause). Reverse and fast/slow shuttle can't
 * be done by a browser <video>, so those states scrub the element frame-by-
 * frame via seeks (it stays paused and just follows the playhead).
 *
 * Native transport controls are hidden, but a user can still scrub via the OS
 * media keys / picture-in-picture; if that happens during playback we raise a
 * warning rather than silently fighting them.
 */

import { useEffect, useRef, useState } from "react";

import { convertFileSrc } from "@tauri-apps/api/core";

import type { Project } from "@/lib/bindings/Project";
import { useT } from "@/lib/i18n";
import { intentFor, reconcile, isUserGesture } from "./mediaSync";
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
  /** Raised when the user manually scrubs/pauses while the timeline plays. */
  onConflict?: () => void;
}

export function MediaPlayer({
  src,
  project,
  proxySrc,
  playheadMs,
  rate,
  durationMs,
  fps,
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

  // NLE mode is active only when a project with placed clips is supplied AND no
  // preview proxy is loaded; else we fall back to the single-source element
  // (either the rendered proxy composite or the legacy single `src`).
  const singleSrc = proxySrc ?? src;
  const mappingMode =
    !proxySrc && !!(project && project.timeline_items.length > 0);

  // The media path currently loaded into the element (NLE mode). Swapping
  // <video>.src reloads decode, so we only reassign it when the path changes.
  const loadedMediaPath = useRef<string | null>(null);
  // Toggling the proxy on/off swaps the element between the single flattened
  // file and the per-clip NLE mapping. Forget the last-loaded clip path so the
  // NLE branch re-syncs the element the next frame after we return to live.
  useEffect(() => {
    loadedMediaPath.current = null;
  }, [proxySrc]);

  // A new single source (rendered proxy loaded/cleared, legacy src change)
  // invalidates any sticky error from the previous source.
  useEffect(() => {
    setUnavailable(false);
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
    const tick = () => {
      const video = videoRef.current;
      if (video) {
        const s = stateRef.current;

        if (!s.proxySrc && s.project && s.project.timeline_items.length > 0) {
          // ── NLE multi-track: drive the active clip's source media. ──
          const active = activeVideoItem(s.project, s.playheadMs);
          if (!active) {
            // No clip under the playhead — hold the element paused (keep the
            // last-loaded frame; don't fight an empty gap by seeking).
            if (!video.paused) {
              lastProgrammaticAtMs.current = performance.now();
              video.pause();
            }
          } else {
            // Swap decode only when the active media path actually changes.
            if (loadedMediaPath.current !== active.media.path) {
              loadedMediaPath.current = active.media.path;
              video.src = convertFileSrc(active.media.path);
              // The error belonged to the previous clip's media — a fresh
              // source starts clean (its own onError re-raises if broken).
              setUnavailable(false);
            }
            // Map the playhead into this clip's source time; clamp/pace against
            // the media's OWN duration (this clip, not the whole timeline).
            const srcTimeMs = sourceTimeSec(active.item, s.playheadMs) * 1000;
            const mediaDurSec = active.media.duration_ms / 1000;
            const elementDurSec = Number.isFinite(video.duration)
              ? video.duration
              : mediaDurSec;
            const intent = intentFor(
              srcTimeMs,
              s.rate,
              mediaDurSec,
              s.fps,
              elementDurSec,
            );
            const step = reconcile(intent, {
              currentTimeSec: video.currentTime,
              paused: video.paused,
              durationSec: elementDurSec,
            });
            if (step.seekTo !== null || step.transport) {
              lastProgrammaticAtMs.current = performance.now();
            }
            if (step.seekTo !== null) video.currentTime = step.seekTo;
            if (step.transport === "play") void video.play().catch(() => {});
            else if (step.transport === "pause") video.pause();
          }
        } else {
          // ── Legacy single-source: element spans the whole timeline. ──
          // The timeline's durationMs is the authority for when playback ends
          // (same domain we export against). The element's own duration may
          // disagree slightly (probe metadata vs container length); pass it only
          // to clamp the seek target so we never seek past real footage. This
          // keeps the preview and the timeline clock stopping at the same end.
          const timelineDurSec = s.durationMs / 1000;
          const elementDurSec = Number.isFinite(video.duration)
            ? video.duration
            : timelineDurSec;
          const intent = intentFor(
            s.playheadMs,
            s.rate,
            timelineDurSec,
            s.fps,
            elementDurSec,
          );
          const step = reconcile(intent, {
            currentTimeSec: video.currentTime,
            paused: video.paused,
            durationSec: elementDurSec,
          });
          if (step.seekTo !== null || step.transport) {
            lastProgrammaticAtMs.current = performance.now();
          }
          if (step.seekTo !== null) video.currentTime = step.seekTo;
          if (step.transport === "play") void video.play().catch(() => {});
          else if (step.transport === "pause") video.pause();
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

  return (
    <div className="relative grid h-full place-items-center overflow-hidden">
      {/* Captions are the timeline's job; this is a driven preview surface,
          not delivered media, so it carries no <track>. */}
      <video
        ref={videoRef}
        // In NLE mode the rAF loop sets `.src` imperatively (per active clip),
        // so we leave the React attribute unbound to avoid clobbering it. In
        // single-source mode this is either the rendered proxy or the legacy src.
        src={mappingMode ? undefined : singleSrc}
        className="max-h-full max-w-full"
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
