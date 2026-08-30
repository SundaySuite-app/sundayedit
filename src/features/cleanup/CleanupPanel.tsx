/**
 * Cleanup panel — Phase 7.2 + 7.3.
 *
 * Two tools in one pane:
 *   - Find & replace: search across captions (case / whole-word / regex),
 *     replace-all with a live match count.
 *   - Remove fillers: detect "um/uh/eh/liksom…" per language, review the
 *     list, and ripple-cut the approved ones (shifts later captions
 *     earlier). Replacing with "" via find/replace also works for a quick
 *     non-ripple removal.
 *
 * Edits flow back to the parent through `onProjectChange`, which App wires to
 * `useProjectStore.commit`: a replace-all or a ripple-cut is one undo step on
 * the same stack as caption ops and timeline drags, and export sees the
 * result.
 */

import { useState } from "react";
import { Search, Wand2, Scissors, Check, AudioLines } from "lucide-react";

import { ipc, IPCError } from "@/lib/ipc";
import type { FillerHit, Project, SilenceGap } from "@/lib/bindings";
import { useT } from "@/lib/i18n";
import { cn } from "@/lib/cn";

interface Props {
  project: Project;
  onProjectChange: (project: Project) => void;
}

export function CleanupPanel({ project, onProjectChange }: Props) {
  return (
    <div className="mx-auto max-w-2xl space-y-8 p-6">
      <FindReplace project={project} onProjectChange={onProjectChange} />
      <div className="h-px bg-[var(--color-border)]" />
      <FillerRemoval project={project} onProjectChange={onProjectChange} />
      <div className="h-px bg-[var(--color-border)]" />
      <SilenceRemoval project={project} onProjectChange={onProjectChange} />
    </div>
  );
}

function FindReplace({ project, onProjectChange }: Props) {
  const t = useT();
  const [query, setQuery] = useState("");
  const [replacement, setReplacement] = useState("");
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [wholeWord, setWholeWord] = useState(false);
  const [regex, setRegex] = useState(false);
  const [count, setCount] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const opts = () => ({
    query,
    case_sensitive: caseSensitive,
    whole_word: wholeWord,
    regex,
  });

  async function doFind() {
    setError(null);
    if (!query) {
      setCount(null);
      return;
    }
    try {
      const matches = await ipc.cleanup.find(project, opts());
      setCount(matches.length);
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
      setCount(null);
    }
  }

  async function doReplace() {
    setError(null);
    if (!query) return;
    try {
      const res = await ipc.cleanup.replace(project, opts(), replacement);
      onProjectChange(res.project);
      setCount(0);
      setError(res.count > 0 ? null : t("cleanupNoMatches"));
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
    }
  }

  return (
    <section>
      <h2 className="mb-3 flex items-center gap-2 text-[var(--text-ui-lg)] font-semibold">
        <Search size={16} className="text-[var(--color-accent-400)]" />{" "}
        {t("cleanupFindReplace")}
      </h2>
      <div className="space-y-2">
        <div className="flex gap-2">
          <input
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setCount(null);
            }}
            onKeyDown={(e) => e.key === "Enter" && doFind()}
            placeholder={t("cleanupSearchPlaceholder")}
            className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-bg-input)] px-3 py-1.5 text-[var(--text-ui-sm)] outline-none focus:border-[var(--color-accent-500)]"
          />
          <button
            type="button"
            onClick={doFind}
            className="rounded-md bg-[var(--color-bg-surface)] px-3 py-1.5 text-[var(--text-ui-sm)] hover:text-[var(--color-accent-400)]"
          >
            {t("cleanupFind")}
          </button>
        </div>
        <div className="flex gap-2">
          <input
            value={replacement}
            onChange={(e) => setReplacement(e.target.value)}
            placeholder={t("cleanupReplacePlaceholder")}
            className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-bg-input)] px-3 py-1.5 text-[var(--text-ui-sm)] outline-none focus:border-[var(--color-accent-500)]"
          />
          <button
            type="button"
            onClick={doReplace}
            className="rounded-md bg-[var(--color-accent-600)] px-3 py-1.5 text-[var(--text-ui-sm)] font-medium text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)]"
          >
            {t("cleanupReplaceAll")}
          </button>
        </div>
        <div className="flex items-center gap-4 text-[var(--text-ui-xs)] text-[var(--color-fg-muted)]">
          <Toggle
            label="Aa"
            title={t("cleanupCaseTitle")}
            on={caseSensitive}
            onChange={setCaseSensitive}
          />
          <Toggle
            label={t("cleanupWholeWordLabel")}
            title={t("cleanupWholeWordTitle")}
            on={wholeWord}
            onChange={setWholeWord}
          />
          <Toggle
            label=".*"
            title={t("cleanupRegexTitle")}
            on={regex}
            onChange={setRegex}
          />
          {count !== null && (
            <span className="ml-auto">{t("cleanupMatches", { n: count })}</span>
          )}
        </div>
        {error && (
          <p className="text-[var(--text-ui-xs)] text-[var(--color-danger)]">
            {error}
          </p>
        )}
      </div>
    </section>
  );
}

