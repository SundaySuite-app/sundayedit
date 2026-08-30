/**
 * A detented dB slider — the clip inspector's gain control (R2 audio). Mirrors
 * `SliderField` in ClipInspector.tsx (label wraps a value readout + the
 * input, giving an accessible name of `"{label}{formatted value}"`, same
 * convention the Scale/X/Y transform sliders use) plus a "reset to 0 dB"
 * affordance the transform sliders don't need — 0 dB is the one value on this
 * range that means "untouched," so it gets a detent AND a one-click way back.
 *
 * The compact track-volume control in the timeline gutter (`Timeline.tsx`'s
 * `TrackHeader`) has its own markup to fit a 184px header row, but shares
 * this module's pure math (`audioLevels.ts`) so "0 dB is home" behaves
 * identically everywhere gain is dragged.
 */

import { useT } from "@/lib/i18n";
import {
  GAIN_DB_MIN,
  GAIN_DB_MAX,
  applyGainDetent,
  formatDb,
} from "./audioLevels";

export function GainSlider({
  label,
  value,
  onChange,
  testId,
}: {
  label: string;
  value: number;
  onChange: (db: number) => void;
  testId?: string;
}) {
  const t = useT();
  const atUnity = value === 0;
  return (
    <div className="flex flex-col gap-1">
      <label className="flex flex-col gap-1">
        <div className="flex items-center justify-between">
          <span className="text-[10px] text-[var(--color-fg-subtle)]">
            {label}
          </span>
          <span className="font-mono text-[10px] tabular-nums text-[var(--color-fg-muted)]">
            {formatDb(value)}
          </span>
        </div>
        <input
          type="range"
          data-testid={testId}
          min={GAIN_DB_MIN}
          max={GAIN_DB_MAX}
          step={0.5}
          value={value}
          onChange={(e) => onChange(applyGainDetent(Number(e.target.value)))}
          className="accent-[var(--color-accent-500)]"
        />
      </label>
      {!atUnity && (
        <button
          type="button"
          data-testid={testId ? `${testId}-reset` : undefined}
          onClick={() => onChange(0)}
          className="self-start text-[10px] text-[var(--color-fg-subtle)] underline decoration-dotted hover:text-[var(--color-fg)]"
        >
          {t("audioResetToUnity")}
        </button>
      )}
    </div>
  );
}
