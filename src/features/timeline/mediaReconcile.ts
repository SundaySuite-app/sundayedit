/**
 * Media reconcile policy — decides what every preview media element should be
 * doing this frame, as a pure function returning a list of actions.
 *
 * ── Provenance ──────────────────────────────────────────────────────────────
 * The decide/execute *shape* is ported from Clypra (MIT):
 *   repo    https://github.com/AIEraDev/Clypra
 *   path    src/core/playback/PreviewPlaybackScheduler.ts
 *   commit  2e85676f0c56d1e5f28fabcd9a3ab9952442a35b (2026-08-06)
 *   license MIT © Clypra Contributors — full text in THIRD-PARTY-NOTICES.md
 *
 * What was lifted is the architecture, not the numbers: **this module DECIDES,
 * the caller EXECUTES.** Nothing here touches a `<video>`; it reads a snapshot
 * of each element and returns `MediaAction[]`. That boundary is what makes
 * multi-clip preview testable at all — the seek/play/pause policy is the part
 * that goes subtly wrong, and here it runs headless in a decision table.
 * Their ±2% `playbackRate` nudge for sub-frame drift is lifted wholesale; it is
 * a genuinely good idea (correct a small error by bending time instead of
 * cutting it, which no viewer can see and no seek storm can follow).
 *
 * ── Our tolerances, NOT theirs (the deliberate divergence) ─────────────────
 * Clypra tolerates 0.5–2 s of A/V drift and rate-limits corrective seeks to
 * one per 400 ms (1.5 s on audio-bearing elements). At 30 fps that is 15–60
 * frames out of sync — visible lip-sync failure, and unusable for the frame-
 * exact work SundayEdit does. Our budget is **one frame at the project fps**
 * (33.3 ms at 30 fps) while playing, and **half a frame while paused** (past
 * half a frame the element would present a different frame than the playhead
 * names). Both are ≤ one frame, playing and paused.
 *
 * Dropping their time-based rate limits is safe because we replace them with a
 * *state*-based guard: while an element reports `seeking`, we issue no further
 * seek to it. A seek storm is impossible not because a stopwatch forbids it,
 * but because the element itself says it is still busy. This also removes their
 * worst failure mode — a 1.5 s lockout during which drift is left uncorrected.
 *
 * Further divergences:
 * - **The rate nudge is allowed on audio-bearing elements.** Clypra excludes
 *   them, fearing artefacts. A ±2% rate change is inaudible with the browser
 *   default `preservesPitch`, whereas the corrective seek it forces instead is
 *   an audible dropout. Cheaper correction on the element that needs it most.
 * - **Milliseconds throughout.** Clypra is in seconds; SundayEdit is ms end to
 *   end (`playheadMs`, `in_ms`/`out_ms`, the Rust model). Element times are
 *   seconds only at the DOM boundary — the executor divides by 1000 when it
 *   writes `element.currentTime`, and multiplies when it snapshots. Same single
 *   decision as `playbackClock.ts`.
 * - **Signed transport rate.** A browser `<video>` cannot play backwards, and
 *   shuttle is realised as per-frame seeks, so only forward realtime transport
 *   lets an element drive itself — the rule `mediaSync.intentFor` already
 *   encodes. Reverse/shuttle/paused all land in the "pin the frame" branch.
 * - **Transitions and prewarm targets are computed by the caller**, not here:
 *   the signature is `(timeline, snapshots) → actions`, so each snapshot
 *   carries its already-resolved `targetTimeMs` (see `previewMap.ts`).
 *
 * ── Relationship to mediaSync.ts ────────────────────────────────────────────
 * Programme stage E2 made this the policy MediaPlayer executes: it snapshots
 * its single `<video>`, calls `reconcileMedia`, and applies the actions.
 * `mediaSync.ts` keeps only `isUserGesture` (the gesture-window rule, which is
 * not a reconcile decision at all) in production; its `intentFor`/`reconcile`/
 * `snapToFrameSec` remain exported and tested as the superseded single-epsilon
 * reconciler. The signature here is already the multi-element one
 * (`snapshots` is a list), so the source-keyed element POOL that backs true
 * multi-clip preview needs no change to this module — only a caller that
 * hands it more than one snapshot.
 */

