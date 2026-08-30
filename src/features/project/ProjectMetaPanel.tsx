/**
 * ProjectMetaPanel — Oppgave 2.
 *
 * Editable project metadata:
 *   - Prosjekt-tittel
 *   - Videobeskrivelse (brukes som AI-kontekst)
 *   - Egennavn/glossar (komma-separert liste for Whisper-priming)
 *   - Språk (auto-detect / no / en / sv / da / de / fr / pl)
 *
 * Lives as a dock tool on the right rail. Changes are lifted into the Project
 * via `onProjectChange`, which App wires to `useProjectStore.commit` under a
 * coalescing key: every field here is a text input firing per keystroke, so a
 * typed burst folds into one undo step. Persistence is the store's autosave
 * (or the next manual save) — see `src/lib/autosave.ts`.
 */

import { FileText } from "lucide-react";

import type { Project, ProjectMeta } from "@/lib/bindings";
import { LANGS, LANG_NAMES, useT } from "@/lib/i18n";

interface Props {
  project: Project;
  onProjectChange: (p: Project) => void;
}

function patchMeta(project: Project, delta: Partial<ProjectMeta>): Project {
  return {
    ...project,
    project_meta: { ...project.project_meta, ...delta },
  };
}

export function ProjectMetaPanel({ project, onProjectChange }: Props) {
  const t = useT();
  const meta = project.project_meta;

  return (
    <div className="mx-auto max-w-2xl p-6">
      <div className="mb-1 flex items-center gap-2">
        <FileText size={18} className="text-[var(--color-accent-400)]" />
        <h2 className="text-[var(--text-ui-lg)] font-semibold">
          {t("navProjectMeta")}
        </h2>
      </div>
      <p className="mb-6 text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
        {t("projectMetaIntro")}
      </p>

      {/* ── Title ── */}
      <label className="mb-4 block">
        <span className="mb-1.5 block text-[var(--text-ui-sm)] font-medium">
          {t("projectMetaTitleLabel")}
        </span>
        <input
          type="text"
          value={meta.title}
          onChange={(e) =>
            onProjectChange(patchMeta(project, { title: e.target.value }))
          }
          placeholder={project.name}
          className="w-full rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-elevated)] px-3 py-2 text-[var(--text-ui-sm)] outline-none focus:border-[var(--color-accent-500)]"
        />
        <p className="mt-1 text-[10px] text-[var(--color-fg-muted)]">
          {t("projectMetaTitleHint")}
        </p>
      </label>

      {/* ── Description ── */}
      <label className="mb-4 block">
        <span className="mb-1.5 block text-[var(--text-ui-sm)] font-medium">
          {t("projectMetaDescLabel")}
        </span>
        <textarea
          value={meta.description}
          onChange={(e) =>
            onProjectChange(patchMeta(project, { description: e.target.value }))
          }
          rows={4}
          placeholder={t("projectMetaDescPlaceholder")}
          className="w-full resize-y rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-elevated)] px-3 py-2 text-[var(--text-ui-sm)] outline-none focus:border-[var(--color-accent-500)]"
        />
        <p className="mt-1 text-[10px] text-[var(--color-fg-muted)]">
          {t("projectMetaDescHint")}
        </p>
      </label>

      {/* ── Proper nouns / glossary priming ── */}
      <label className="mb-4 block">
        <span className="mb-1.5 block text-[var(--text-ui-sm)] font-medium">
          {t("projectMetaNounsLabel")}
        </span>
        <input
          type="text"
          value={meta.proper_nouns}
          onChange={(e) =>
            onProjectChange(
              patchMeta(project, { proper_nouns: e.target.value }),
            )
          }
          placeholder={t("projectMetaNounsPlaceholder")}
          className="w-full rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-elevated)] px-3 py-2 text-[var(--text-ui-sm)] font-mono outline-none focus:border-[var(--color-accent-500)]"
        />
        <p className="mt-1 text-[10px] text-[var(--color-fg-muted)]">
          {t("projectMetaNounsHint")}
        </p>
      </label>

      {/* ── Language ── */}
      <label className="block">
        <span className="mb-1.5 block text-[var(--text-ui-sm)] font-medium">
          {t("projectMetaLanguageLabel")}
        </span>
        <select
          value={meta.language}
          onChange={(e) =>
            onProjectChange(patchMeta(project, { language: e.target.value }))
          }
          className="w-full rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-elevated)] px-3 py-2 text-[var(--text-ui-sm)] outline-none focus:border-[var(--color-accent-500)]"
        >
          <option value="auto">{t("projectMetaLanguageAuto")}</option>
          {LANGS.map((lang) => (
            <option key={lang} value={lang}>
              {LANG_NAMES[lang]}
            </option>
          ))}
        </select>
        <p className="mt-1 text-[10px] text-[var(--color-fg-muted)]">
          {t("projectMetaLanguageHint")}
        </p>
      </label>
    </div>
  );
}
