/**
 * Timeline playback clock — a continuous, imperative time signal derived from
 * an audio clock rather than from `requestAnimationFrame` deltas.
 *
 * ── Provenance ──────────────────────────────────────────────────────────────
 * Ported from Clypra (MIT):
 *   repo    https://github.com/AIEraDev/Clypra
 *   path    src/core/playback/PlaybackClock.ts
 *   commit  2e85676f0c56d1e5f28fabcd9a3ab9952442a35b (2026-08-06)
 *   license MIT © Clypra Contributors — full text in THIRD-PARTY-NOTICES.md
 *
 * ── Why lift it ─────────────────────────────────────────────────────────────
 * Timeline.tsx currently accumulates `playheadMs += dt * rate` inside a rAF
 * loop. That accumulator inherits every rAF pathology: the browser throttles
 * background tabs to 1 Hz (the playhead crawls), a long synchronous stall
 * (decode, shader compile) swallows frames, and float error accumulates over a
 * long session. An AudioContext advances on the audio hardware clock, so
 * reading `currentTime` gives the true elapsed time no matter what rAF did.
 * The clock is the *authority*; rAF only decides when we look at it.
 *
 * ── Time domain: MILLISECONDS (the one decision) ───────────────────────────
 * Clypra's clock is in seconds. SundayEdit is milliseconds end to end —
 * `playheadMs`, `durationMs`, `timeline_start_ms`, `in_ms`/`out_ms`, the Rust
 * model, the export path. Rather than convert at every call site (where a
 * missing ×1000 is a silent, plausible-looking bug), this port is milliseconds
 * *throughout*; seconds exist only inside `ClockTimeSource`, which is the one
 * boundary that talks to `AudioContext.currentTime`, and they are converted
 * the moment they are read. No public API of this module is in seconds.
 *
 * ── Port changes (beyond seconds → ms and naming) ───────────────────────────
 * 1. **Injectable time source / frame scheduler.** Clypra news up an
 *    `AudioContext` and calls global rAF directly, so its own tests have to
 *    monkey-patch `globalThis`. Here both are constructor options, which makes
 *    the whole clock testable with no DOM and no audio hardware.
 * 2. **Never freezes on a suspended AudioContext.** Clypra returns the stale
 *    `_time` whenever `ctx.state !== "running"` — an AudioContext created
 *    without a user gesture starts suspended, so its playhead would sit still
 *    until `resume()` resolved. We anchor on `performance.now()` until the
 *    audio clock actually runs, then re-anchor onto it seamlessly.
 * 3. **Signed rate.** Clypra clamps speed to [0.1, 4] forward-only. Our
 *    transport is J/K/L shuttle: negative rates play in reverse, magnitude up
 *    to `MAX_SHUTTLE` (8), and `rate === 0` means stopped. Reverse stops at 0
 *    exactly like forward stops at the duration.
 * 4. **`setRate` re-anchors instead of pause→play.** Clypra pauses (which
 *    snaps the playhead to a frame boundary) and replays, so every shuttle
 *    step nudged the playhead. We keep the exact position and just re-anchor.
 * 5. **Seek hold is opt-in.** Clypra's `seek()` always sets `isSeeking`, which
 *    freezes the clock until `completeSeek()` — and `play()` does not clear it,
 *    so one missed `completeSeek()` wedges playback silently. Here the freeze
 *    is `seek(ms, { hold: true })`, taken deliberately by a caller that will
 *    complete it.
 * 6. **Generation is bumped on every transport change**, not only on `play()`,
 *    so a tick scheduled before a pause/stop/seek can never apply after it.
 * 7. **No module-level singleton.** Clypra exports `getPlaybackClock()`; a
 *    process-wide mutable clock is a footgun under React StrictMode double
 *    mounts. Owners construct and `dispose()` their own instance.
 *
 * Not wired into Timeline.tsx — that swap is programme stage E2 (see
 * docs/OSS-INTEGRATION-PROGRAM.md). This lands as a standalone tested module.
 */

/** Transport state. `stopped` is "parked at 0", `paused` is "parked here". */
export type PlaybackStatus = "playing" | "paused" | "stopped";