/** Why an action was issued — kept on every action for logs and tests. */
export type MediaActionReason =
  /** Automatic correction of accumulated drift. */
  | "drift-recovery"
  /** Drift so large it can only be a jump: scrub, shuttle, tab throttling. */
  | "scrub"
  /** A transport change: play, pause, or a deliberate playhead jump. */
  | "transport"
  /** First seek into a clip that just came under the playhead. */
  | "clip-enter"
  /** The playhead left this clip. */
  | "clip-exit"
  /** Parking an upcoming clip on its first frame before it is needed. */
  | "prewarm"
  /** Restoring the element's nominal rate once it is back in sync. */
  | "rate-restore"
  /** The clip's gain, its track's fader, or the active clip itself changed
   *  (R2 audio). */
  | "volume-change";

/** One mutation for the executor to apply to one element. */
export interface MediaAction {
  type: "seek" | "play" | "pause" | "setRate" | "setVolume";
  /** The timeline item (clip) whose element this is. */
  itemId: string;
  /** Target source time, ms. Set for `seek`. */
  timeMs?: number;
  /** Target `playbackRate`. Set for `setRate`. */
  rate?: number;
  /** Target `HTMLMediaElement.volume`, already dB→linear and clamped to
   *  `[0, 1]` by the caller. Set for `setVolume`. */
  volume?: number;
  reason: MediaActionReason;
}

/** What the timeline wants, this frame. */
export interface TimelineSyncState {
  /** Playhead, ms. */
  playheadMs: number;
  /** Signed transport rate: 0 stopped, 1 realtime, <0 reverse, |r|>1 shuttle. */
  rate: number;
  /** Project frame rate — the drift budget is derived from it. */
  fps: number;
}

/** What one media element is doing, as read by the executor. */
export interface MediaElementSnapshot {
  /** The timeline item this element is bound to. */
  itemId: string;
  /**
   * Source time this element should show for the current playhead, ms, or
   * `null` when the playhead is not inside the clip. Resolved by the caller
   * (`previewMap.sourceTimeSec` × 1000) so this module stays a pure policy.
   */
  targetTimeMs: number | null;
  /** The element's `currentTime`, ms. */
  currentTimeMs: number;
  paused: boolean;
  /** The element's `seeking` flag — our seek-storm guard. */
  seeking: boolean;
  /** The element's `readyState` (0 nothing … 4 enough data). */
  readyState: number;
  /** The element's `playbackRate`. */
  playbackRate: number;
  /** The clip's speed multiplier (`TimelineItem.speed`). Defaults to 1. */
  speed?: number;
  /** The element's current `.volume` (0..1). Defaults to 1 when omitted, so a
   *  caller that doesn't track gain at all (legacy single-source mode) never
   *  manufactures a spurious correction. */
  volume?: number;
  /**
   * The volume this element SHOULD be playing at — already dB→linear and
   * clamped to `[0, 1]` by the caller (`previewVolumeFor`, R2's dB math is
   * not this module's job). `undefined` means "don't manage volume for this
   * element" (no per-clip gain to honour).
   */
  targetVolume?: number;
  /** Whether this element has ever been seeked (browsers decode lazily). */
  hasBeenSeeked?: boolean;
  /**
   * Where to park an inactive element so it is ready when its clip arrives,
   * ms of source time. `null`/omitted disables prewarming for this element.
   */
  prewarmTimeMs?: number | null;
}

