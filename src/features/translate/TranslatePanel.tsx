/**
 * Translation panel — Phase 7.1.
 *
 * Translate the caption track to another language. Timing is preserved:
 * each caption keeps its display span and the translation is re-timed
 * within it. Glossary terms are passed to the model for consistency.
 *
 * Non-destructive until the user commits: running shows a side-by-side
 * preview (original → translation) and flags captions whose translation
 * grew much longer (reading-speed risk). "Replace captions" swaps the track
 * in via `onProjectChange`, which App wires to `useProjectStore.commit` — so
 * the swap is ONE undo step and ⌘Z restores every original caption.
 *
 * That last sentence used to be a lie: the panel committed through
 * `setProject`, which never touched the undo stacks, so the single edit that
 * rewrites the text of every caption in the project was the one edit the user
 * could not take back. `TranslatePanel.undo.test.tsx` now proves it.
 */

import { useEffect, useMemo, useState } from "react";
import { Languages, ArrowRight, AlertTriangle } from "lucide-react";

import { ipc, IPCError } from "@/lib/ipc";
import type {
  ClaudeModel,
  PolishEstimate,
  Project,
  TranslationLanguage,
  TranslationResult,
} from "@/lib/bindings";
import { useT } from "@/lib/i18n";
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

function capText(caption: { words: { text: string }[] }): string {
  return caption.words.map((w) => w.text).join(" ");
}

export function TranslatePanel({ project, onProjectChange }: Props) {
  const t = useT();
  const [model, setModel] = useState<ClaudeModel>("haiku45");
  const [target, setTarget] = useState("en");
  const [apiKey, setApiKey] = useState("");
  const [languages, setLanguages] = useState<TranslationLanguage[]>([]);
  const [estimate, setEstimate] = useState<PolishEstimate | null>(null);
  const [result, setResult] = useState<TranslationResult | null>(null);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    ipc.translate
      .languages()
      .then(setLanguages)
      .catch(() => setLanguages([]));
  }, []);

  useEffect(() => {
    let cancelled = false;
    ipc.translate
      .estimate(project, target, model)
      .then((e) => !cancelled && setEstimate(e))
      .catch(() => !cancelled && setEstimate(null));
    return () => {
      cancelled = true;
    };
  }, [project, target, model]);

  const warnedIds = useMemo(
    () => new Set(result?.warnings.map((w) => w.caption_id) ?? []),
    [result],
  );
  const originalById = useMemo(
    () => new Map(project.captions.map((c) => [c.id, capText(c)])),
    [project],
  );

  async function run() {
    setError(null);
    setResult(null);
    setRunning(true);
    try {
      const res = await ipc.translate.run(
        project,
        target,
        model,
        apiKey || undefined,
      );
      setResult(res);
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
    } finally {
      setRunning(false);
    }
  }

  function replace() {
    if (!result) return;
    onProjectChange({
      ...project,
      captions: result.captions,
      language: result.target_language,
      updated_at: Date.now(),
    });
    setResult(null);
  }

  const targetName = languages.find((l) => l.code === target)?.name ?? target;

  return (
    <div className="mx-auto max-w-2xl space-y-6 p-6">
      <header>
        <h2 className="mb-1 flex items-center gap-2 text-[var(--text-ui-lg)] font-semibold">
          <Languages size={16} className="text-[var(--color-accent-400)]" />{" "}
          {t("translateTitle")}
        </h2>
        <p className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
          {t("translateIntro")}
        </p>
      </header>

      <div className="flex flex-wrap items-center gap-3">
        <label className="flex items-center gap-2 text-[var(--text-ui-sm)]">
          <span className="text-[var(--color-fg-muted)]">
            {t("translateTo")}
          </span>
          <select
            value={target}
            onChange={(e) => setTarget(e.target.value)}
            className="rounded-md border border-[var(--color-border)] bg-[var(--color-bg-input)] px-2 py-1.5 text-[var(--text-ui-sm)] outline-none focus:border-[var(--color-accent-500)]"
          >
            {languages.map((l) => (
              <option key={l.code} value={l.code}>
                {l.name}
              </option>
            ))}
          </select>
        </label>
        <div className="flex gap-1.5">
          {MODELS.map((m) => (
            <button
              key={m.id}
              type="button"
              onClick={() => setModel(m.id)}
              className={cn(
                "rounded-md px-2.5 py-1 text-[var(--text-ui-xs)] transition-colors",
                model === m.id
                  ? "bg-[var(--color-accent-500)] text-[var(--color-neutral-950)]"
                  : "bg-[var(--color-bg-surface)] text-[var(--color-fg-muted)] hover:text-[var(--color-fg)]",
              )}
            >
              {m.name}
            </button>
          ))}
        </div>
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
          <Languages size={14} />{" "}
          {running
            ? t("translateRunning", { lang: targetName })
            : t("translateRun", { lang: targetName })}
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

      {result && (
        <section className="space-y-3">
          <div className="flex items-center gap-3">
            <button
              type="button"
              onClick={replace}
              className="rounded-md bg-[var(--color-accent-600)] px-4 py-2 text-[var(--text-ui-sm)] font-medium text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)]"
            >
              {t("translateReplace")}
            </button>
            <button
              type="button"
              onClick={() => setResult(null)}
              className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)] hover:text-[var(--color-fg)]"
            >
              {t("actionCancel")}
            </button>
          </div>

          {result.warnings.length > 0 && (
            <div className="flex items-start gap-2 rounded-md border border-[var(--color-warning)]/40 bg-[var(--color-warning)]/10 px-3 py-2 text-[var(--text-ui-sm)] text-[var(--color-warning)]">
              <AlertTriangle size={15} className="mt-0.5 shrink-0" />
              <span>
                {t("translateWarnings", { n: result.warnings.length })}
              </span>
            </div>
          )}

          <ul className="max-h-80 space-y-2 overflow-y-auto rounded-md border border-[var(--color-border)] p-2">
            {result.captions.map((c) => (
              <li
                key={c.id}
                className={cn(
                  "rounded px-2 py-1.5 text-[var(--text-ui-sm)]",
                  warnedIds.has(c.id) && "bg-[var(--color-warning)]/5",
                )}
              >
                <p className="text-[var(--color-fg-subtle)]">
                  {originalById.get(c.id) ?? ""}
                </p>
                <p className="flex items-start gap-1.5">
                  <ArrowRight
                    size={13}
                    className="mt-1 shrink-0 text-[var(--color-accent-400)]"
                  />
                  <span>{capText(c)}</span>
                  {warnedIds.has(c.id) && (
                    <AlertTriangle
                      size={12}
                      className="mt-1 shrink-0 text-[var(--color-warning)]"
                    />
                  )}
                </p>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}