function FillerRemoval({ project, onProjectChange }: Props) {
  const t = useT();
  const [hits, setHits] = useState<FillerHit[] | null>(null);
  const [approved, setApproved] = useState<Set<number>>(new Set());

  async function detect() {
    const lang = project.language === "auto" ? "en" : project.language;
    const found = await ipc.cleanup.detectFillers(project, lang);
    setHits(found);
    setApproved(new Set(found.map((_, i) => i))); // default: all approved
  }

  async function removeApproved() {
    if (!hits) return;
    const cuts: Array<[number, number]> = hits
      .filter((_, i) => approved.has(i))
      .map((h) => [h.start_ms, h.end_ms]);
    if (cuts.length === 0) return;
    const next = await ipc.cleanup.applyRippleCuts(project, cuts);
    onProjectChange(next);
    setHits(null);
    setApproved(new Set());
  }

  return (
    <section>
      <h2 className="mb-3 flex items-center gap-2 text-[var(--text-ui-lg)] font-semibold">
        <Wand2 size={16} className="text-[var(--color-accent-400)]" />{" "}
        {t("cleanupFillerTitle")}
      </h2>
      <p className="mb-3 text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
        {t("cleanupFillerIntro")}
      </p>

      {hits === null ? (
        <button
          type="button"
          onClick={detect}
          className="rounded-md bg-[var(--color-bg-surface)] px-4 py-2 text-[var(--text-ui-sm)] font-medium hover:text-[var(--color-accent-400)]"
        >
          {t("cleanupFindFillers")}
        </button>
      ) : hits.length === 0 ? (
        <p className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
          {t("cleanupNoFillers")}
        </p>
      ) : (
        <div className="space-y-3">
          <ul className="max-h-64 space-y-1 overflow-y-auto rounded-md border border-[var(--color-border)] p-2">
            {hits.map((h, i) => (
              <li key={i}>
                <label className="flex cursor-pointer items-center gap-2 rounded px-2 py-1 text-[var(--text-ui-sm)] hover:bg-[var(--color-bg-surface)]">
                  <input
                    type="checkbox"
                    checked={approved.has(i)}
                    onChange={(e) => {
                      const next = new Set(approved);
                      if (e.target.checked) next.add(i);
                      else next.delete(i);
                      setApproved(next);
                    }}
                    className="accent-[var(--color-accent-500)]"
                  />
                  <span className="font-mono">{h.text}</span>
                  <span className="ml-auto text-[10px] text-[var(--color-fg-subtle)]">
                    {(h.start_ms / 1000).toFixed(1)}s
                  </span>
                </label>
              </li>
            ))}
          </ul>
          <div className="flex items-center gap-3">
            <button
              type="button"
              onClick={removeApproved}
              className="flex items-center gap-1.5 rounded-md bg-[var(--color-accent-600)] px-4 py-2 text-[var(--text-ui-sm)] font-medium text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)]"
            >
              <Scissors size={14} />{" "}
              {t("cleanupRemoveSelected", { n: approved.size })}
            </button>
            <button
              type="button"
              onClick={() => {
                setHits(null);
                setApproved(new Set());
              }}
              className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)] hover:text-[var(--color-fg)]"
            >
              {t("actionCancel")}
            </button>
            <span className="ml-auto flex items-center gap-1 text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
              <Check size={11} /> {t("cleanupFound", { n: hits.length })}
            </span>
          </div>
        </div>
      )}
    </section>
  );
}

const SILENCE_MIN_MS = 200;
const SILENCE_MAX_MS = 3000;
const SILENCE_STEP_MS = 50;
const SILENCE_DEFAULT_MS = 1000;