export interface ReconcileConfig {
  /** Drift budget while playing, ms. Default: one frame at the project fps. */
  playingBudgetMs: number;
  /** Drift budget while pinned (paused/scrub), ms. Default: half a frame. */
  pausedBudgetMs: number;
  /** Below this drift the element counts as in sync, ms. Default: ¼ frame. */
  deadBandMs: number;
  /** Above this drift a correction is reported as a jump, not drift, ms. */
  scrubThresholdMs: number;
  /** Rate multiplier applied when the element is behind. */
  nudgeUp: number;
  /** Rate multiplier applied when the element is ahead. */
  nudgeDown: number;
  /** Ignore `playbackRate` differences smaller than this. */
  rateEpsilon: number;
  /** Ignore `.volume` differences smaller than this. */
  volumeEpsilon: number;
  /** Slowest `playbackRate` a browser plays cleanly. */
  minNativeRate: number;
  /** Fastest `playbackRate` a browser plays cleanly. */
  maxNativeRate: number;
}

/** `HTMLMediaElement.HAVE_METADATA` — below this, seeking is meaningless. */
const READY_METADATA = 1;

const DEFAULT_FPS = 30;

/** One frame at `fps`, in ms. Falls back to 30 fps for a nonsense fps. */
export function frameBudgetMs(fps: number): number {
  const safe = typeof fps === "number" && fps > 0 ? fps : DEFAULT_FPS;
  return 1000 / safe;
}

/**
 * The tolerances for a project frame rate. Everything is derived from one
 * frame so there is a single number to reason about when fps changes.
 */
export function defaultReconcileConfig(fps: number): ReconcileConfig {
  const frameMs = frameBudgetMs(fps);
  return {
    playingBudgetMs: frameMs,
    // Half a frame is where rounding starts naming a different frame, so a
    // pinned element must be tighter than the playing budget.
    pausedBudgetMs: frameMs / 2,
    deadBandMs: frameMs / 4,
    // ~15 frames: past this it is a jump (scrub/shuttle/throttled tab), not
    // drift. Only the reason tag differs — the seek is issued either way.
    scrubThresholdMs: frameMs * 15,
    nudgeUp: 1.02,
    nudgeDown: 0.98,
    rateEpsilon: 0.001,
    // Finer than the ~1/255 step a UI slider or the element itself quantises
    // to, so a real change is never missed and a no-op tick never re-issues.
    volumeEpsilon: 0.002,
    // A browser plays cleanly across this band; outside it we scrub by seeking.
    minNativeRate: 0.25,
    maxNativeRate: 4,
  };
}

/**
 * Decide what to do with every preview element this frame.
 *
 * Pure: same inputs, same actions, no clocks and no DOM. Actions are returned
 * in element order, and per element in apply order (seek before play, pause
 * before pin) so an executor can run the list straight through.
 */
export function reconcileMedia(
  timeline: TimelineSyncState,
  snapshots: readonly MediaElementSnapshot[],
  overrides: Partial<ReconcileConfig> = {},
): MediaAction[] {
  const config = { ...defaultReconcileConfig(timeline.fps), ...overrides };
  const actions: MediaAction[] = [];
  for (const snapshot of snapshots) {
    reconcileElement(timeline, snapshot, config, actions);
  }
  return actions;
}

