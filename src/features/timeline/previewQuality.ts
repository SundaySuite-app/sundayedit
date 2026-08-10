/**
 * Adaptive preview quality ladder (programme stage E2) — how many pixels, and
 * how many frames, the preview is allowed to spend given what the editor is
 * doing right now.
 *
 * Until now the preview had ONE implicit quality: whatever the surface happened
 * to be, and a preview-proxy render hardcoded to 480p in `compose::proxy_
 * settings`. That is the wrong shape for both ends of the range — a parked
 * playhead deserves every pixel (the user is inspecting a frame), and a clip
 * being dragged across the timeline deserves almost none (nobody pixel-peeps a
 * moving ghost). A ladder makes that trade explicit and testable:
 *
 *   Idle        parked playhead        100 %  — the frame under inspection
 *   Playback    rolling transport       50 %  — motion hides the resample
 *   Interaction scrubbing or dragging   25 %  — latency beats fidelity here
 *   Export      producing an artefact  100 %  — never trade quality for speed
 *
 * The tier is a pure function of the activity, so the policy is unit-tested
 * offline and every consumer (the preview surface, the proxy-render request)
 * reads the same number instead of inventing its own.
 *
 * ── What "Export" means here ────────────────────────────────────────────────
 * The final export (`compose.render` + `defaultComposeSettings`) does not
 * consult this module at all — it is derived from the project's own geometry
 * and must stay that way. `Export` exists so a caller that is *producing* an
 * artefact from the preview stack (the flatten-to-proxy render) can ask for the
 * un-degraded tier by name rather than by hardcoding 100.
 */

/** The rungs of the ladder, coarsest last. */
export type PreviewTier = "idle" | "playback" | "interaction" | "export";

/** What the editor is doing — everything the tier decision depends on. */
export interface PreviewActivity {
  /** Signed transport rate: 0 parked, 1 realtime, <0 reverse, |r|>1 shuttle. */
  rate: number;
  /** A scrub or a clip/caption drag is in flight (pointer down on the canvas). */
  interacting: boolean;
  /** An artefact is being produced from this state (proxy render / export). */
  exporting: boolean;
}

/** A resolved rung: the tier and the percentage of full resolution it allows. */
export interface PreviewQuality {
  tier: PreviewTier;
  /** Percentage of full resolution, 1…100. */
  scalePct: number;
}

const TIER_SCALE_PCT: Record<PreviewTier, number> = {
  idle: 100,
  playback: 50,
  interaction: 25,
  export: 100,
};

/**
 * Which rung applies. Precedence is coarsest-need-first: an artefact render
 * outranks everything (it must not be degraded), an in-flight interaction
 * outranks playback (you can scrub *while* playing, and the scrub is the thing
 * that needs to feel instant), and playback outranks a parked playhead.
 */
export function tierFor(activity: PreviewActivity): PreviewTier {
  if (activity.exporting) return "export";
  if (activity.interacting) return "interaction";
  const rate = activity.rate;
  const rolling =
    typeof rate === "number" && Number.isFinite(rate) && rate !== 0;
  return rolling ? "playback" : "idle";
}

/** The tier plus its scale, in one read. Pure. */
export function qualityFor(activity: PreviewActivity): PreviewQuality {
  const tier = tierFor(activity);
  return { tier, scalePct: TIER_SCALE_PCT[tier] };
}

/** The scale a named tier allows — for callers that already know their tier. */
export function scalePctForTier(tier: PreviewTier): number {
  return TIER_SCALE_PCT[tier];
}

/** Never skip more than this many frames in a row, whatever the shuttle rate. */
export const MAX_RENDER_STRIDE = 8;

/**
 * How many clock frames pass between two *rendered* preview updates at a given
 * transport rate.
 *
 * At 1× and below we render every frame. Above that the playhead travels
 * `|rate|` frames of content per displayed frame, so rendering every one of
 * them costs `|rate|`× the work per wall-clock second — the queue backs up, the
 * UI falls behind the clock, and (because the clock is the authority now) the
 * gap shows up as a jump the moment it catches up. Striding by `|rate|` keeps
 * the work per wall-clock second flat no matter how fast we shuttle; the media
 * element's own rate/seek behaviour is untouched, only how often we publish.
 */
export function renderStride(rate: number): number {
  const speed = Math.abs(rate);
  if (!Number.isFinite(speed) || speed <= 1) return 1;
  return Math.min(MAX_RENDER_STRIDE, Math.max(1, Math.round(speed)));
}

/**
 * Whether the `frameIndex`-th frame of a shuttle run should be rendered, given
 * a stride. Frame 0 always renders, so a run never starts with a skip.
 */
export function shouldRenderFrame(frameIndex: number, stride: number): boolean {
  if (stride <= 1) return true;
  return frameIndex % stride === 0;
}
