/**
 * Context & glossary — killer feature #2 (Phase 3.4), modes 1 + 2.
 *
 * Mode 1: a free-text description of what the recording is about. Mode 2: a
 * glossary of proper nouns / jargon, each with the misrecognitions to
 * auto-correct. Both feed Whisper's initial_prompt before transcription
 * (priming) and the post-pass correction (applyGlossary) after. AI-suggested
 * glossary entries from a first-pass transcript (mode 3) are a later step.
 *
 * The project is the renderer-side source of truth, so edits here are plain
 * state updates via `onProjectChange`; the backend pass is invoked on demand.
 * App wires `onProjectChange` to `useProjectStore.commit` with a coalescing
 * key, so a typed description is ONE undo step rather than one per keystroke
 * (unkeyed, a long description would evict the 100-deep history the user's
 * real edits live in).
 */

import { useEffect, useState } from "react";
import {
  BookText,
  FileText,
  Plus,
  Trash2,
  Wand2,
  Sparkles,
  Loader2,
} from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

import { ipc, IPCError } from "@/lib/ipc";
import type {
  GlossaryTerm,
  PolishEstimate,
  Project,
  SuggestedTerm,
} from "@/lib/bindings";
import { useT } from "@/lib/i18n";
import { formatCost } from "@/lib/cost";

interface Props {
  project: Project;
  onProjectChange: (p: Project) => void;
}

