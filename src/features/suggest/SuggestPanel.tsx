/**
 * Smart Suggest panel — Phase 4.3.
 *
 * The AI proposes substantive rewrites (fix mis-transcriptions, tighten
 * run-ons, shorten for readability). Per the plan's hard rule, nothing is
 * applied silently: this is a review queue. Each suggestion shows the
 * original → proposed diff and the model's reasoning, and the user accepts
 * or rejects it one at a time. Accepting applies that single suggestion and
 * removes it from the queue; the caption's display span is preserved.
 *
 * `onProjectChange` is `useProjectStore.commit`, so each ACCEPTED suggestion
 * is its own undo step — accept four and change your mind about the last one,
 * and ⌘Z takes back exactly that one.
 */

import { useEffect, useState } from "react";
import { Lightbulb, ArrowRight, Check, X } from "lucide-react";

import { ipc, IPCError } from "@/lib/ipc";
import type {
  ClaudeModel,
  PolishEstimate,
  Project,
  Strictness,
  Suggestion,
  SuggestionKind,
} from "@/lib/bindings";
import { useT, type TKey } from "@/lib/i18n";
import { cn } from "@/lib/cn";
import { formatCost } from "@/lib/cost";

interface Props {
  project: Project;
  onProjectChange: (project: Project) => void;
}

const MODELS: { id: ClaudeModel; name: string }[] = [
  { id: "haiku45", name: "Haiku 4.5" },
  { id: "sonnet46", name: "Sonnet 4.6" },
  { id: "opus47", name: "Opus 4.7" },
];

const STRICTNESS: { id: Strictness; labelKey: TKey }[] = [
  { id: "conservative", labelKey: "strictnessConservative" },
  { id: "balanced", labelKey: "strictnessBalanced" },
  { id: "aggressive", labelKey: "strictnessAggressive" },
];

const KIND_KEY: Record<SuggestionKind, TKey> = {
  "fix-transcription": "suggestKindFix",
  rephrase: "suggestKindRephrase",
  shorten: "suggestKindShorten",
};

