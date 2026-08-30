/**
 * ExportConfigPanel — Oppgave 1.
 *
 * Configurable export pipeline settings persisted on the Project:
 *   - Default sidecar format (SRT / VTT / ASS)
 *   - Burn-in toggle
 *   - Subtitle style: size, colour, background
 *   - Max chars per line (32 / 42 / 52)
 *
 * State is lifted straight into the Project via `onProjectChange`, which App
 * wires to `useProjectStore.commit` under a coalescing key (the size and
 * chars-per-line controls fire in bursts). Persistence is the store's autosave
 * or the next manual save — see `src/lib/autosave.ts`.
 */

import { Settings2 } from "lucide-react";

import type {
  ExportConfig,
  KaraokeOptions,
  KaraokeStyle,
  Project,
} from "@/lib/bindings";
import { effectiveKaraokeOptions } from "./karaokeOptions";
import { useT } from "@/lib/i18n";
import { cn } from "@/lib/cn";

interface Props {
  project: Project;
  onProjectChange: (p: Project) => void;
}

function patch(project: Project, delta: Partial<ExportConfig>): Project {
  return {
    ...project,
    export_config: { ...project.export_config, ...delta },
  };
}

function patchKaraoke(
  project: Project,
  delta: Partial<KaraokeOptions>,
): Project {
  const current = effectiveKaraokeOptions(project.export_config);
  return patch(project, { karaoke: { ...current, ...delta } });
}

