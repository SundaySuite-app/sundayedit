/**
 * Clip inspector — properties panel for the selected timeline item.
 *
 * Floats over the workspace when a clip is selected on the timeline. Every edit
 * commits through the shared `useProjectStore.run`, so it lands on the SAME
 * undo/redo stack as caption + drag edits and is fully reversible:
 *
 *   - Trim — in/out source points (ms). `trimTimelineItem`.
 *   - Split — at the playhead (mirrors the timeline's B blade key), enabled
 *     only while the playhead stands strictly inside the clip's span.
 *     `splitTimelineItem`.
 *   - Transition — leading-edge type (none/fade/crossfade/dip) + duration.
 *     `setTransition` / `clearTransition`.
 *   - Transform — scale / x / y for overlay/PiP clips. `setTransform`.
 *
 * The panel reads the live item out of the project each render (so committed
 * ops reflect immediately), while numeric trim fields keep a local buffer so
 * typing isn't clobbered mid-edit; they commit on blur / Enter.
 */

import { useEffect, useState } from "react";
import { Scissors, Trash2, X } from "lucide-react";

import type { Project, TimelineItem, Transform } from "@/lib/bindings";
import { ipc } from "@/lib/ipc";
import { useProjectStore } from "@/lib/useProjectStore";
import { useT, type TKey } from "@/lib/i18n";
import { itemSpan } from "./laneLayout";
import { getPlayheadMs, usePlayheadInsideSpan } from "./playhead";

/**
 * Transition kinds we expose in the picker. `""` = no transition. Every
 * non-empty value is a REAL ffmpeg `xfade` transition name — the kind string
 * is stored verbatim in the project and emitted as `xfade=transition={kind}`
 * at export, so an invented "friendly" name here aborts the whole render
 * (regression: seam-xfade-transition-vocabulary — the picker used to offer
 * "crossfade"/"dip", which ffmpeg rejects). The picker↔ffmpeg seam is pinned
 * end-to-end by src-tauri/tests/compose_xfade_vocabulary.rs, which parses
 * THIS array literal out of the source — keep it a flat list of quoted names.
 */
const TRANSITION_KINDS = [
  "",
  "fade",
  "dissolve",
  "fadeblack",
  "fadewhite",
  "wipeleft",
  "wiperight",
  "slideleft",
  "slideright",
  "circleopen",
] as const;

/** Translated label per xfade name (the value stays the exact ffmpeg name). */
const TRANSITION_LABEL_KEYS: Record<string, TKey> = {
  fade: "inspectorTransitionFade",
  dissolve: "inspectorTransitionCrossfade",
  fadeblack: "inspectorTransitionDip",
  fadewhite: "inspectorTransitionDipWhite",
  wipeleft: "inspectorTransitionWipeLeft",
  wiperight: "inspectorTransitionWipeRight",
  slideleft: "inspectorTransitionSlideLeft",
  slideright: "inspectorTransitionSlideRight",
  circleopen: "inspectorTransitionCircleOpen",
};

/**
 * Friendly kinds older project files may still carry, mapped to the real
 * xfade names the backend renders them as (`compose::xfade_transition_name`).
 * Shown — and re-committed — as their real name so the picker reflects what
 * will actually be rendered.
 */
const LEGACY_TRANSITION_KINDS: Record<string, string> = {
  crossfade: "dissolve",
  dip: "fadeblack",
};

const DEFAULT_TRANSITION_MS = 500;

