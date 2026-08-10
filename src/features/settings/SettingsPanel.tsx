/**
 * Settings — API keys (Phase 2.2).
 *
 * Keys are stored in the OS keychain by the backend. This panel can set,
 * clear, and see whether a key exists — it never receives a stored key back.
 */

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  KeyRound,
  Check,
  Trash2,
  Save,
  Languages,
  Highlighter,
  MonitorPlay,
} from "lucide-react";

import { ipc, IPCError } from "@/lib/ipc";
import type { SecretProvider } from "@/lib/bindings";
import { useCompositorFlag } from "@/features/timeline/compositor";
import { useT, useLocale, LANGS, LANG_NAMES, type TKey } from "@/lib/i18n";
import { cn } from "@/lib/cn";

const PROVIDERS: Array<{
  id: SecretProvider;
  label: string;
  noteKey: TKey;
}> = [
  {
    id: "anthropic",
    label: "Anthropic (Claude)",
    noteKey: "providerAnthropicNote",
  },
  { id: "open-ai", label: "OpenAI", noteKey: "providerCloudNote" },
  { id: "assembly-ai", label: "AssemblyAI", noteKey: "providerCloudNote" },
  { id: "deepgram", label: "Deepgram", noteKey: "providerCloudNote" },
];

export function SettingsPanel() {
  const t = useT();
  const statusQuery = useQuery({
    queryKey: ["secret-status"],
    queryFn: () => ipc.secrets.status(),
  });
  const status = statusQuery.data ?? [];
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [msg, setMsg] = useState<string | null>(null);

  const present = (id: SecretProvider) =>
    status.find((s) => s.provider === id)?.present ?? false;

  async function save(id: SecretProvider) {
    const value = (drafts[id] ?? "").trim();
    if (!value) return;
    setMsg(null);
    try {
      await ipc.secrets.set(id, value);
      setDrafts((d) => ({ ...d, [id]: "" }));
      await statusQuery.refetch();
      setMsg(t("settingsKeySaved"));
    } catch (e) {
      setMsg(
        e instanceof IPCError
          ? t("errorPrefix", { error: e.message })
          : String(e),
      );
    }
  }

  async function clear(id: SecretProvider) {
    setMsg(null);
    try {
      await ipc.secrets.delete(id);
      await statusQuery.refetch();
      setMsg(t("settingsKeyRemoved"));
    } catch (e) {
      setMsg(
        e instanceof IPCError
          ? t("errorPrefix", { error: e.message })
          : String(e),
      );
    }
  }

  return (
    <div className="mx-auto max-w-2xl p-6">
      <LanguagePicker />
      <PreviewSettings />

      <div className="mb-1 flex items-center gap-2">
        <KeyRound size={18} className="text-[var(--color-accent-400)]" />
        <h2 className="text-[var(--text-ui-lg)] font-semibold">
          {t("settingsApiKeys")}
        </h2>
      </div>
      <p className="mb-6 text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
        {t("settingsIntro")}
      </p>

      <ul className="space-y-3">
        {PROVIDERS.map((p) => {
          const isSet = present(p.id);
          return (
            <li
              key={p.id}
              className="rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-elevated)] p-3"
            >
              <div className="mb-2 flex items-center gap-2">
                <span className="text-[var(--text-ui-sm)] font-medium">
                  {p.label}
                </span>
                {isSet && (
                  <span className="flex items-center gap-1 rounded-full bg-[var(--color-success)]/15 px-2 py-0.5 text-[10px] text-[var(--color-success)]">
                    <Check size={10} /> {t("settingsSet")}
                  </span>
                )}
              </div>
              <p className="mb-2 text-[10px] text-[var(--color-fg-subtle)]">
                {t(p.noteKey)}
              </p>
              <div className="flex items-center gap-2">
                <input
                  type="password"
                  value={drafts[p.id] ?? ""}
                  placeholder={
                    isSet ? t("settingsKeyStored") : t("settingsKeyPlaceholder")
                  }
                  onChange={(e) =>
                    setDrafts((d) => ({ ...d, [p.id]: e.target.value }))
                  }
                  className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-bg-surface)] px-2.5 py-1.5 font-mono text-[var(--text-ui-sm)] outline-none focus:border-[var(--color-accent-500)]"
                />
                <button
                  type="button"
                  onClick={() => save(p.id)}
                  disabled={!(drafts[p.id] ?? "").trim()}
                  className="flex items-center gap-1.5 rounded-md bg-[var(--color-accent-600)] px-3 py-1.5 text-[var(--text-ui-xs)] font-semibold text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)] disabled:opacity-40"
                >
                  <Save size={12} /> {t("actionSave")}
                </button>
                {isSet && (
                  <button
                    type="button"
                    onClick={() => clear(p.id)}
                    title={t("settingsRemoveKey")}
                    aria-label={t("settingsRemoveKey")}
                    className="rounded-md p-1.5 text-[var(--color-fg-subtle)] hover:bg-[var(--color-danger)]/10 hover:text-[var(--color-danger)]"
                  >
                    <Trash2 size={14} />
                  </button>
                )}
              </div>
            </li>
          );
        })}
      </ul>

      {msg && (
        <p className="mt-4 text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
          {msg}
        </p>
      )}

      <AboutConfidence />
    </div>
  );
}

