/**
 * Readability (re-flow) panel — Phase 7.
 *
 * Surfaces the `reflow` service the same way confidence highlighting surfaces
 * uncertain words: it flags every caption that breaks a broadcast readability
 * limit (reading speed / line length / line count / minimum duration) against
 * an editable {@link ReflowConfig}, and offers a one-click auto-repair that
 * splits the offenders at word boundaries into the fewest broadcast-compliant
 * sub-captions.
 *
 * Analysis is pure + offline, so the panel re-checks live whenever the project
 * or the limits change. Repair flows back through `onProjectChange` — wired by
 * App to `useProjectStore.commit` — so the auto-split is one undo step and the
 * export sees the result, exactly like the cleanup tools.
 */

import { useCallback, useEffect, useState } from "react";
import { Gauge, Wand2, Check, AlertTriangle, RotateCcw } from "lucide-react";

import { ipc, IPCError, DEFAULT_REFLOW_CONFIG } from "@/lib/ipc";
import type { Project, ReflowConfig, ReflowIssue } from "@/lib/bindings";
import { useT, type TKey } from "@/lib/i18n";
import { cn } from "@/lib/cn";

interface Props {
  project: Project;
  onProjectChange: (project: Project) => void;
}

// Issue `kind` (the contract string from the service) → its label key. The
// order here is the order the breakdown chips render in.
const KINDS: Array<{ kind: string; labelKey: TKey }> = [
  { kind: "cps", labelKey: "reflowKindCps" },
  { kind: "line_length", labelKey: "reflowKindLineLength" },
  { kind: "line_count", labelKey: "reflowKindLineCount" },
  { kind: "min_duration", labelKey: "reflowKindMinDuration" },
];

interface RepairResult {
  before: number;
  after: number;
  resolved: number;
  residual: number;
}

