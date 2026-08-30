/**
 * AI social-clips panel — SundayEdit's headline feature (Phase E, the UI).
 *
 * One job: turn a finished transcript into a handful of short, self-contained
 * social clips. Claude proposes clips that reference REAL caption ids, so the
 * timings are never model-invented — the backend grounds every clip in the
 * captions it covers.
 *
 * Flow mirrors PolishPanel/SuggestPanel: pick a model → see the scope +
 * estimated cost (pure, no spend) → generate a reviewable plan → curate it
 * (edit titles/hooks, drop clips) → apply it onto the project. Each kept clip
 * can then be rendered as a vertical video with its title overlay burned in.
 *
 * Generating proposes; nothing touches the project until "Bruk plan". The
 * applied plan flows back through `onProjectChange` — App wires it to
 * `useProjectStore.commit` — so applying a plan is one undo step and
 * save/export see it.
 */

import { useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  Scissors,
  KeyRound,
  Trash2,
  Clock,
  Film,
  Loader2,
  Check,
} from "lucide-react";

import { ipc, IPCError } from "@/lib/ipc";
import type {
  ClaudeModel,
  Clip,
  ClipPlan,
  PolishEstimate,
  Project,
} from "@/lib/bindings";
import { useT, type TKey } from "@/lib/i18n";
import { cn } from "@/lib/cn";
import { formatCost } from "@/lib/cost";

interface Props {
  project: Project;
  onProjectChange: (project: Project) => void;
}

const MODELS: { id: ClaudeModel; name: string; hintKey: TKey }[] = [
  { id: "haiku45", name: "Haiku 4.5", hintKey: "modelHaikuHint" },
  { id: "sonnet46", name: "Sonnet 4.6", hintKey: "modelSonnetHint" },
  { id: "opus47", name: "Opus 4.7", hintKey: "modelOpusHint" },
];

export function ClipsPanel({ project, onProjectChange }: Props) {
  const t = useT();
  const [model, setModel] = useState<ClaudeModel>("haiku45");
  const [apiKey, setApiKey] = useState("");
  const [estimate, setEstimate] = useState<PolishEstimate | null>(null);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The locally-editable plan under review. null until generated; if the
  // project already has clips we seed it so they're re-editable.
  const [plan, setPlan] = useState<ClipPlan | null>(() =>
    project.clips.length > 0
      ? { talk_summary: project.talk_summary ?? "", clips: project.clips }
      : null,
  );
  const [applied, setApplied] = useState(false);

  // Pure, no-network preview — refresh when the model changes.
  useEffect(() => {
    let cancelled = false;
    ipc.clips
      .estimate(project, model)
      .then((e) => !cancelled && setEstimate(e))
      .catch(() => !cancelled && setEstimate(null));
    return () => {
      cancelled = true;
    };
  }, [project, model]);

  async function generate() {
    setError(null);
    setApplied(false);
    setRunning(true);
    try {
      const result = await ipc.clips.generate(
        project,
        model,
        apiKey || undefined,
      );
      setPlan(result);
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
    } finally {
      setRunning(false);
    }
  }

  function updateClip(id: string, patch: Partial<Clip>) {
    setApplied(false);
    setPlan((p) =>
      p
        ? {
            ...p,
            clips: p.clips.map((c) => (c.id === id ? { ...c, ...patch } : c)),
          }
        : p,
    );
  }

  function dropClip(id: string) {
    setApplied(false);
    setPlan((p) =>
      p ? { ...p, clips: p.clips.filter((c) => c.id !== id) } : p,
    );
  }

  async function apply() {
    if (!plan) return;
    try {
      const next = await ipc.clips.applyPlan(project, plan);
      onProjectChange(next);
      setApplied(true);
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
    }
  }

  const hasCaptions = (estimate?.caption_count ?? 0) > 0;

  return (
    <div className="mx-auto max-w-2xl space-y-6 p-6">
      <header>
        <h2 className="mb-1 flex items-center gap-2 text-[var(--text-ui-lg)] font-semibold">
          <Scissors size={16} className="text-[var(--color-accent-400)]" />{" "}
          {t("clipsTitle")}
        </h2>
        <p className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
          {t("clipsIntro")}
        </p>
      </header>

      {/* Model picker */}
      <div className="grid grid-cols-3 gap-2">
        {MODELS.map((m) => (
          <button
            key={m.id}
            type="button"
            onClick={() => setModel(m.id)}
            className={cn(
              "rounded-lg border px-3 py-2 text-left transition-colors",
              model === m.id
                ? "border-[var(--color-accent-500)] bg-[var(--color-bg-surface)]"
                : "border-[var(--color-border)] hover:border-[var(--color-fg-subtle)]",
            )}
          >
            <div className="text-[var(--text-ui-sm)] font-medium">{m.name}</div>
            <div className="text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
              {t(m.hintKey)}
            </div>
          </button>
        ))}
      </div>

      {/* API key (optional — backend falls back to ANTHROPIC_API_KEY) */}
      <label className="flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-bg-input)] px-3 py-1.5">
        <KeyRound size={14} className="text-[var(--color-fg-subtle)]" />
        <input
          type="password"
          value={apiKey}
          onChange={(e) => setApiKey(e.target.value)}
          placeholder={t("apiKeyPlaceholder")}
          className="flex-1 bg-transparent text-[var(--text-ui-sm)] outline-none placeholder:text-[var(--color-fg-subtle)]"
        />
      </label>

      {/* Estimate + generate */}
      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={generate}
          disabled={running || !hasCaptions}
          className="flex items-center gap-1.5 rounded-md bg-[var(--color-accent-600)] px-4 py-2 text-[var(--text-ui-sm)] font-medium text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)] disabled:opacity-50"
        >
          {running ? (
            <Loader2 size={14} className="animate-spin" />
          ) : (
            <Scissors size={14} />
          )}
          {running
            ? t("clipsFinding")
            : plan
              ? t("clipsRegenerate")
              : t("clipsGenerate")}
        </button>
        {estimate && (
          <span className="text-[var(--text-ui-xs)] text-[var(--color-fg-muted)]">
            {t("estCaptionsCost", {
              n: estimate.caption_count,
              cost: formatCost(estimate.estimated_cost_usd),
            })}
          </span>
        )}
      </div>

      {!hasCaptions && (
        <p className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
          {t("clipsNeedCaptions")}
        </p>
      )}

      {error && (
        <p className="rounded-md border border-[var(--color-danger)]/40 bg-[var(--color-danger)]/10 px-3 py-2 text-[var(--text-ui-sm)] text-[var(--color-danger)]">
          {error}
        </p>
      )}

      {plan && (
        <PlanReview
          project={project}
          plan={plan}
          applied={applied}
          onUpdateClip={updateClip}
          onDropClip={dropClip}
          onChangeSummary={(s) => {
            setApplied(false);
            setPlan((p) => (p ? { ...p, talk_summary: s } : p));
          }}
          onApply={apply}
        />
      )}
    </div>
  );
}

