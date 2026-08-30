/**
 * Speakers panel — Phase 4.2.
 *
 * Detect who-said-what, then manage the roster: rename, recolour, and merge
 * speakers diarization split apart. Diarization is best-effort, so the panel
 * says so up front and the result is fully editable before export.
 *
 * "Detect speakers" needs extracted audio (project.audio_wav_path); without
 * it — or in a build without the diarize sidecar — the call returns a clear
 * error shown inline. Roster edits are pure round-trips through the backend,
 * committed through `onProjectChange` (= `useProjectStore.commit`), so a
 * detection run and each rename/recolour/merge are undoable steps.
 */

import { useState } from "react";
import { Users, Info, Merge } from "lucide-react";

import { ipc, IPCError } from "@/lib/ipc";
import type { Project } from "@/lib/bindings";
import { useT } from "@/lib/i18n";

interface Props {
  project: Project;
  onProjectChange: (project: Project) => void;
}

export function SpeakersPanel({ project, onProjectChange }: Props) {
  const t = useT();
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const counts = new Map<string, number>();
  for (const c of project.captions) {
    if (c.speaker_id)
      counts.set(c.speaker_id, (counts.get(c.speaker_id) ?? 0) + 1);
  }

  async function detect() {
    setError(null);
    if (!project.audio_wav_path) {
      setError(t("speakersNoAudio"));
      return;
    }
    setRunning(true);
    try {
      const next = await ipc.diarize.run(project, project.audio_wav_path);
      onProjectChange(next);
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
    } finally {
      setRunning(false);
    }
  }

  async function rename(id: string, name: string) {
    try {
      onProjectChange(await ipc.diarize.renameSpeaker(project, id, name));
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
    }
  }
  async function setColor(id: string, color: string) {
    try {
      onProjectChange(await ipc.diarize.setSpeakerColor(project, id, color));
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
    }
  }
  async function merge(removeId: string, keepId: string) {
    try {
      onProjectChange(
        await ipc.diarize.mergeSpeakers(project, keepId, removeId),
      );
    } catch (e) {
      setError(e instanceof IPCError ? e.message : String(e));
    }
  }

  return (
    <div className="mx-auto max-w-2xl space-y-6 p-6">
      <header>
        <h2 className="mb-1 flex items-center gap-2 text-[var(--text-ui-lg)] font-semibold">
          <Users size={16} className="text-[var(--color-accent-400)]" />{" "}
          {t("navSpeakers")}
        </h2>
        <p className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
          {t("speakersIntro")}
        </p>
      </header>

      <div className="flex items-start gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-bg-surface)] px-3 py-2 text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
        <Info
          size={15}
          className="mt-0.5 shrink-0 text-[var(--color-accent-400)]"
        />
        <span>{t("speakersDisclaimer")}</span>
      </div>

      <button
        type="button"
        onClick={detect}
        disabled={running}
        className="flex items-center gap-1.5 rounded-md bg-[var(--color-accent-600)] px-4 py-2 text-[var(--text-ui-sm)] font-medium text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)] disabled:opacity-50"
      >
        <Users size={14} />{" "}
        {running ? t("speakersDetecting") : t("speakersDetect")}
      </button>

      {error && (
        <p className="rounded-md border border-[var(--color-danger)]/40 bg-[var(--color-danger)]/10 px-3 py-2 text-[var(--text-ui-sm)] text-[var(--color-danger)]">
          {error}
        </p>
      )}

      {project.speakers.length === 0 ? (
        <p className="text-[var(--text-ui-sm)] text-[var(--color-fg-subtle)]">
          {t("speakersNoneYet")}
        </p>
      ) : (
        <ul className="space-y-2">
          {project.speakers.map((s) => (
            <li
              key={s.id}
              className="flex items-center gap-3 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-surface)] p-3"
            >
              <input
                type="color"
                value={s.color_hex ?? "#888888"}
                onChange={(e) => setColor(s.id, e.target.value)}
                title={t("speakersChangeColor")}
                className="h-6 w-6 shrink-0 cursor-pointer rounded border-0 bg-transparent p-0"
              />
              <input
                defaultValue={s.display_name}
                onBlur={(e) => {
                  if (
                    e.target.value.trim() &&
                    e.target.value !== s.display_name
                  )
                    rename(s.id, e.target.value.trim());
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                }}
                className="flex-1 rounded border border-transparent bg-transparent px-1 py-0.5 text-[var(--text-ui-sm)] font-medium outline-none hover:border-[var(--color-border)] focus:border-[var(--color-accent-500)]"
              />
              <span className="text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
                {t("clipsCaptionsCount", { n: counts.get(s.id) ?? 0 })}
              </span>
              {project.speakers.length > 1 && (
                <label className="flex items-center gap-1 text-[var(--text-ui-xs)] text-[var(--color-fg-muted)]">
                  <Merge size={12} />
                  <select
                    value=""
                    onChange={(e) => {
                      if (e.target.value) merge(s.id, e.target.value);
                    }}
                    title={t("speakersMergeTitle")}
                    className="rounded border border-[var(--color-border)] bg-[var(--color-bg-input)] px-1 py-0.5 outline-none focus:border-[var(--color-accent-500)]"
                  >
                    <option value="">{t("speakersMergePlaceholder")}</option>
                    {project.speakers
                      .filter((o) => o.id !== s.id)
                      .map((o) => (
                        <option key={o.id} value={o.id}>
                          → {o.display_name}
                        </option>
                      ))}
                  </select>
                </label>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
