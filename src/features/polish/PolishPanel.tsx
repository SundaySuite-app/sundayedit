/**
 * AI polish panel — Phase 4.1.
 *
 * One job: fix punctuation and capitalization with Claude, never word
 * content. The backend enforces that guarantee with a substance guard, so
 * anything the model tried to rephrase is rejected and listed here rather
 * than silently applied.
 *
 * Flow: pick a model → see the scope + estimated cost (pure, no spend) →
 * run → review the per-word changes (original → polished) and any rejected
 * captions.
 *
 * `onProjectChange` is `useProjectStore.commit` (App wires every panel to it):
 * the polished project enters the SAME undo stack as caption ops and timeline
 * drags, so ⌘Z reverts the whole pass. That was not true before the R1 trust
 * round — panels landed via `setProject`, which touched neither stack, and
 * this comment claimed an undo that did not exist.
 */

import { useEffect, useState } from "react";
import { Sparkles, ArrowRight, ShieldAlert, KeyRound } from "lucide-react";

import { ipc, IPCError } from "@/lib/ipc";
import type {
  ClaudeModel,
  PolishEstimate,
  PolishResult,
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

export function PolishPanel({ project, onProjectChange }: Props) {
  const t = useT();
  const [model, setModel] = useState<ClaudeModel>("haiku45");
  const [apiKey, setApiKey] = useState("");
  const [estimate, setEstimate] = useState<PolishEstimate | null>(null);
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<PolishResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Pure, no-network preview — refresh when the model changes.
  useEffect(() => {
    let cancelled = false;
    ipc.polish
      .estimate(project, model)
      .then((e) => !cancelled && setEstimate(e))
      .catch(() => !cancelled && setEstimate(null));
    return () => {
      cancelled = true;
    };
  }, [project, model]);

  async function run() {
    setError(null);
    setResult(null);
    setRunning(true);
    try {
      const res = await ipc.polish.run(project, model, apiKey || undefined);
      setResult(res);
      onProjectChange(res.project);
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
    } finally {
      setRunning(false);
    }
  }

  return (
    <div className="mx-auto max-w-2xl space-y-6 p-6">
      <header>
        <h2 className="mb-1 flex items-center gap-2 text-[var(--text-ui-lg)] font-semibold">
          <Sparkles size={16} className="text-[var(--color-accent-400)]" />{" "}
          {t("polishTitle")}
        </h2>
        <p className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
          {t("polishIntro")}
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

      {/* Estimate + run */}
      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={run}
          disabled={running || !estimate || estimate.caption_count === 0}
          className="flex items-center gap-1.5 rounded-md bg-[var(--color-accent-600)] px-4 py-2 text-[var(--text-ui-sm)] font-medium text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)] disabled:opacity-50"
        >
          <Sparkles size={14} /> {running ? t("polishRunning") : t("polishRun")}
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

      {result && <Results result={result} />}
    </div>
  );
}

function Results({ result }: { result: PolishResult }) {
  const t = useT();
  const { changes, rejected } = result;
  return (
    <section className="space-y-4">
      {rejected.length > 0 && (
        <div className="flex items-start gap-2 rounded-md border border-[var(--color-warning)]/40 bg-[var(--color-warning)]/10 px-3 py-2 text-[var(--text-ui-sm)] text-[var(--color-warning)]">
          <ShieldAlert size={15} className="mt-0.5 shrink-0" />
          <span>{t("polishRejected", { n: rejected.length })}</span>
        </div>
      )}

      {changes.length === 0 ? (
        <p className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
          {t("polishNoChanges")}
        </p>
      ) : (
        <>
          <h3 className="text-[var(--text-ui-sm)] font-semibold">
            {t("polishChangesHeader", { n: changes.length })}
          </h3>
          <ul className="max-h-72 space-y-1 overflow-y-auto rounded-md border border-[var(--color-border)] p-2">
            {changes.map((c, i) => (
              <li
                key={i}
                className="flex items-center gap-2 rounded px-2 py-1 text-[var(--text-ui-sm)] hover:bg-[var(--color-bg-surface)]"
              >
                <span className="font-mono text-[var(--color-fg-subtle)] line-through">
                  {c.from}
                </span>
                <ArrowRight
                  size={12}
                  className="shrink-0 text-[var(--color-fg-subtle)]"
                />
                <span className="font-mono text-[var(--color-accent-400)]">
                  {c.to}
                </span>
              </li>
            ))}
          </ul>
        </>
      )}
    </section>
  );
}