function PlanReview({
  project,
  plan,
  applied,
  onUpdateClip,
  onDropClip,
  onChangeSummary,
  onApply,
}: {
  project: Project;
  plan: ClipPlan;
  applied: boolean;
  onUpdateClip: (id: string, patch: Partial<Clip>) => void;
  onDropClip: (id: string) => void;
  onChangeSummary: (s: string) => void;
  onApply: () => void;
}) {
  const t = useT();
  return (
    <section className="space-y-5 border-t border-[var(--color-border)] pt-5">
      {/* Talk summary */}
      <div>
        <h3 className="mb-1.5 text-[var(--text-ui-sm)] font-semibold">
          {t("clipsSummaryLabel")}
        </h3>
        <textarea
          value={plan.talk_summary}
          onChange={(e) => onChangeSummary(e.target.value)}
          rows={3}
          placeholder={t("clipsSummaryPlaceholder")}
          className="w-full resize-y rounded-md border border-[var(--color-border)] bg-[var(--color-bg-input)] px-3 py-2 text-[var(--text-ui-sm)] outline-none placeholder:text-[var(--color-fg-subtle)] focus:border-[var(--color-accent-500)]"
        />
      </div>

      {/* Clips */}
      <div>
        <h3 className="mb-2 text-[var(--text-ui-sm)] font-semibold">
          {t("clipsCountHeader", { n: plan.clips.length })}
        </h3>
        {plan.clips.length === 0 ? (
          <p className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
            {t("clipsNoneInPlan")}
          </p>
        ) : (
          <ul className="space-y-3">
            {plan.clips.map((clip) => (
              <ClipCard
                key={clip.id}
                project={project}
                clip={clip}
                onUpdate={(patch) => onUpdateClip(clip.id, patch)}
                onDrop={() => onDropClip(clip.id)}
              />
            ))}
          </ul>
        )}
      </div>

      {/* Apply */}
      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={onApply}
          disabled={plan.clips.length === 0}
          className="flex items-center gap-1.5 rounded-md bg-[var(--color-accent-600)] px-4 py-2 text-[var(--text-ui-sm)] font-medium text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)] disabled:opacity-50"
        >
          <Check size={14} /> {t("clipsApply")}
        </button>
        {applied && (
          <span className="flex items-center gap-1 text-[var(--text-ui-sm)] text-[var(--color-accent-400)]">
            <Check size={13} /> {t("clipsApplied")}
          </span>
        )}
      </div>
    </section>
  );
}