export function ContextPanel({ project, onProjectChange }: Props) {
  const t = useT();
  const [applyMsg, setApplyMsg] = useState<string | null>(null);
  const [suggested, setSuggested] = useState<SuggestedTerm[] | null>(null);
  const [suggesting, setSuggesting] = useState(false);
  const [suggestMsg, setSuggestMsg] = useState<string | null>(null);
  const [estimate, setEstimate] = useState<PolishEstimate | null>(null);

  // Pure, no-network preview of the AI "suggest terms" pass — so the cost is
  // visible before spending. The suggest pass runs on Haiku (see runSuggest),
  // and refreshes whenever the transcript changes.
  useEffect(() => {
    let cancelled = false;
    ipc.glossary
      .estimate(project, "haiku45")
      .then((e) => !cancelled && setEstimate(e))
      .catch(() => !cancelled && setEstimate(null));
    return () => {
      cancelled = true;
    };
  }, [project]);

  function setContext(value: string) {
    onProjectChange({
      ...project,
      context_description: value.trim() === "" ? null : value,
    });
  }

  function updateTerm(id: string, patch: Partial<GlossaryTerm>) {
    onProjectChange({
      ...project,
      glossary: project.glossary.map((t) =>
        t.id === id ? { ...t, ...patch } : t,
      ),
    });
  }

  function addTerm() {
    const term: GlossaryTerm = {
      id: crypto.randomUUID(),
      term: "",
      aliases: [],
      definition: null,
      pronunciation_hint: null,
    };
    onProjectChange({ ...project, glossary: [...project.glossary, term] });
  }

  function removeTerm(id: string) {
    onProjectChange({
      ...project,
      glossary: project.glossary.filter((t) => t.id !== id),
    });
  }

  async function applyNow() {
    setApplyMsg(null);
    try {
      const res = await ipc.ops.applyGlossary(project);
      onProjectChange(res.project);
      setApplyMsg(
        res.corrections.length === 0
          ? t("contextNoTermsToCorrect")
          : t("contextCorrected", { n: res.corrections.length }),
      );
    } catch (e) {
      setApplyMsg(
        e instanceof IPCError
          ? t("errorPrefix", { error: e.message })
          : String(e),
      );
    }
  }

  // Killer feature #2, mode 3: ask the LLM to propose glossary terms from the
  // transcript. Propose-and-approve — nothing is added until the user accepts.
  async function runSuggest() {
    setSuggesting(true);
    setSuggestMsg(null);
    setSuggested(null);
    try {
      const out = await ipc.glossary.suggest(project, "haiku45");
      setSuggested(out);
      if (out.length === 0) setSuggestMsg(t("contextNoNewTerms"));
    } catch (e) {
      // Most likely: no Anthropic key set.
      setSuggestMsg(
        e instanceof IPCError
          ? t("contextSuggestError", { error: e.message })
          : String(e),
      );
    } finally {
      setSuggesting(false);
    }
  }

  // Killer feature #2, mode 4: seed the glossary from a reference document the
  // speaker is working from (script, manuscript, notes) — runnable *before*
  // transcription. Same propose-and-approve review queue as mode 3.
  async function runFromDocument() {
    const selected = await openDialog({
      multiple: false,
      filters: [
        {
          name: t("contextDocFilterName"),
          extensions: ["txt", "md", "markdown", "docx"],
        },
      ],
    });
    if (typeof selected !== "string") return; // cancelled

    setSuggesting(true);
    setSuggestMsg(null);
    setSuggested(null);
    try {
      const doc = await ipc.glossary.extractDocument(selected);
      const out = await ipc.glossary.suggestFromDocument(
        project,
        "haiku45",
        doc.text,
      );
      setSuggested(out);
      const note = doc.truncated
        ? " " + t("contextDocTruncatedNote", { n: doc.char_count })
        : "";
      if (out.length === 0) setSuggestMsg(t("contextNoNewTerms") + note);
      else if (doc.truncated) setSuggestMsg(note.trim());
    } catch (e) {
      setSuggestMsg(
        e instanceof IPCError
          ? t("contextSuggestError", { error: e.message })
          : String(e),
      );
    } finally {
      setSuggesting(false);
    }
  }

  function acceptSuggestion(s: SuggestedTerm) {
    const exists = project.glossary.some(
      (t) => t.term.toLowerCase() === s.term.toLowerCase(),
    );
    if (!exists) {
      const term: GlossaryTerm = {
        id: crypto.randomUUID(),
        term: s.term,
        aliases: s.aliases,
        definition: s.reason.trim() || null,
        pronunciation_hint: null,
      };
      onProjectChange({ ...project, glossary: [...project.glossary, term] });
    }
    setSuggested((cur) => cur?.filter((t) => t !== s) ?? null);
  }

  function dismissSuggestion(s: SuggestedTerm) {
    setSuggested((cur) => cur?.filter((t) => t !== s) ?? null);
  }

  return (
    <div className="mx-auto max-w-2xl p-6">
      <div className="mb-1 flex items-center gap-2">
        <BookText size={18} className="text-[var(--color-accent-400)]" />
        <h2 className="text-[var(--text-ui-lg)] font-semibold">
          {t("navContext")}
        </h2>
      </div>
      <p className="mb-6 text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
        {t("contextIntro")}
      </p>

      {/* Mode 1 — free-text context */}
      <label className="mb-2 block text-[var(--text-ui-sm)] font-medium">
        {t("contextDescriptionLabel")}
      </label>
      <textarea
        value={project.context_description ?? ""}
        onChange={(e) => setContext(e.target.value)}
        rows={3}
        placeholder={t("contextDescriptionPlaceholder")}
        className="w-full resize-y rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-elevated)] px-3 py-2 text-[var(--text-ui-sm)] outline-none focus:border-[var(--color-accent-500)]"
      />

      {/* Mode 2 — glossary */}
      <div className="mb-2 mt-6 flex items-center justify-between">
        <label className="text-[var(--text-ui-sm)] font-medium">
          {t("contextGlossaryLabel", { n: project.glossary.length })}
        </label>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={runSuggest}
            disabled={suggesting}
            title={t("contextSuggestTitle")}
            className="flex items-center gap-1.5 rounded-md border border-[var(--color-border)] px-2.5 py-1 text-[var(--text-ui-xs)] font-medium hover:border-[var(--color-accent-600)] disabled:opacity-50"
          >
            {suggesting ? (
              <Loader2 size={12} className="animate-spin" />
            ) : (
              <Sparkles size={12} />
            )}
            {t("contextSuggestTerms")}
          </button>
          <button
            type="button"
            onClick={runFromDocument}
            disabled={suggesting}
            title={t("contextFromDocumentTitle")}
            className="flex items-center gap-1.5 rounded-md border border-[var(--color-border)] px-2.5 py-1 text-[var(--text-ui-xs)] font-medium hover:border-[var(--color-accent-600)] disabled:opacity-50"
          >
            <FileText size={12} />
            {t("contextFromDocument")}
          </button>
          <button
            type="button"
            onClick={addTerm}
            className="flex items-center gap-1.5 rounded-md border border-[var(--color-border)] px-2.5 py-1 text-[var(--text-ui-xs)] font-medium hover:border-[var(--color-accent-600)]"
          >
            <Plus size={12} /> {t("contextAddTerm")}
          </button>
        </div>
      </div>

      {/* Pre-run cost preview for the AI "suggest terms" pass. */}
      {estimate && estimate.caption_count > 0 && (
        <p className="mb-3 text-[var(--text-ui-xs)] text-[var(--color-fg-muted)]">
          {t("contextSuggestEstimate", {
            n: estimate.caption_count,
            cost: formatCost(estimate.estimated_cost_usd),
          })}
        </p>
      )}

      {/* AI suggestion review queue — accept adds to the glossary above. */}
      {suggestMsg && (
        <p className="mb-3 text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
          {suggestMsg}
        </p>
      )}
      {suggested && suggested.length > 0 && (
        <div className="mb-4 rounded-lg border border-[var(--color-accent-600)]/40 bg-[var(--color-accent-500)]/5 p-3">
          <p className="mb-2 flex items-center gap-1.5 text-[var(--text-ui-xs)] font-semibold text-[var(--color-accent-300)]">
            <Sparkles size={12} />{" "}
            {t("contextSuggestionsHeader", { n: suggested.length })}
          </p>
          <ul className="space-y-2">
            {suggested.map((s, i) => (
              <li
                key={`${s.term}-${i}`}
                className="flex items-start gap-2 rounded-md bg-[var(--color-bg-elevated)] px-3 py-2"
              >
                <div className="flex-1">
                  <span className="font-mono text-[var(--text-ui-sm)] font-semibold">
                    {s.term}
                  </span>
                  {s.aliases.length > 0 && (
                    <span className="ml-2 text-[10px] text-[var(--color-fg-subtle)]">
                      ↔ {s.aliases.join(", ")}
                    </span>
                  )}
                  {s.reason && (
                    <p className="mt-0.5 text-[10px] text-[var(--color-fg-muted)]">
                      {s.reason}
                    </p>
                  )}
                </div>
                <button
                  type="button"
                  onClick={() => acceptSuggestion(s)}
                  className="rounded-md bg-[var(--color-accent-600)] px-2.5 py-1 text-[10px] font-semibold text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)]"
                >
                  {t("actionAdd")}
                </button>
                <button
                  type="button"
                  onClick={() => dismissSuggestion(s)}
                  title={t("contextDismiss")}
                  aria-label={t("contextDismiss")}
                  className="rounded-md p-1 text-[var(--color-fg-subtle)] hover:text-[var(--color-fg)]"
                >
                  <Trash2 size={13} />
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}

      {project.glossary.length === 0 ? (
        <p className="rounded-lg border border-dashed border-[var(--color-border)] px-3 py-6 text-center text-[var(--text-ui-sm)] text-[var(--color-fg-subtle)]">
          {t("contextNoTermsYet")}
        </p>
      ) : (
        <ul className="space-y-3">
          {project.glossary.map((term) => (
            <li
              key={term.id}
              className="rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-elevated)] p-3"
            >
              <div className="flex items-start gap-2">
                <div className="grid flex-1 grid-cols-2 gap-2">
                  <Field
                    label={t("contextFieldTerm")}
                    value={term.term}
                    onChange={(v) => updateTerm(term.id, { term: v })}
                    placeholder="kerygma"
                  />
                  <Field
                    label={t("contextFieldAliases")}
                    value={term.aliases.join(", ")}
                    onChange={(v) =>
                      updateTerm(term.id, {
                        aliases: v
                          .split(",")
                          .map((a) => a.trim())
                          .filter(Boolean),
                      })
                    }
                    placeholder="kerigma, kerygmae"
                  />
                  <Field
                    label={t("contextFieldDefinition")}
                    value={term.definition ?? ""}
                    onChange={(v) =>
                      updateTerm(term.id, { definition: v.trim() || null })
                    }
                    placeholder="forkynnelsen av evangeliet"
                  />
                  <Field
                    label={t("contextFieldPronunciation")}
                    value={term.pronunciation_hint ?? ""}
                    onChange={(v) =>
                      updateTerm(term.id, {
                        pronunciation_hint: v.trim() || null,
                      })
                    }
                    placeholder="ke-ROOG-ma"
                  />
                </div>
                <button
                  type="button"
                  onClick={() => removeTerm(term.id)}
                  title={t("contextRemoveTerm")}
                  aria-label={t("contextRemoveTerm")}
                  className="mt-5 shrink-0 rounded-md p-1.5 text-[var(--color-fg-subtle)] hover:bg-[var(--color-danger)]/10 hover:text-[var(--color-danger)]"
                >
                  <Trash2 size={14} />
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}

      <div className="mt-6 flex items-center gap-3">
        <button
          type="button"
          onClick={applyNow}
          disabled={project.glossary.length === 0}
          className="flex items-center gap-2 rounded-lg bg-[var(--color-accent-600)] px-4 py-2 text-[var(--text-ui-sm)] font-semibold text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)] disabled:opacity-50"
        >
          <Wand2 size={14} /> {t("contextApplyNow")}
        </button>
        {applyMsg && (
          <span className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
            {applyMsg}
          </span>
        )}
      </div>
    </div>
  );
}

function Field({
  label,
  value,
  onChange,
  placeholder,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <label className="flex flex-col gap-1">
      <span className="text-[10px] uppercase tracking-wider text-[var(--color-fg-subtle)]">
        {label}
      </span>
      <input
        type="text"
        value={value}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
        className="rounded-md border border-[var(--color-border)] bg-[var(--color-bg-surface)] px-2.5 py-1.5 text-[var(--text-ui-sm)] outline-none focus:border-[var(--color-accent-500)]"
      />
    </label>
  );
}