/** An immutable read of the clock, handed to subscribers. */
export interface PlaybackSnapshot {
  /** Playhead position, ms. */
  timeMs: number;
  status: PlaybackStatus;
  /** Signed transport rate: 0 stopped, 1 realtime, <0 reverse. */
  rate: number;
  /** Timeline duration, ms — the upper bound the playhead stops at. */
  durationMs: number;
  /** Project frame rate, used for frame-boundary snapping. */
  fps: number;
}

export type PlaybackListener = (snapshot: PlaybackSnapshot) => void;

/**
 * The monotonic clock the playhead rides on. `AudioContext` in the app,
 * a fake in tests — the only place seconds appear in this module.
 */
export interface ClockTimeSource {
  /** Monotonically increasing seconds (`AudioContext.currentTime`). */
  readonly currentTimeSec: number;
  /** Whether the source is actually advancing right now. */
  readonly running: boolean;
  /** Ask a suspended source to start advancing (may resolve asynchronously). */
  resume(): void;
  /** Release the underlying resource. */
  close(): void;
}

export interface PlaybackClockOptions {
  /** Time source factory. Defaults to AudioContext, then `performance.now()`. */
  createTimeSource?: () => ClockTimeSource;
  /** Frame scheduler. Defaults to `requestAnimationFrame`. */
  requestFrame?: (callback: () => void) => number;
  /** Frame canceller. Defaults to `cancelAnimationFrame`. */
  cancelFrame?: (handle: number) => void;
  /** Wall clock for notification throttling. Defaults to `performance.now()`. */
  nowMs?: () => number;
  /** Minimum gap between subscriber notifications during playback, ms. */
  notifyIntervalMs?: number;
  /** Initial duration, ms. */
  durationMs?: number;
  /** Initial frame rate. */
  fps?: number;
}

/** Highest shuttle magnitude, mirroring `geometry.MAX_SHUTTLE`. */
export const MAX_CLOCK_RATE = 8;

/** Subscribers are UI; ~10 Hz is plenty and keeps React out of the rAF loop. */
const DEFAULT_NOTIFY_INTERVAL_MS = 100;

const DEFAULT_FPS = 30;

/** Round a time (ms) to the nearest frame boundary. Non-positive fps: no-op. */
export function snapMsToFrame(ms: number, fps: number): number {
  if (!(fps > 0)) return ms;
  return (Math.round((ms * fps) / 1000) * 1000) / fps;
}