export function SuggestPanel({ project, onProjectChange }: Props) {
  const t = useT();
  const captionText = (captionId: string): string => {
    const cap = project.captions.find((c) => c.id === captionId);
    return cap ? cap.words.map((w) => w.text).join(" ") : t("captionGone");
  };
  const [model, setModel] = useState<ClaudeModel>("haiku45");
  const [strictness, setStrictness] = useState<Strictness>("balanced");
  const [apiKey, setApiKey] = useState("");
  const [estimate, setEstimate] = useState<PolishEstimate | null>(null);
  const [queue, setQueue] = useState<Suggestion[] | null>(null);
  const [running, setRunning] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    ipc.suggest
      .estimate(project, model, strictness)
      .then((e) => !cancelled && setEstimate(e))
      .catch(() => !cancelled && setEstimate(null));
    return () => {
      cancelled = true;
    };
  }, [project, model, strictness]);

  async function run() {
    setError(null);
    setRunning(true);
    try {
      const res = await ipc.suggest.run(
        project,
        model,
        strictness,
        apiKey || undefined,
      );
      setQueue(res);
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
    } finally {
      setRunning(false);
    }
  }

  async function accept(s: Suggestion) {
    setError(null);
    setBusyId(s.caption_id);
    try {
      const next = await ipc.suggest.apply(project, s);
      onProjectChange(next);
      setQueue((q) => (q ? q.filter((x) => x !== s) : q));
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
    } finally {
      setBusyId(null);
    }
  }

  function reject(s: Suggestion) {
    setQueue((q) => (q ? q.filter((x) => x !== s) : q));
  }

  return (
    <div className="mx-auto max-w-2xl space-y-6 p-6">
      <header>
        <h2 className="mb-1 flex items-center gap-2 text-[var(--text-ui-lg)] font-semibold">
          <Lightbulb size={16} className="text-[var(--color-accent-400)]" />{" "}
          {t("navSuggest")}
        </h2>
        <p className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
          {t("suggestIntro")}
        </p>
      </header>

      <div className="grid grid-cols-3 gap-2">
        {MODELS.map((m) => (
          <button
            key={m.id}
            type="button"
            onClick={() => setModel(m.id)}
            className={cn(
              "rounded-lg border px-3 py-2 text-[var(--text-ui-sm)] font-medium transition-colors",
              model === m.id
                ? "border-[var(--color-accent-500)] bg-[var(--color-bg-surface)]"
                : "border-[var(--color-border)] hover:border-[var(--color-fg-subtle)]",
            )}
          >
            {m.name}
          </button>
        ))}
      </div>

      <div className="flex items-center gap-2">
        <span className="text-[var(--text-ui-xs)] text-[var(--color-fg-muted)]">
          {t("strictnessLabel")}
        </span>
        {STRICTNESS.map((s) => (
          <button
            key={s.id}
            type="button"
            onClick={() => setStrictness(s.id)}
            className={cn(
              "rounded-md px-2.5 py-1 text-[var(--text-ui-xs)] transition-colors",
              strictness === s.id
                ? "bg-[var(--color-accent-500)] text-[var(--color-neutral-950)]"
                : "bg-[var(--color-bg-surface)] text-[var(--color-fg-muted)] hover:text-[var(--color-fg)]",
            )}
          >
            {t(s.labelKey)}
          </button>
        ))}
      </div>

      <input
        type="password"
        value={apiKey}
        onChange={(e) => setApiKey(e.target.value)}
        placeholder={t("apiKeyPlaceholder")}
        className="w-full rounded-md border border-[var(--color-border)] bg-[var(--color-bg-input)] px-3 py-1.5 text-[var(--text-ui-sm)] outline-none focus:border-[var(--color-accent-500)]"
      />

      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={run}
          disabled={running || !estimate || estimate.caption_count === 0}
          className="flex items-center gap-1.5 rounded-md bg-[var(--color-accent-600)] px-4 py-2 text-[var(--text-ui-sm)] font-medium text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)] disabled:opacity-50"
        >
          <Lightbulb size={14} />{" "}
          {running ? t("suggestRunning") : t("suggestRun")}
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

      {error && (
        <p className="rounded-md border border-[var(--color-danger)]/40 bg-[var(--color-danger)]/10 px-3 py-2 text-[var(--text-ui-sm)] text-[var(--color-danger)]">
          {error}
        </p>
      )}

      {queue &&
        (queue.length === 0 ? (
          <p className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
            {t("suggestNoneNeeded")}
          </p>
        ) : (
          <section className="space-y-3">
            <h3 className="text-[var(--text-ui-sm)] font-semibold">
              {t("suggestQueueHeader", { n: queue.length })}
            </h3>
            {queue.map((s, i) => (
              <article
                key={i}
                className="rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-surface)] p-3"
              >
                <div className="mb-2 flex items-center gap-2">
                  <span className="rounded bg-[var(--color-accent-600)]/20 px-2 py-0.5 text-[var(--text-ui-xs)] font-medium text-[var(--color-accent-400)]">
                    {t(KIND_KEY[s.kind])}
                  </span>
                  <span className="text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
                    {s.reasoning}
                  </span>
                </div>
                <div className="mb-3 space-y-1 text-[var(--text-ui-sm)]">
                  <p className="text-[var(--color-fg-subtle)] line-through">
                    {captionText(s.caption_id)}
                  </p>
                  <p className="flex items-start gap-1.5 text-[var(--color-fg)]">
                    <ArrowRight
                      size={13}
                      className="mt-1 shrink-0 text-[var(--color-accent-400)]"
                    />
                    <span>{s.suggestion}</span>
                  </p>
                </div>
                <div className="flex gap-2">
                  <button
                    type="button"
                    onClick={() => accept(s)}
                    disabled={busyId === s.caption_id}
                    className="flex items-center gap-1 rounded-md bg-[var(--color-accent-600)] px-3 py-1.5 text-[var(--text-ui-xs)] font-medium text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)] disabled:opacity-50"
                  >
                    <Check size={13} /> {t("actionAccept")}
                  </button>
                  <button
                    type="button"
                    onClick={() => reject(s)}
                    className="flex items-center gap-1 rounded-md bg-[var(--color-bg-elevated)] px-3 py-1.5 text-[var(--text-ui-xs)] text-[var(--color-fg-muted)] hover:text-[var(--color-fg)]"
                  >
                    <X size={13} /> {t("actionReject")}
                  </button>
                </div>
              </article>
            ))}
          </section>
        ))}
    </div>
  );
}