function SilenceRemoval({ project, onProjectChange }: Props) {
  const t = useT();
  const [minGapMs, setMinGapMs] = useState(SILENCE_DEFAULT_MS);
  const [gaps, setGaps] = useState<SilenceGap[] | null>(null);
  const [approved, setApproved] = useState<Set<number>>(new Set());
  const [error, setError] = useState<string | null>(null);

  async function detect() {
    setError(null);
    try {
      const found = await ipc.cleanup.detectSilences(project, minGapMs);
      setGaps(found);
      setApproved(new Set(found.map((_, i) => i))); // default: all approved
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
      setGaps(null);
    }
  }

  async function removeApproved() {
    if (!gaps) return;
    const cuts: Array<[number, number]> = gaps
      .filter((_, i) => approved.has(i))
      .map((g) => [g.start_ms, g.end_ms]);
    if (cuts.length === 0) return;
    try {
      const next = await ipc.cleanup.applyRippleCuts(project, cuts);
      onProjectChange(next);
      setGaps(null);
      setApproved(new Set());
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
    }
  }

  return (
    <section>
      <h2 className="mb-3 flex items-center gap-2 text-[var(--text-ui-lg)] font-semibold">
        <AudioLines size={16} className="text-[var(--color-accent-400)]" />{" "}
        {t("cleanupSilenceTitle")}
      </h2>
      <p className="mb-3 text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
        {t("cleanupSilenceIntro")}
      </p>

      <label className="mb-3 flex items-center gap-3 text-[var(--text-ui-sm)]">
        <input
          type="range"
          min={SILENCE_MIN_MS}
          max={SILENCE_MAX_MS}
          step={SILENCE_STEP_MS}
          value={minGapMs}
          onChange={(e) => {
            setMinGapMs(Number(e.target.value));
            setGaps(null);
            setApproved(new Set());
          }}
          className="flex-1 accent-[var(--color-accent-500)]"
        />
        <span className="w-40 shrink-0 text-right tabular-nums text-[var(--color-fg-muted)]">
          {t("cleanupSilenceThreshold", { n: minGapMs })}
        </span>
      </label>

      {error && (
        <p className="mb-3 text-[var(--text-ui-xs)] text-[var(--color-danger)]">
          {error}
        </p>
      )}

      {gaps === null ? (
        <button
          type="button"
          onClick={detect}
          className="rounded-md bg-[var(--color-bg-surface)] px-4 py-2 text-[var(--text-ui-sm)] font-medium hover:text-[var(--color-accent-400)]"
        >
          {t("cleanupFindSilences")}
        </button>
      ) : gaps.length === 0 ? (
        <p className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
          {t("cleanupNoSilences")}
        </p>
      ) : (
        <div className="space-y-3">
          <ul className="max-h-64 space-y-1 overflow-y-auto rounded-md border border-[var(--color-border)] p-2">
            {gaps.map((g, i) => (
              <li key={i}>
                <label className="flex cursor-pointer items-center gap-2 rounded px-2 py-1 text-[var(--text-ui-sm)] hover:bg-[var(--color-bg-surface)]">
                  <input
                    type="checkbox"
                    checked={approved.has(i)}
                    onChange={(e) => {
                      const next = new Set(approved);
                      if (e.target.checked) next.add(i);
                      else next.delete(i);
                      setApproved(next);
                    }}
                    className="accent-[var(--color-accent-500)]"
                  />
                  <span className="font-mono">
                    {t("cleanupSilenceGap", { n: g.duration_ms })}
                  </span>
                  <span className="ml-auto text-[10px] text-[var(--color-fg-subtle)]">
                    {(g.start_ms / 1000).toFixed(1)}s
                  </span>
                </label>
              </li>
            ))}
          </ul>
          <div className="flex items-center gap-3">
            <button
              type="button"
              onClick={removeApproved}
              className="flex items-center gap-1.5 rounded-md bg-[var(--color-accent-600)] px-4 py-2 text-[var(--text-ui-sm)] font-medium text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)]"
            >
              <Scissors size={14} />{" "}
              {t("cleanupRemoveSelected", { n: approved.size })}
            </button>
            <button
              type="button"
              onClick={() => {
                setGaps(null);
                setApproved(new Set());
              }}
              className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)] hover:text-[var(--color-fg)]"
            >
              {t("actionCancel")}
            </button>
            <span className="ml-auto flex items-center gap-1 text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
              <Check size={11} /> {t("cleanupFound", { n: gaps.length })}
            </span>
          </div>
        </div>
      )}
    </section>
  );
}

function Toggle({
  label,
  title,
  on,
  onChange,
}: {
  label: string;
  title: string;
  on: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <button
      type="button"
      title={title}
      onClick={() => onChange(!on)}
      className={cn(
        "rounded px-2 py-0.5 font-mono",
        on
          ? "bg-[var(--color-accent-500)] text-[var(--color-neutral-950)]"
          : "bg-[var(--color-bg-surface)] text-[var(--color-fg-muted)] hover:text-[var(--color-fg)]",
      )}
    >
      {label}
    </button>
  );
}