function reconcileElement(
  timeline: TimelineSyncState,
  element: MediaElementSnapshot,
  config: ReconcileConfig,
  actions: MediaAction[],
): void {
  const { itemId } = element;
  const canSeek = !element.seeking && element.readyState >= READY_METADATA;

  // ── Volume (R2 audio) ─────────────────────────────────────────────────────
  // Checked first and unconditionally — it applies whether the element is
  // active, prewarming, or paused between clips, and never competes with the
  // seek/play/pause decisions below (a real `<video>` accepts a volume change
  // regardless of playback state, so there is no ordering hazard to resolve).
  if (element.targetVolume !== undefined) {
    const currentVolume = element.volume ?? 1;
    if (Math.abs(currentVolume - element.targetVolume) > config.volumeEpsilon) {
      actions.push({
        type: "setVolume",
        itemId,
        volume: element.targetVolume,
        reason: "volume-change",
      });
    }
  }

  // ── Inactive: park it (prewarm) and make sure it is quiet ────────────────
  if (element.targetTimeMs === null) {
    if (!element.paused) {
      actions.push({ type: "pause", itemId, reason: "clip-exit" });
    }
    const prewarm = element.prewarmTimeMs;
    if (
      prewarm !== null &&
      prewarm !== undefined &&
      canSeek &&
      Math.abs(element.currentTimeMs - prewarm) > config.pausedBudgetMs
    ) {
      actions.push({
        type: "seek",
        itemId,
        timeMs: prewarm,
        reason: "prewarm",
      });
    }
    return;
  }

  const target = element.targetTimeMs;
  // Signed: negative means the element sits *behind* the playhead.
  const drift = element.currentTimeMs - target;
  const absDrift = Math.abs(drift);
  const speed = element.speed !== undefined ? element.speed : 1;
  // What `playbackRate` this element needs to track the timeline: the clip's
  // own speed times the transport rate (which is 1 whenever we play natively).
  const nominalRate = speed * Math.abs(timeline.rate);

  // A browser <video> only ever plays forward at a sane rate; reverse, shuttle
  // and stopped all pin the frame by seeking instead (as `mediaSync` does).
  const canPlayNatively =
    timeline.rate === 1 &&
    nominalRate >= config.minNativeRate &&
    nominalRate <= config.maxNativeRate;

  if (!canPlayNatively) {
    if (!element.paused) {
      actions.push({ type: "pause", itemId, reason: "transport" });
    }
    pinFrame(timeline, element, config, target, absDrift, canSeek, actions);
    return;
  }

  if (element.paused) {
    // Land on the right frame before rolling, so playback starts in sync.
    if (canSeek && absDrift > config.playingBudgetMs) {
      actions.push({
        type: "seek",
        itemId,
        timeMs: target,
        reason:
          element.hasBeenSeeked === true ? "drift-recovery" : "clip-enter",
      });
    }
    actions.push({ type: "play", itemId, reason: "transport" });
    return;
  }

  // Playing. While a seek is in flight the element's clock is meaningless —
  // wait for it rather than piling on corrections (our seek-storm guard).
  if (element.seeking) return;

  if (absDrift > config.playingBudgetMs) {
    actions.push({
      type: "seek",
      itemId,
      timeMs: target,
      reason: absDrift > config.scrubThresholdMs ? "scrub" : "drift-recovery",
    });
    return;
  }

  if (absDrift > config.deadBandMs) {
    // Sub-frame drift: bend time instead of cutting it. Behind → speed up.
    const nudged =
      nominalRate * (drift < 0 ? config.nudgeUp : config.nudgeDown);
    if (Math.abs(element.playbackRate - nudged) > config.rateEpsilon) {
      actions.push({
        type: "setRate",
        itemId,
        rate: nudged,
        reason: "drift-recovery",
      });
    }
    return;
  }

  // In sync — undo any nudge still standing.
  if (Math.abs(element.playbackRate - nominalRate) > config.rateEpsilon) {
    actions.push({
      type: "setRate",
      itemId,
      rate: nominalRate,
      reason: "rate-restore",
    });
  }
}

/**
 * Keep a non-playing element's frame pinned under the playhead. Also forces
 * the very first seek even when drift is nil: browsers do not decode and
 * present a frame until something seeks the element.
 */
function pinFrame(
  timeline: TimelineSyncState,
  element: MediaElementSnapshot,
  config: ReconcileConfig,
  target: number,
  absDrift: number,
  canSeek: boolean,
  actions: MediaAction[],
): void {
  if (!canSeek) return;

  const needsFirstFrame = element.hasBeenSeeked !== true;
  if (!needsFirstFrame && absDrift <= config.pausedBudgetMs) return;

  let reason: MediaActionReason;
  if (needsFirstFrame) {
    reason = "clip-enter";
  } else if (absDrift > config.scrubThresholdMs) {
    reason = "scrub";
  } else {
    reason = timeline.rate === 0 ? "transport" : "scrub";
  }

  actions.push({
    type: "seek",
    itemId: element.itemId,
    timeMs: target,
    reason,
  });
}