function ClipCard({
  project,
  clip,
  onUpdate,
  onDrop,
}: {
  project: Project;
  clip: Clip;
  onUpdate: (patch: Partial<Clip>) => void;
  onDrop: () => void;
}) {
  const t = useT();
  // Vertical platform presets only — clips are 9:16 by nature.
  const presetsQuery = useQuery({
    queryKey: ["export-presets"],
    queryFn: () => ipc.render.listExportPresets(),
  });
  const portraitPresets = useMemo(
    () => (presetsQuery.data ?? []).filter((p) => p.aspect === "portrait"),
    [presetsQuery.data],
  );
  const [presetId, setPresetId] = useState<string | null>(null);
  const preset =
    portraitPresets.find((p) => p.id === presetId) ??
    portraitPresets[0] ??
    null;

  const [rendering, setRendering] = useState(false);
  const [renderMsg, setRenderMsg] = useState<string | null>(null);

  async function render() {
    if (!preset) return;
    const base = project.name.replace(/\.[^.]+$/, "");
    const slug = clip.title
      .toLowerCase()
      .replace(/[^a-z0-9æøå]+/gi, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 40);
    const out = await saveDialog({
      defaultPath: `${base}_${slug || "klipp"}.mp4`,
      filters: [{ name: "Video", extensions: ["mp4"] }],
    });
    if (typeof out !== "string") return;
    setRendering(true);
    setRenderMsg(null);
    try {
      await ipc.clips.render(project, clip, out, preset);
      setRenderMsg(t("doneFile", { path: out }));
    } catch (e) {
      setRenderMsg(
        e instanceof IPCError
          ? t("errorPrefix", { error: e.message })
          : String(e),
      );
    } finally {
      setRendering(false);
    }
  }

  return (
    <li className="rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-elevated)] p-3">
      <div className="flex items-start gap-2">
        <div className="flex-1 space-y-2">
          {/* Title — the on-screen overlay */}
          <input
            value={clip.title}
            onChange={(e) => onUpdate({ title: e.target.value })}
            placeholder={t("clipsTitlePlaceholder")}
            className="w-full rounded border border-transparent bg-transparent text-[var(--text-ui-sm)] font-semibold outline-none hover:border-[var(--color-border)] focus:border-[var(--color-accent-500)] focus:bg-[var(--color-bg-input)] focus:px-2 focus:py-1"
          />
          {/* Hook */}
          <input
            value={clip.hook}
            onChange={(e) => onUpdate({ hook: e.target.value })}
            placeholder={t("clipsHookPlaceholder")}
            className="w-full rounded border border-transparent bg-transparent text-[var(--text-ui-sm)] text-[var(--color-fg-muted)] outline-none hover:border-[var(--color-border)] focus:border-[var(--color-accent-500)] focus:bg-[var(--color-bg-input)] focus:px-2 focus:py-1"
          />
          <div className="flex items-center gap-3 text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
            <span className="flex items-center gap-1">
              <Clock size={11} /> {fmtRange(clip.start_ms, clip.end_ms)}
            </span>
            <span>·</span>
            <span>{fmtDuration(clip.end_ms - clip.start_ms)}</span>
            <span>·</span>
            <span>
              {t("clipsCaptionsCount", { n: clip.caption_ids.length })}
            </span>
          </div>
        </div>
        <button
          type="button"
          onClick={onDrop}
          title={t("clipsRemove")}
          aria-label={t("clipsRemove")}
          className="shrink-0 rounded p-1.5 text-[var(--color-fg-subtle)] hover:bg-[var(--color-danger)]/10 hover:text-[var(--color-danger)]"
        >
          <Trash2 size={14} />
        </button>
      </div>

      {/* Render row */}
      <div className="mt-3 flex flex-wrap items-center gap-2 border-t border-[var(--color-border)] pt-3">
        <select
          value={preset?.id ?? ""}
          onChange={(e) => setPresetId(e.target.value)}
          disabled={portraitPresets.length === 0}
          className="rounded-md border border-[var(--color-border)] bg-[var(--color-bg-input)] px-2 py-1 text-[var(--text-ui-xs)] outline-none focus:border-[var(--color-accent-500)] disabled:opacity-50"
        >
          {portraitPresets.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name} · {p.width}×{p.height}
            </option>
          ))}
        </select>
        <button
          type="button"
          onClick={render}
          disabled={rendering || !preset}
          className="flex items-center gap-1.5 rounded-md border border-[var(--color-border)] px-3 py-1 text-[var(--text-ui-xs)] font-medium hover:border-[var(--color-accent-600)] disabled:opacity-50"
        >
          {rendering ? (
            <Loader2 size={12} className="animate-spin" />
          ) : (
            <Film size={12} />
          )}
          {rendering ? t("clipsRendering") : t("clipsRenderVertical")}
        </button>
        {renderMsg && (
          <span className="text-[var(--text-ui-xs)] text-[var(--color-fg-muted)]">
            {renderMsg}
          </span>
        )}
      </div>
    </li>
  );
}
function fmtClock(ms: number): string {
  const totalSec = Math.floor(ms / 1000);
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

function fmtRange(startMs: number, endMs: number): string {
  return `${fmtClock(startMs)}–${fmtClock(endMs)}`;
}

function fmtDuration(ms: number): string {
  const sec = Math.round(ms / 1000);
  return `${sec}s`;
}