/** Coerce a possibly-garbage number to a finite one, else `fallback`. */
function finiteOr(value: number, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

/**
 * The default time source: an `AudioContext` when the runtime has one (the
 * Tauri webview always does), otherwise a `performance.now()` shim so the
 * clock still works in tests, SSR-ish contexts and headless runs.
 */
export function createDefaultTimeSource(): ClockTimeSource {
  const Ctor =
    typeof globalThis.AudioContext !== "undefined"
      ? globalThis.AudioContext
      : undefined;

  if (!Ctor) {
    return {
      get currentTimeSec() {
        return performance.now() / 1000;
      },
      get running() {
        return true;
      },
      resume() {},
      close() {},
    };
  }

  const context = new Ctor();
  return {
    get currentTimeSec() {
      return context.currentTime;
    },
    get running() {
      return context.state === "running";
    },
    resume() {
      void context.resume();
    },
    close() {
      void context.close();
    },
  };
}

/**
 * The playhead clock. Read `timeMs` imperatively every frame from render
 * loops; `subscribe()` is for UI that only needs a throttled snapshot.
 */
export class PlaybackClock {
  private timeMsValue = 0;
  private statusValue: PlaybackStatus = "stopped";
  private rateValue = 0;
  private durationMsValue: number;
  private fpsValue: number;

  /** True while a caller holds a seek open (see `seek(ms, { hold: true })`). */
  private seekHeld = false;

  private readonly createTimeSource: () => ClockTimeSource;
  private readonly requestFrame: (callback: () => void) => number;
  private readonly cancelFrame: (handle: number) => void;
  private readonly nowMs: () => number;
  private readonly notifyIntervalMs: number;

  private source: ClockTimeSource | null = null;
  private frameHandle: number | null = null;

  /** Playhead position when the current play segment was anchored. */
  private anchorTimeMs = 0;
  /** Audio-source reading at the anchor (seconds), when anchored to audio. */
  private anchorSourceSec = 0;
  /** Wall-clock reading at the anchor (ms), the fallback basis. */
  private anchorWallMs = 0;
  /** Whether the anchor currently rides the audio clock. */
  private anchoredToAudio = false;

  /** Elapsed reading captured by `recordStallStart()`, ms. */
  private stallStartElapsedMs: number | null = null;

  /** Bumped on every transport change; stale frame callbacks self-cancel. */
  private generation = 0;

  private readonly listeners = new Set<PlaybackListener>();
  private lastNotifyMs = Number.NEGATIVE_INFINITY;
  private disposed = false;

  constructor(options: PlaybackClockOptions = {}) {
    this.createTimeSource = options.createTimeSource ?? createDefaultTimeSource;
    this.requestFrame =
      options.requestFrame ?? ((cb) => requestAnimationFrame(() => cb()));
    this.cancelFrame = options.cancelFrame ?? ((h) => cancelAnimationFrame(h));
    this.nowMs = options.nowMs ?? (() => performance.now());
    this.notifyIntervalMs = Math.max(
      0,
      options.notifyIntervalMs ?? DEFAULT_NOTIFY_INTERVAL_MS,
    );
    this.durationMsValue = Math.max(0, finiteOr(options.durationMs ?? 0, 0));
    this.fpsValue = Math.max(
      1,
      finiteOr(options.fps ?? DEFAULT_FPS, DEFAULT_FPS),
    );
  }

  // ─── Imperative reads ─────────────────────────────────────────────────────

  /**
   * The live playhead, ms. Computed from the time source rather than from the
   * last tick, so it stays correct even when frames are being dropped (idle
   * background tab, GPU stall, a long synchronous op on the main thread).
   */
  get timeMs(): number {
    if (this.statusValue !== "playing" || this.seekHeld)
      return this.timeMsValue;
    return this.clampTime(
      this.anchorTimeMs + this.elapsedMs() * this.rateValue,
    );
  }

  get status(): PlaybackStatus {
    return this.statusValue;
  }

  get rate(): number {
    return this.rateValue;
  }

  get durationMs(): number {
    return this.durationMsValue;
  }

  get fps(): number {
    return this.fpsValue;
  }

  /** Whether a caller is holding a seek open. */
  get isSeeking(): boolean {
    return this.seekHeld;
  }

  /** Full snapshot — what subscribers receive. */
  getSnapshot(): PlaybackSnapshot {
    return {
      timeMs: this.timeMs,
      status: this.statusValue,
      rate: this.rateValue,
      durationMs: this.durationMsValue,
      fps: this.fpsValue,
    };
  }

  // ─── Configuration ────────────────────────────────────────────────────────

  /** Set the timeline duration (ms). Re-clamps a playhead now out of range. */
  setDurationMs(durationMs: number): void {
    this.durationMsValue = Math.max(0, finiteOr(durationMs, 0));
    if (this.timeMsValue > this.durationMsValue) {
      this.timeMsValue = this.durationMsValue;
      if (this.statusValue === "playing") this.reanchor();
    }
    this.notify();
  }

  /** Set the project frame rate (used for frame snapping). */
  setFps(fps: number): void {
    this.fpsValue = Math.max(1, finiteOr(fps, DEFAULT_FPS));
    this.notify();
  }

  /**
   * Set the signed transport rate. `0` pauses. Magnitude is clamped to
   * `MAX_CLOCK_RATE`. While playing the playhead keeps its exact position —
   * we only re-anchor, so a shuttle step never nudges it (port change #4).
   */
  setRate(rate: number): void {
    const wanted = finiteOr(rate, 0);
    if (wanted === 0) {
      // Pause FIRST: it reads the live playhead, which is only computable
      // while the old rate still stands.
      this.pause();
      this.rateValue = 0;
      this.notify();
      return;
    }

    const clamped =
      Math.sign(wanted) * Math.min(MAX_CLOCK_RATE, Math.abs(wanted));

    if (this.statusValue === "playing") {
      this.timeMsValue = this.timeMs;
      this.rateValue = clamped;
      this.invalidateTicks();
      this.cancelScheduledTick();
      this.reanchor();
      this.notify();
      // Re-arm the loop under the new generation.
      this.scheduleTick();
      return;
    }

    this.rateValue = clamped;
    this.notify();
  }

  // ─── Transport ────────────────────────────────────────────────────────────

  /**
   * Start playing at the current rate (defaulting to realtime forward when the
   * clock is at rest). No-op when already playing.
   */
  play(): void {
    if (this.disposed || this.statusValue === "playing") return;

    if (this.rateValue === 0) this.rateValue = 1;

    if (!this.source) this.source = this.createTimeSource();
    if (!this.source.running) this.source.resume();

    this.statusValue = "playing";
    this.reanchor();
    this.invalidateTicks();
    this.notify();
    this.scheduleTick();
  }

  /**
   * Pause where we are. The playhead snaps to the nearest frame boundary so a
   * paused preview always shows a real frame (and stepping from here is exact).
   */
  pause(): void {
    const wasPlaying = this.statusValue === "playing";
    if (wasPlaying) this.timeMsValue = this.timeMs;

    this.timeMsValue = this.clampTime(
      snapMsToFrame(this.timeMsValue, this.fpsValue),
    );
    this.statusValue = this.statusValue === "stopped" ? "stopped" : "paused";
    this.seekHeld = false;
    this.invalidateTicks();
    this.cancelScheduledTick();
    this.notify();
  }

  /** Pause and rewind to 0, in a single notification. */
  stop(): void {
    this.invalidateTicks();
    this.cancelScheduledTick();
    this.statusValue = "stopped";
    this.timeMsValue = 0;
    this.rateValue = 0;
    this.seekHeld = false;
    this.notify();
  }

  /**
   * Jump the playhead to `timeMs` (clamped, frame-snapped). Playback continues
   * if it was running — the pause/play cycle bumps the generation so a tick
   * scheduled before the jump can't drag the playhead back.
   *
   * `hold: true` freezes the clock at the target until `completeSeek()`, for
   * callers that want to wait for media elements to present the frame. Opt-in,
   * because a forgotten `completeSeek()` stalls playback (port change #5).
   *
   * Seeking off zero from `stopped` leaves the clock `paused`: `stopped` means
   * "at rest at the head", and a parked playhead elsewhere is a pause.
   */
  seek(timeMs: number, options: { hold?: boolean } = {}): void {
    const wasPlaying = this.statusValue === "playing";
    const target = this.clampTime(finiteOr(timeMs, 0));

    this.invalidateTicks();
    this.cancelScheduledTick();

    this.timeMsValue = this.clampTime(snapMsToFrame(target, this.fpsValue));
    this.seekHeld = options.hold === true;

    if (wasPlaying) {
      this.reanchor();
      this.notify();
      this.scheduleTick();
      return;
    }

    if (this.statusValue === "stopped" && this.timeMsValue !== 0) {
      this.statusValue = "paused";
    }
    this.notify();
  }

  /** Release a held seek and re-anchor so playback resumes without a jump. */
  completeSeek(): void {
    if (!this.seekHeld) return;
    this.seekHeld = false;
    if (this.statusValue === "playing") this.reanchor();
    this.notify();
  }

  // ─── Stall compensation ───────────────────────────────────────────────────

  /**
   * Mark the start of a known synchronous stall (decoding a frame, compiling a
   * shader, an expensive layout). Pair with `compensateStall()`.
   *
   * Without this the audio clock keeps running through the stall, so when we
   * look again the playhead has leapt forward — and every media element then
   * looks "behind" and gets a corrective seek, which is exactly the stutter we
   * are trying to avoid. No-op when not playing.
   */
  recordStallStart(): void {
    if (this.statusValue !== "playing") return;
    this.stallStartElapsedMs = this.elapsedMs();
  }

  /**
   * Push the anchor forward by the stall's duration so the playhead resumes
   * where it stood when the stall began. No-op without a `recordStallStart()`.
   */
  compensateStall(): void {
    const stallStart = this.stallStartElapsedMs;
    this.stallStartElapsedMs = null;
    if (stallStart === null || this.statusValue !== "playing") return;

    const stallMs = this.elapsedMs() - stallStart;
    if (stallMs <= 0) return;

    // Advance both bases: the anchor may switch from wall clock to audio at
    // any tick, and both must describe the same instant.
    this.anchorSourceSec += stallMs / 1000;
    this.anchorWallMs += stallMs;
  }

  // ─── Subscription ─────────────────────────────────────────────────────────

  /**
   * Subscribe to throttled snapshots (~10 Hz during playback, immediately on
   * every transport change). Render loops should read `timeMs` instead.
   */
  subscribe(listener: PlaybackListener): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  // ─── Teardown ─────────────────────────────────────────────────────────────

  /** Cancel the loop, drop subscribers and release the time source. */
  dispose(): void {
    this.disposed = true;
    this.invalidateTicks();
    this.cancelScheduledTick();
    this.statusValue = "stopped";
    this.rateValue = 0;
    this.seekHeld = false;
    this.listeners.clear();
    if (this.source) {
      this.source.close();
      this.source = null;
    }
  }

  // ─── Internals ────────────────────────────────────────────────────────────

  private clampTime(ms: number): number {
    return Math.max(0, Math.min(this.durationMsValue, ms));
  }

  /** Milliseconds elapsed since the current anchor, on whichever basis holds. */
  private elapsedMs(): number {
    if (this.anchoredToAudio && this.source && this.source.running) {
      return (this.source.currentTimeSec - this.anchorSourceSec) * 1000;
    }
    return this.nowMs() - this.anchorWallMs;
  }

  /** Pin the anchor to the present instant and the present playhead. */
  private reanchor(): void {
    this.anchorTimeMs = this.timeMsValue;
    this.anchorWallMs = this.nowMs();
    this.anchorSourceSec = this.source ? this.source.currentTimeSec : 0;
    this.anchoredToAudio = this.source !== null && this.source.running;
    this.stallStartElapsedMs = null;
  }

  /**
   * An AudioContext created outside a user gesture starts suspended and its
   * `currentTime` sits at 0 until `resume()` lands. We ride the wall clock
   * until then and switch over the moment the audio clock is really running
   * (port change #2) — re-anchoring first, so the switch costs no time.
   */
  private adoptAudioClockWhenReady(): void {
    if (this.anchoredToAudio || !this.source || !this.source.running) return;
    this.timeMsValue = this.timeMs;
    this.reanchor();
  }

  private invalidateTicks(): void {
    this.generation += 1;
  }

  private cancelScheduledTick(): void {
    if (this.frameHandle !== null) {
      this.cancelFrame(this.frameHandle);
      this.frameHandle = null;
    }
  }

  private scheduleTick(): void {
    if (this.disposed) return;
    const generation = this.generation;
    this.frameHandle = this.requestFrame(() => this.tick(generation));
  }

  /**
   * One frame of the loop. The clock does not *advance* here — the time source
   * does that on its own — this only publishes, watches for the end of the
   * range, and re-arms.
   */
  private tick(generation: number): void {
    // A tick scheduled before a pause/seek/play must not apply afterwards.
    if (generation !== this.generation) return;
    if (this.disposed || this.statusValue !== "playing") return;

    this.frameHandle = null;

    if (this.seekHeld) {
      // Held seek: stay pinned at the target, keep the loop alive.
      this.reanchor();
      this.notifyThrottled();
      this.scheduleTick();
      return;
    }

    this.adoptAudioClockWhenReady();

    const raw = this.anchorTimeMs + this.elapsedMs() * this.rateValue;
    const hitEnd = this.rateValue > 0 && raw >= this.durationMsValue;
    const hitStart = this.rateValue < 0 && raw <= 0;

    if (hitEnd || hitStart) {
      this.timeMsValue = hitEnd ? this.durationMsValue : 0;
      this.statusValue = "paused";
      this.notify();
      return;
    }

    this.timeMsValue = raw;
    this.notifyThrottled();
    this.scheduleTick();
  }

  private notifyThrottled(): void {
    const now = this.nowMs();
    if (now - this.lastNotifyMs < this.notifyIntervalMs) return;
    this.lastNotifyMs = now;
    this.emit();
  }

  private notify(): void {
    this.lastNotifyMs = this.nowMs();
    this.emit();
  }

  private emit(): void {
    if (this.listeners.size === 0) return;
    const snapshot = this.getSnapshot();
    for (const listener of [...this.listeners]) listener(snapshot);
  }
}