/**
 * The empirical backing for killer feature #1. The numbers come from
 * docs/CALIBRATION.md — flagging below the tier-2 floor (confidence 70)
 * catches 88% of errors at 100% precision on the calibration set. Surfacing
 * them is what lets users trust the colours instead of second-guessing them.
 */
const CONF_CALIBRATION = {
  floor: 70,
  recall: 88,
  precision: 0,
  miss: 1.3,
} as const;

function AboutConfidence() {
  const t = useT();
  return (
    <section className="mt-10">
      <div className="mb-1 flex items-center gap-2">
        <Highlighter size={18} className="text-[var(--color-accent-400)]" />
        <h2 className="text-[var(--text-ui-lg)] font-semibold">
          {t("settingsConfidenceTitle")}
        </h2>
      </div>
      <p className="mb-3 text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
        {t("settingsConfidenceIntro")}
      </p>
      <div className="rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-elevated)] p-3">
        <p className="text-[var(--text-ui-sm)]">
          {t("settingsConfidenceHeadline", {
            floor: CONF_CALIBRATION.floor,
            recall: CONF_CALIBRATION.recall,
            precision: CONF_CALIBRATION.precision,
            miss: CONF_CALIBRATION.miss,
          })}
        </p>
        <p className="mt-2 text-[10px] text-[var(--color-fg-subtle)]">
          {t("settingsConfidenceCaveat")}
        </p>
      </div>
    </section>
  );
}

/**
 * The GPU preview compositor's opt-in (E6; ADR-013).
 *
 * Two independent facts are shown: the SETTING (what the user chose,
 * persisted) and the RUNTIME (whether this machine can honour it). They are
 * never conflated — an automatic fallback leaves the checkbox where the user
 * put it and explains itself underneath, instead of appearing to flip itself.
 */
function PreviewSettings() {
  const t = useT();
  const enabled = useCompositorFlag((s) => s.enabled);
  const setEnabled = useCompositorFlag((s) => s.setEnabled);
  const unavailableReason = useCompositorFlag((s) => s.unavailableReason);

  const notice =
    enabled && unavailableReason !== null
      ? unavailableReason === "no-webgl2"
        ? t("settingsGpuUnavailableNoWebgl2")
        : t("settingsGpuUnavailableFailed")
      : null;

  return (
    <section className="mb-8">
      <div className="mb-1 flex items-center gap-2">
        <MonitorPlay size={18} className="text-[var(--color-accent-400)]" />
        <h2 className="text-[var(--text-ui-lg)] font-semibold">
          {t("settingsPreviewTitle")}
        </h2>
      </div>
      <p className="mb-3 text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
        {t("settingsPreviewIntro")}
      </p>
      <div className="rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-elevated)] p-3">
        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            data-testid="gpu-compositor-toggle"
            checked={enabled}
            onChange={(e) => setEnabled(e.target.checked)}
            className="accent-[var(--color-accent-500)]"
          />
          <span className="text-[var(--text-ui-sm)] font-medium">
            {t("settingsGpuCompositor")}
          </span>
        </label>
        <p className="mt-2 text-[10px] text-[var(--color-fg-subtle)]">
          {t("settingsGpuCompositorNote")}
        </p>
        {notice && (
          <p
            data-testid="gpu-compositor-unavailable"
            role="status"
            className="mt-2 text-[10px] text-[var(--color-fg-muted)]"
          >
            {notice}
          </p>
        )}
      </div>
    </section>
  );
}

function LanguagePicker() {
  const t = useT();
  const lang = useLocale((s) => s.lang);
  const setLang = useLocale((s) => s.setLang);
  return (
    <div className="mb-8">
      <div className="mb-1 flex items-center gap-2">
        <Languages size={18} className="text-[var(--color-accent-400)]" />
        <h2 className="text-[var(--text-ui-lg)] font-semibold">
          {t("settingsLanguage")}
        </h2>
      </div>
      <p className="mb-3 text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]">
        {t("settingsLanguageIntro")}
      </p>
      <div className="flex flex-wrap gap-2">
        {LANGS.map((l) => (
          <button
            key={l}
            type="button"
            onClick={() => setLang(l)}
            className={cn(
              "rounded-md border px-3 py-1.5 text-[var(--text-ui-sm)] transition-colors",
              l === lang
                ? "border-[var(--color-accent-500)] bg-[var(--color-accent-500)]/8 font-medium text-[var(--color-accent-300)]"
                : "border-[var(--color-border)] hover:border-[var(--color-border-strong)]",
            )}
          >
            {LANG_NAMES[l]}
          </button>
        ))}
      </div>
    </div>
  );
}