export function ClipInspector({
  item,
  onClose,
}: {
  item: TimelineItem;
  onClose: () => void;
}) {
  const t = useT();
  const run = useProjectStore((s) => s.run);

  // Local buffers for the trim fields so typing isn't overwritten by re-renders;
  // reset whenever the selected clip (or its committed value) changes.
  const [inBuf, setInBuf] = useState(String(item.in_ms));
  const [outBuf, setOutBuf] = useState(String(item.out_ms));
  useEffect(() => setInBuf(String(item.in_ms)), [item.id, item.in_ms]);
  useEffect(() => setOutBuf(String(item.out_ms)), [item.id, item.out_ms]);

  // Live playhead from the timeline's shared store — split is only meaningful
  // strictly inside the clip's span (an edge cut leaves a zero-length half).
  // Subscribing to the derived boolean (not the raw ms) keeps the inspector
  // from re-rendering 60×/s during playback; the click reads the live value.
  const span = itemSpan(item);
  const canSplit = usePlayheadInsideSpan(span.start_ms, span.end_ms);

  const transition = item.transition_in;
  // Legacy friendly kinds ("crossfade"/"dip") display — and re-commit — as the
  // real xfade name the backend renders them as.
  const transitionKind = transition
    ? (LEGACY_TRANSITION_KINDS[transition.kind] ?? transition.kind)
    : "";
  const transform = item.transform;

  const [durBuf, setDurBuf] = useState(String(transition?.duration_ms ?? ""));
  useEffect(
    () => setDurBuf(String(transition?.duration_ms ?? "")),
    [item.id, transition?.duration_ms],
  );

  function commit(op: (p: Project) => Promise<Project>) {
    void run(op).catch(() => {
      // Clamped / rejected by the backend — leave the project untouched.
    });
  }

  // Ripple-delete: later same-track clips slide left to close the gap.
  // Mirrors the Timeline's Delete/Backspace shortcut — this is the same op
  // made discoverable as a button, and closes the panel with the clip since
  // there's nothing left to inspect.
  function deleteClip() {
    void run((p) => ipc.timeline.rippleDeleteItem(p, item.id))
      .then(() => onClose())
      .catch(() => {
        // Clamped / rejected by the backend — leave the project untouched.
      });
  }

  function commitTrim(edge: "in" | "out", raw: string) {
    const value = Number(raw);
    if (!Number.isFinite(value)) return;
    if (edge === "in" && value === item.in_ms) return;
    if (edge === "out" && value === item.out_ms) return;
    commit((p) =>
      ipc.timeline.trimTimelineItem(p, item.id, {
        newInMs: edge === "in" ? value : undefined,
        newOutMs: edge === "out" ? value : undefined,
      }),
    );
  }

  function commitTransitionKind(kind: string) {
    if (kind === "") {
      commit((p) => ipc.timeline.clearTransition(p, item.id));
      return;
    }
    const duration = transition?.duration_ms ?? DEFAULT_TRANSITION_MS;
    commit((p) => ipc.timeline.setTransition(p, item.id, kind, duration));
  }

  function commitTransitionDuration(raw: string) {
    const duration = Number(raw);
    if (!Number.isFinite(duration) || !transition) return;
    commit((p) =>
      ipc.timeline.setTransition(p, item.id, transitionKind, duration),
    );
  }

  function commitTransform(patch: Partial<Transform>) {
    const next: Transform = { ...transform, ...patch };
    commit((p) => ipc.timeline.setTransform(p, item.id, next));
  }

  return (
    <aside
      data-testid="clip-inspector"
      className="pointer-events-auto flex max-h-[70vh] w-72 flex-col overflow-hidden rounded-xl border border-[var(--color-border)] bg-[var(--color-bg-elevated)] shadow-2xl"
    >
      <div className="flex shrink-0 items-center justify-between border-b border-[var(--color-border)] px-4 py-2.5">
        <div className="flex min-w-0 flex-col">
          <span className="text-[var(--text-ui-sm)] font-semibold">
            {t("inspectorTitle")}
          </span>
          <span className="truncate text-[10px] uppercase tracking-wide text-[var(--color-fg-subtle)]">
            {item.text?.text || item.kind}
          </span>
        </div>
        <button
          type="button"
          onClick={onClose}
          title={t("inspectorClose")}
          aria-label={t("inspectorClose")}
          className="rounded-md p-1 text-[var(--color-fg-subtle)] hover:bg-[var(--color-bg-surface)] hover:text-[var(--color-fg)]"
        >
          <X size={15} />
        </button>
      </div>

      <div className="min-h-0 flex-1 space-y-5 overflow-y-auto p-4">
        {/* Trim */}
        <Section title={t("inspectorTrimHeader")}>
          <div className="grid grid-cols-2 gap-2">
            <NumberField
              label={t("inspectorTrimIn")}
              value={inBuf}
              onChange={setInBuf}
              onCommit={() => commitTrim("in", inBuf)}
            />
            <NumberField
              label={t("inspectorTrimOut")}
              value={outBuf}
              onChange={setOutBuf}
              onCommit={() => commitTrim("out", outBuf)}
            />
          </div>
          <button
            type="button"
            data-testid="inspector-split"
            disabled={!canSplit}
            onClick={() =>
              commit((p) =>
                ipc.timeline.splitTimelineItem(p, item.id, getPlayheadMs()),
              )
            }
            title={t("inspectorSplit")}
            className="inline-flex w-full items-center justify-center gap-1.5 rounded-md border border-[var(--color-border)] px-2 py-1.5 text-[var(--text-ui-sm)] text-[var(--color-fg-muted)] hover:bg-[var(--color-bg-surface)] hover:text-[var(--color-fg)] disabled:pointer-events-none disabled:opacity-40"
          >
            <Scissors size={13} />
            {t("inspectorSplit")}
          </button>
          <button
            type="button"
            data-testid="inspector-delete"
            onClick={deleteClip}
            title={t("inspectorDelete")}
            className="inline-flex w-full items-center justify-center gap-1.5 rounded-md border border-[var(--color-border)] px-2 py-1.5 text-[var(--text-ui-sm)] text-[var(--color-fg-muted)] hover:border-[var(--color-danger,#b3261e)] hover:bg-[var(--color-bg-surface)] hover:text-[var(--color-danger,#b3261e)]"
          >
            <Trash2 size={13} />
            {t("inspectorDelete")}
          </button>
        </Section>

        {/* Transition */}
        <Section title={t("inspectorTransitionHeader")}>
          <label className="flex flex-col gap-1">
            <span className="text-[10px] text-[var(--color-fg-subtle)]">
              {t("inspectorTransitionType")}
            </span>
            <select
              value={transitionKind}
              onChange={(e) => commitTransitionKind(e.target.value)}
              className="rounded-md border border-[var(--color-border)] bg-[var(--color-bg-surface)] px-2 py-1.5 text-[var(--text-ui-sm)]"
            >
              {TRANSITION_KINDS.map((k) => (
                <option key={k || "none"} value={k}>
                  {k === ""
                    ? t("inspectorTransitionNone")
                    : t(TRANSITION_LABEL_KEYS[k])}
                </option>
              ))}
              {/* A kind we don't offer (hand-edited project file) still shows
                  as itself instead of silently snapping to the first option. */}
              {transitionKind !== "" &&
                !(TRANSITION_KINDS as readonly string[]).includes(
                  transitionKind,
                ) && <option value={transitionKind}>{transitionKind}</option>}
            </select>
          </label>
          {transition && (
            <NumberField
              label={t("inspectorDurationMs")}
              value={durBuf}
              onChange={setDurBuf}
              onCommit={() => commitTransitionDuration(durBuf)}
            />
          )}
        </Section>

        {/* Transform (overlay / PiP) */}
        <Section title={t("inspectorTransformHeader")}>
          <SliderField
            label={t("inspectorScale")}
            min={0.1}
            max={3}
            step={0.05}
            value={transform.scale}
            onChange={(v) => commitTransform({ scale: v })}
          />
          <SliderField
            label={t("inspectorX")}
            min={-1}
            max={1}
            step={0.01}
            value={transform.x}
            onChange={(v) => commitTransform({ x: v })}
          />
          <SliderField
            label={t("inspectorY")}
            min={-1}
            max={1}
            step={0.01}
            value={transform.y}
            onChange={(v) => commitTransform({ y: v })}
          />
        </Section>
      </div>
    </aside>
  );
}

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section className="space-y-2">
      <h4 className="text-[var(--text-ui-xs)] font-semibold uppercase tracking-wider text-[var(--color-fg-subtle)]">
        {title}
      </h4>
      {children}
    </section>
  );
}