export function ExportConfigPanel({ project, onProjectChange }: Props) {
  const t = useT();
  const cfg = project.export_config;
  const karaoke = effectiveKaraokeOptions(cfg);

  return (
    <div className="space-y-6 p-4">
      <div className="flex items-center gap-2">
        <Settings2 size={16} className="text-[var(--color-accent-400)]" />
        <h3 className="text-[var(--text-ui-sm)] font-semibold">
          {t("exportConfigTitle")}
        </h3>
      </div>

      {/* ── Default sidecar format ── */}
      <section>
        <label className="mb-1.5 block text-[var(--text-ui-xs)] font-semibold uppercase tracking-wider text-[var(--color-fg-subtle)]">
          {t("exportConfigFormatLabel")}
        </label>
        <div className="flex gap-2">
          {(["srt", "vtt", "ass"] as const).map((fmt) => (
            <button
              key={fmt}
              type="button"
              onClick={() => onProjectChange(patch(project, { format: fmt }))}
              className={cn(
                "rounded-md border px-3 py-1.5 font-mono text-[var(--text-ui-xs)] font-semibold uppercase transition-colors",
                cfg.format === fmt
                  ? "border-[var(--color-accent-500)] bg-[var(--color-accent-500)]/12 text-[var(--color-accent-300)]"
                  : "border-[var(--color-border)] hover:border-[var(--color-border-strong)]",
              )}
            >
              {fmt}
            </button>
          ))}
        </div>
      </section>

      {/* ── Burn-in toggle ── */}
      <section>
        <label className="flex cursor-pointer items-center gap-3">
          <input
            type="checkbox"
            checked={cfg.burn_in}
            onChange={(e) =>
              onProjectChange(patch(project, { burn_in: e.target.checked }))
            }
            className="h-4 w-4 rounded border-[var(--color-border)] accent-[var(--color-accent-500)]"
          />
          <span className="text-[var(--text-ui-sm)]">
            {t("exportConfigBurnInLabel")}
          </span>
        </label>
        <p className="mt-1 pl-7 text-[10px] text-[var(--color-fg-muted)]">
          {t("exportConfigBurnInHint")}
        </p>
      </section>

      {/* ── Karaoke (E4a) — per-word `\k`/`\kf` highlighting. Read/written on
          `export_config.karaoke` so the sidecar `.ass`, the final burn-in
          and the live preview overlay all agree (KaraokeOptions doc). ── */}
      <section>
        <label className="flex cursor-pointer items-center gap-3">
          <input
            type="checkbox"
            checked={karaoke.enabled}
            onChange={(e) =>
              onProjectChange(
                patchKaraoke(project, { enabled: e.target.checked }),
              )
            }
            className="h-4 w-4 rounded border-[var(--color-border)] accent-[var(--color-accent-500)]"
          />
          <span className="text-[var(--text-ui-sm)]">
            {t("exportConfigKaraokeLabel")}
          </span>
        </label>
        <p className="mt-1 pl-7 text-[10px] text-[var(--color-fg-muted)]">
          {t("exportConfigKaraokeHint")}
        </p>

        {karaoke.enabled && (
          <div className="mt-3 pl-7">
            <label className="mb-1.5 block text-[var(--text-ui-xs)] font-semibold uppercase tracking-wider text-[var(--color-fg-subtle)]">
              {t("exportConfigKaraokeStyleLabel")}
            </label>
            <div className="flex gap-2">
              {(["highlight", "sweep"] as KaraokeStyle[]).map((s) => (
                <button
                  key={s}
                  type="button"
                  onClick={() =>
                    onProjectChange(patchKaraoke(project, { style: s }))
                  }
                  className={cn(
                    "rounded-md border px-3 py-1.5 text-[var(--text-ui-xs)] transition-colors",
                    karaoke.style === s
                      ? "border-[var(--color-accent-500)] bg-[var(--color-accent-500)]/12 text-[var(--color-accent-300)]"
                      : "border-[var(--color-border)] hover:border-[var(--color-border-strong)]",
                  )}
                >
                  {s === "highlight"
                    ? t("exportConfigKaraokeStyleHighlight")
                    : t("exportConfigKaraokeStyleSweep")}
                </button>
              ))}
            </div>

            <label className="mt-3 flex cursor-pointer items-center gap-3">
              <input
                type="checkbox"
                checked={karaoke.confidence_tint}
                onChange={(e) =>
                  onProjectChange(
                    patchKaraoke(project, {
                      confidence_tint: e.target.checked,
                    }),
                  )
                }
                className="h-4 w-4 rounded border-[var(--color-border)] accent-[var(--color-accent-500)]"
              />
              <span className="text-[var(--text-ui-sm)]">
                {t("exportConfigKaraokeTintLabel")}
              </span>
            </label>
            <p className="mt-1 pl-7 text-[10px] text-[var(--color-fg-muted)]">
              {t("exportConfigKaraokeTintHint")}
            </p>
          </div>
        )}
      </section>

      {/* ── Caption size ── */}
      <section>
        <label className="mb-1.5 block text-[var(--text-ui-xs)] font-semibold uppercase tracking-wider text-[var(--color-fg-subtle)]">
          {t("exportConfigSizeLabel")}
        </label>
        <div className="flex gap-2">
          {([16, 20, 24, 28] as const).map((px) => (
            <button
              key={px}
              type="button"
              onClick={() =>
                onProjectChange(patch(project, { caption_size_px: px }))
              }
              className={cn(
                "rounded-md border px-3 py-1.5 text-[var(--text-ui-xs)] transition-colors",
                cfg.caption_size_px === px
                  ? "border-[var(--color-accent-500)] bg-[var(--color-accent-500)]/12 text-[var(--color-accent-300)]"
                  : "border-[var(--color-border)] hover:border-[var(--color-border-strong)]",
              )}
            >
              {px}px
            </button>
          ))}
        </div>
      </section>

      {/* ── Caption colour ── */}
      <section>
        <label className="mb-1.5 block text-[var(--text-ui-xs)] font-semibold uppercase tracking-wider text-[var(--color-fg-subtle)]">
          {t("exportConfigColorLabel")}
        </label>
        <div className="flex gap-2">
          {(
            [
              {
                id: "white",
                hex: "#FFFFFF",
                label: t("exportConfigColorWhite"),
              },
              {
                id: "yellow",
                hex: "#FFD700",
                label: t("exportConfigColorYellow"),
              },
              {
                id: "green",
                hex: "#00FF88",
                label: t("exportConfigColorGreen"),
              },
            ] as const
          ).map(({ id, hex, label }) => (
            <button
              key={id}
              type="button"
              onClick={() =>
                onProjectChange(patch(project, { caption_color: id }))
              }
              title={label}
              aria-label={label}
              className={cn(
                "flex items-center gap-1.5 rounded-md border px-2.5 py-1.5 text-[var(--text-ui-xs)] transition-colors",
                cfg.caption_color === id
                  ? "border-[var(--color-accent-500)] bg-[var(--color-accent-500)]/12"
                  : "border-[var(--color-border)] hover:border-[var(--color-border-strong)]",
              )}
            >
              <span
                className="h-3 w-3 rounded-full border border-white/20"
                style={{ background: hex }}
              />
              {label}
            </button>
          ))}
        </div>
      </section>

      {/* ── Caption background ── */}
      <section>
        <label className="mb-1.5 block text-[var(--text-ui-xs)] font-semibold uppercase tracking-wider text-[var(--color-fg-subtle)]">
          {t("exportConfigBgLabel")}
        </label>
        <div className="flex gap-2">
          {(
            [
              { id: "black", label: t("exportConfigBgBlack") },
              { id: "semitransparent", label: t("exportConfigBgSemi") },
              { id: "none", label: t("exportConfigBgNone") },
            ] as const
          ).map(({ id, label }) => (
            <button
              key={id}
              type="button"
              onClick={() =>
                onProjectChange(patch(project, { caption_background: id }))
              }
              className={cn(
                "rounded-md border px-2.5 py-1.5 text-[var(--text-ui-xs)] transition-colors",
                cfg.caption_background === id
                  ? "border-[var(--color-accent-500)] bg-[var(--color-accent-500)]/12 text-[var(--color-accent-300)]"
                  : "border-[var(--color-border)] hover:border-[var(--color-border-strong)]",
              )}
            >
              {label}
            </button>
          ))}
        </div>
      </section>

      {/* ── Max chars per line ── */}
      <section>
        <label className="mb-1.5 block text-[var(--text-ui-xs)] font-semibold uppercase tracking-wider text-[var(--color-fg-subtle)]">
          {t("exportConfigCharsLabel")}
        </label>
        <div className="flex gap-2">
          {([32, 42, 52] as const).map((n) => (
            <button
              key={n}
              type="button"
              onClick={() =>
                onProjectChange(patch(project, { max_chars_per_line: n }))
              }
              className={cn(
                "rounded-md border px-3 py-1.5 text-[var(--text-ui-xs)] transition-colors",
                cfg.max_chars_per_line === n
                  ? "border-[var(--color-accent-500)] bg-[var(--color-accent-500)]/12 text-[var(--color-accent-300)]"
                  : "border-[var(--color-border)] hover:border-[var(--color-border-strong)]",
              )}
            >
              {n}
            </button>
          ))}
        </div>
        <p className="mt-1.5 text-[10px] text-[var(--color-fg-muted)]">
          {t("exportConfigCharsHint")}
        </p>
      </section>
    </div>
  );
}
