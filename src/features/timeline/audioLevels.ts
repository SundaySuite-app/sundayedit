/**
 * Audio-level math (R2) — the ONE place gain/volume dB values get clamped and
 * converted to a linear amplitude, shared by the preview executor
 * (`MediaPlayer`/`mediaReconcile`) and the inspector/track-header UI.
 *
 * Mirrors `src-tauri/src/model.rs`'s `clamp_gain_db`/`GAIN_DB_MIN`/
 * `GAIN_DB_MAX` and `services/compose.rs`'s `total_db = effective_gain_db() +
 * effective_volume_db()` byte-for-byte. Two independently-correct copies of
 * "clamp to [-60, 12], NaN → unity, dB add for the combined level" is exactly
 * the seam this codebase keeps producing — keeping the frontend's copy in
 * this one module (instead of inlined wherever a slider or a preview reads a
 * level) means there is only one place to keep in sync with the Rust side.
 */

/** Mirrors `model::GAIN_DB_MIN` — silent for any practical purpose. */
export const GAIN_DB_MIN = -60;
/** Mirrors `model::GAIN_DB_MAX` — four doublings of amplitude. */
export const GAIN_DB_MAX = 12;

/**
 * Clamp a dB level into `[GAIN_DB_MIN, GAIN_DB_MAX]`, mapping NaN to unity.
 * Mirrors `model::clamp_gain_db` — the backend re-clamps on every op, so a
 * value already in the project is always in range, but a hand-edited file
 * (or a slider mid-drag before the round-trip lands) may not be.
 */
export function clampGainDb(db: number): number {
  // Only NaN has no in-range meaning — +/-Infinity clamp to their nearer
  // bound just like any other out-of-range number (mirrors `f32::clamp`,
  // which is defined for infinities but panics on NaN).
  return Number.isNaN(db)
    ? 0
    : Math.min(GAIN_DB_MAX, Math.max(GAIN_DB_MIN, db));
}

/** Convert a dB level to a linear amplitude ratio (10^(db/20)). Not clamped —
 *  callers that feed an `HTMLMediaElement.volume` need {@link previewVolumeFor}
 *  instead, which additionally reports when the true value exceeds unity. */
export function dbToLinear(db: number): number {
  return Math.pow(10, db / 20);
}

/**
 * The clip's audible level the render will use: item gain dB-added to its
 * track's fader, each clamped first — the exact expression
 * `services::compose::total_db` sums. `trackVolumeDb` defaults to `0` (unity)
 * for an item whose track cannot be resolved, matching the Rust side's
 * `is_none_or`-flavoured fallbacks elsewhere in the same module.
 */
export function combinedGainDb(
  itemGainDb: number,
  trackVolumeDb: number,
): number {
  return clampGainDb(itemGainDb) + clampGainDb(trackVolumeDb);
}

/**
 * What a `<video>` element should be set to for a combined dB level.
 *
 * `volume` is the linear amplitude clamped to `[0, 1]` — the range
 * `HTMLMediaElement.volume` accepts; it CANNOT exceed unity, unlike the
 * ffmpeg `volume=` filter the export applies. `clipped` is true exactly when
 * the combined level is positive gain (> 0 dB, i.e. the true linear value
 * exceeds 1) — the preview then plays quieter than the export will render,
 * and the caller must say so rather than pretend the boost was heard.
 */
export function previewVolumeFor(combinedDb: number): {
  volume: number;
  clipped: boolean;
} {
  const linear = dbToLinear(Number.isNaN(combinedDb) ? 0 : combinedDb);
  return { volume: Math.min(1, Math.max(0, linear)), clipped: linear > 1 };
}

/** How close a dragged dB value must be to 0 to snap to it — a slider
 *  convenience only (not persisted, not sent to Rust). Wide enough to be
 *  reachable by mouse/touch, narrow enough that a deliberate small trim near
 *  unity is never eaten by the snap. */
export const GAIN_DETENT_EPSILON_DB = 0.3;

/** Snap a dragged dB value to exactly `0` when it is within
 *  {@link GAIN_DETENT_EPSILON_DB} of it — the "0 dB detent" the gain and
 *  track-volume sliders both offer. */
export function applyGainDetent(db: number): number {
  return Math.abs(db) <= GAIN_DETENT_EPSILON_DB ? 0 : db;
}

/** Format a dB value for a slider readout: always signed, one decimal. */
export function formatDb(db: number): string {
  const rounded = Math.round(db * 10) / 10;
  const sign = rounded > 0 ? "+" : "";
  return `${sign}${rounded.toFixed(1)} dB`;
}