function NumberField({
  label,
  value,
  onChange,
  onCommit,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  /** Commit the current buffer (on blur / Enter). */
  onCommit: () => void;
}) {
  return (
    <label className="flex flex-col gap-1">
      <span className="text-[10px] text-[var(--color-fg-subtle)]">{label}</span>
      <input
        type="number"
        inputMode="numeric"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onBlur={() => onCommit()}
        onKeyDown={(e) => {
          if (e.key === "Enter") (e.target as HTMLInputElement).blur();
        }}
        className="rounded-md border border-[var(--color-border)] bg-[var(--color-bg-surface)] px-2 py-1.5 font-mono text-[var(--text-ui-sm)] tabular-nums"
      />
    </label>
  );
}

function SliderField({
  label,
  min,
  max,
  step,
  value,
  onChange,
}: {
  label: string;
  min: number;
  max: number;
  step: number;
  value: number;
  onChange: (v: number) => void;
}) {
  return (
    <label className="flex flex-col gap-1">
      <div className="flex items-center justify-between">
        <span className="text-[10px] text-[var(--color-fg-subtle)]">
          {label}
        </span>
        <span className="font-mono text-[10px] tabular-nums text-[var(--color-fg-muted)]">
          {value.toFixed(2)}
        </span>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="accent-[var(--color-accent-500)]"
      />
    </label>
  );
}