export function ReflowPanel({ project, onProjectChange }: Props) {
  const t = useT();
  const [cfg, setCfg] = useState<ReflowConfig>(DEFAULT_REFLOW_CONFIG);
  const [issues, setIssues] = useState<ReflowIssue[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<RepairResult | null>(null);

  const analyze = useCallback(async (p: Project, c: ReflowConfig) => {
    setError(null);
    try {
      setIssues(await ipc.reflow.analyze(p, c));
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
      setIssues(null);
    }
  }, []);

  // Live re-check: analysis is a pure, offline round-trip, so re-run it
  // whenever the captions or the limits change. A repair updates `project`
  // (via onProjectChange) which flows back through here and refreshes the
  // remaining issues automatically.
  useEffect(() => {
    void analyze(project, cfg);
  }, [project, cfg, analyze]);

  async function repair() {
    if (!issues || issues.length === 0) return;
    setBusy(true);
    setError(null);
    try {
      const before = project.captions.length;
      const next = await ipc.reflow.repair(project, cfg);
      // Re-analyze the repaired track so the summary can report exactly how
      // many issues survive (a uniformly over-CPS caption can't be split into
      // compliance — repair leaves it visible rather than dropping words).
      const residual = await ipc.reflow.analyze(next, cfg);
      setResult({
        before,
        after: next.captions.length,
        resolved: issues.length - residual.length,
        residual: residual.length,
      });
      onProjectChange(next);
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  function patch(p: Partial<ReflowConfig>) {
    setCfg((c) => ({ ...c, ...p }));
    setResult(null);
  }

  const total = issues?.length ?? 0;

  return (
    <div className="space-y-6 p-5">
      <section>
        <h2 className="mb-1 flex items-center gap-2 text-[var(--text-ui-lg)] font-semibold">
          <Gauge size={16} className="text-[var(--color-accent-400)]" />
          {t("reflowTitle")}
        </h2>
        <p className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
          {t("reflowIntro")}
        </p>
      </section>

      {/* Limits */}
      <section className="space-y-3">
        <div className="flex items-center justify-between">
          <h3 className="text-[var(--text-ui-sm)] font-semibold text-[var(--color-fg-muted)]">
            {t("reflowLimits")}
          </h3>
          <button
            type="button"
            onClick={() => {
              setCfg(DEFAULT_REFLOW_CONFIG);
              setResult(null);
            }}
            className="flex items-center gap-1 text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)] hover:text-[var(--color-accent-400)]"
          >
            <RotateCcw size={11} /> {t("reflowReset")}
          </button>
        </div>
        <div className="grid grid-cols-2 gap-3">
          <NumberField
            label={t("reflowMaxCps")}
            unit={t("reflowUnitCps")}
            value={cfg.max_cps}
            step={0.5}
            min={1}
            onChange={(v) => patch({ max_cps: v })}
          />
          <NumberField
            label={t("reflowMaxCharsPerLine")}
            unit={t("reflowUnitChars")}
            value={cfg.max_chars_per_line}
            min={1}
            onChange={(v) => patch({ max_chars_per_line: Math.round(v) })}
          />
          <NumberField
            label={t("reflowMaxLines")}
            value={cfg.max_lines}
            min={1}
            onChange={(v) => patch({ max_lines: Math.round(v) })}
          />
          <NumberField
            label={t("reflowMinDuration")}
            unit={t("reflowUnitMs")}
            value={cfg.min_duration_ms}
            step={50}
            min={0}
            onChange={(v) => patch({ min_duration_ms: Math.round(v) })}
          />
        </div>
      </section>

      {/* Status */}
      {error ? (
        <p className="text-[var(--text-ui-sm)] text-[var(--color-danger)]">
          {error}
        </p>
      ) : issues === null ? (
        <p className="text-[var(--text-ui-sm)] text-[var(--color-fg-subtle)]">
          {t("reflowChecking")}
        </p>
      ) : total === 0 ? (
        <div className="flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-bg-surface)] px-3 py-2.5 text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
          <Check size={15} className="text-[var(--color-accent-400)]" />
          {t("reflowClean")}
        </div>
      ) : (
        <section className="space-y-3">
          <div className="flex items-center gap-2 text-[var(--text-ui-sm)] font-medium">
            <AlertTriangle size={15} className="text-[var(--color-warning)]" />
            {t("reflowIssueCount", { n: total })}
          </div>

          {/* Breakdown by kind */}
          <div className="flex flex-wrap gap-1.5">
            {KINDS.map(({ kind, labelKey }) => {
              const n = issues.filter((i) => i.kind === kind).length;
              if (n === 0) return null;
              return (
                <span
                  key={kind}
                  className="rounded-full bg-[var(--color-bg-surface)] px-2.5 py-0.5 text-[var(--text-ui-xs)] text-[var(--color-fg-muted)]"
                >
                  {t(labelKey)} · {n}
                </span>
              );
            })}
          </div>

          {/* Per-caption issues */}
          <ul className="max-h-56 space-y-1 overflow-y-auto rounded-md border border-[var(--color-border)] p-2">
            {issues.map((issue, i) => (
              <li
                key={`${issue.caption_id}-${issue.kind}-${i}`}
                className="rounded px-2 py-1 text-[var(--text-ui-sm)] hover:bg-[var(--color-bg-surface)]"
              >
                <span className="text-[var(--color-fg)]">{issue.message}</span>
                <span className="ml-2 font-mono text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
                  {fmtValue(issue)} /{" "}
                  {fmtValue({ ...issue, value: issue.limit })}
                </span>
              </li>
            ))}
          </ul>

          <button
            type="button"
            onClick={repair}
            disabled={busy}
            className={cn(
              "flex items-center gap-1.5 rounded-md bg-[var(--color-accent-600)] px-4 py-2 text-[var(--text-ui-sm)] font-medium text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)]",
              busy && "cursor-not-allowed opacity-60",
            )}
          >
            <Wand2 size={14} />
            {busy ? t("reflowRepairing") : t("reflowRepair", { n: total })}
          </button>
        </section>
      )}

      {/* Last repair outcome */}
      {result && (
        <div className="space-y-1 rounded-md border border-[var(--color-border)] bg-[var(--color-bg-surface)] px-3 py-2.5 text-[var(--text-ui-sm)]">
          <p className="text-[var(--color-fg-muted)]">
            {t("reflowRepaired", {
              resolved: result.resolved,
              before: result.before,
              after: result.after,
            })}
          </p>
          {result.residual > 0 && (
            <p className="text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
              {t("reflowResidual", { n: result.residual })}
            </p>
          )}
        </div>
      )}
    </div>
  );
}

/** Render an issue's measured value with a unit appropriate to its kind. */
function fmtValue(issue: Pick<ReflowIssue, "kind" | "value">): string {
  switch (issue.kind) {
    case "cps":
      return `${issue.value.toFixed(1)} CPS`;
    case "min_duration":
      return `${Math.round(issue.value)} ms`;
    case "line_length":
      return `${Math.round(issue.value)} ch`;
    default:
      return String(Math.round(issue.value));
  }
}

function NumberField({
  label,
  unit,
  value,
  onChange,
  min,
  step = 1,
}: {
  label: string;
  unit?: string;
  value: number;
  onChange: (v: number) => void;
  min?: number;
  step?: number;
}) {
  return (
    <label className="flex flex-col gap-1">
      <span className="text-[var(--text-ui-xs)] text-[var(--color-fg-muted)]">
        {label}
      </span>
      <div className="flex items-center gap-1.5">
        <input
          type="number"
          value={value}
          min={min}
          step={step}
          onChange={(e) => {
            const v = Number(e.target.value);
            if (!Number.isNaN(v))
              onChange(min !== undefined ? Math.max(min, v) : v);
          }}
          className="w-full rounded-md border border-[var(--color-border)] bg-[var(--color-bg-input)] px-2.5 py-1.5 text-[var(--text-ui-sm)] outline-none focus:border-[var(--color-accent-500)]"
        />
        {unit && (
          <span className="shrink-0 text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
            {unit}
          </span>
        )}
      </div>
    </label>
  );
}
