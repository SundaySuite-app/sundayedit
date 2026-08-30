/**
 * Multi-track compose export — flatten the whole timeline (every video
 * / audio / caption / overlay track) into one MP4 via the ffmpeg
 * `filter_complex` compose engine.
 *
 * This is an ADDED export path that sits alongside the sidecar + burn-in exports
 * in `ExportPanel`; it never touches them. The flow:
 *   pick output (save dialog) → build ComposeSettings from the project geometry,
 *   codec/encoder from the picker below → `ipc.compose.render` → a fixed
 *   progress overlay driven by the `compose-render-progress` event, with a
 *   Cancel button (`ipc.compose.cancel`).
 *
 * The encoder picker exists because a 60-minute timeline is minutes on a
 * hardware encoder versus close to an hour unconditionally on `libx264` (CPU)
 * — the gap this export path used to default into silently. It seeds from
 * `detectDefaultEncoder` (the same hardware-aware pick the burn-in/preset
 * export path makes) but the user can always override it, "CPU (most
 * compatible)" included, for a machine where the hardware encoder is flaky or
 * a file that must open everywhere.
 *
 * Everything here assumes Tauri; the caller guards mounting behind `isTauri()`.
 */

import { useEffect, useRef, useState } from "react";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { Clapperboard, Loader2, X, CheckCircle2 } from "lucide-react";

import { ipc, IPCError } from "@/lib/ipc";
import type { Encoder, Project, VideoCodec } from "@/lib/bindings";
import {
  defaultComposeSettings,
  detectDefaultEncoder,
  subscribeComposeProgress,
} from "@/lib/composeEngine";
import { useT } from "@/lib/i18n";
import { cn } from "@/lib/cn";

type Phase =
  | { kind: "idle" }
  | { kind: "rendering"; percent: number; cancelling: boolean }
  | { kind: "done"; path: string }
  | { kind: "cancelled" }
  | { kind: "error"; message: string };

export function ComposeExport({ project }: { project: Project }) {
  const t = useT();
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  // Live progress subscription; torn down on unmount / when the render settles.
  const unsubRef = useRef<(() => void) | null>(null);
  // Set when the user clicks Cancel. The Rust side rejects the render future on
  // cancel, so without this flag a user cancel is indistinguishable from a real
  // failure unless we sniff the message — the flag makes the calm "cancelled"
  // state robust even if the backend's error text changes.
  const cancelRequestedRef = useRef(false);

  // Codec/encoder the user picked, seeded from the platform's hardware-aware
  // default (see the module doc comment). Starts at the universal-fallback
  // "cpu" for the one render before `detectDefaultEncoder` resolves — a user
  // clicking export in that split second still gets a correct, if slower,
  // render, never a broken one.
  const [codec, setCodec] = useState<VideoCodec>("h264");
  const [encoder, setEncoder] = useState<Encoder>("cpu");

  useEffect(() => {
    let cancelled = false;
    void detectDefaultEncoder().then((e) => {
      if (!cancelled) setEncoder(e);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => () => unsubRef.current?.(), []);

  async function doExport() {
    const base = project.name.replace(/\.[^.]+$/, "");
    const out = await saveDialog({
      defaultPath: `${base}_composed.mp4`,
      filters: [{ name: "Video", extensions: ["mp4"] }],
    });
    if (typeof out !== "string") return; // cancelled the dialog

    cancelRequestedRef.current = false;
    setPhase({ kind: "rendering", percent: 0, cancelling: false });
    unsubRef.current = subscribeComposeProgress((p) => {
      const pct = Math.round((p.fraction ?? 0) * 100);
      setPhase((cur) =>
        cur.kind === "rendering"
          ? { ...cur, percent: Math.max(cur.percent, pct) }
          : cur,
      );
    });

    try {
      // Geometry/fps come from the project; codec/encoder come from the
      // picker below — see `defaultComposeSettings`'s own doc comment for why
      // that function stays a pure "h264"/"cpu" baseline instead of reaching
      // for the platform pick itself.
      await ipc.compose.render(project, out, {
        ...defaultComposeSettings(project),
        codec,
        encoder,
      });
      setPhase({ kind: "done", path: out });
    } catch (e) {
      const message =
        e instanceof IPCError
          ? e.message
          : e instanceof Error
            ? e.message
            : String(e);
      // A user-triggered cancel surfaces as a rejection of the render future
      // (Rust: "compose render cancelled"). The local flag is authoritative;
      // the message sniff covers a cancel initiated elsewhere. Either way it's
      // a calm, distinct state — never the error banner.
      setPhase(
        cancelRequestedRef.current || /cancel/i.test(message)
          ? { kind: "cancelled" }
          : { kind: "error", message },
      );
    } finally {
      unsubRef.current?.();
      unsubRef.current = null;
    }
  }

  function cancel() {
    cancelRequestedRef.current = true;
    setPhase((cur) =>
      cur.kind === "rendering" ? { ...cur, cancelling: true } : cur,
    );
    void ipc.compose.cancel().catch(() => {});
  }

  return (
    <>
      <button
        type="button"
        onClick={() => void doExport()}
        className="mt-2 flex w-full items-center gap-2 rounded-md border border-[var(--color-accent-500)]/50 bg-[var(--color-accent-500)]/8 px-3 py-2 text-left text-[var(--color-accent-300)] transition-colors hover:border-[var(--color-accent-500)] hover:bg-[var(--color-accent-500)]/12"
      >
        <Clapperboard size={14} className="shrink-0" />
        <span className="flex flex-col">
          <span className="text-[var(--text-ui-xs)] font-semibold">
            {t("exportComposeAction")}
          </span>
          <span className="text-[10px] text-[var(--color-fg-muted)]">
            {t("exportComposeDesc")}
          </span>
        </span>
      </button>

      {/* Encoder/codec picker — see the module doc comment. Defaults to the
          hardware-aware pick (once `detectDefaultEncoder` resolves) but
          always leaves "CPU (most compatible)" reachable. */}
      <div className="mt-2 space-y-2 rounded-md border border-[var(--color-border)] p-2">
        <div className="flex items-center justify-between gap-2">
          <span className="text-[10px] font-semibold uppercase tracking-wider text-[var(--color-fg-subtle)]">
            {t("composeCodecLabel")}
          </span>
          <div className="flex gap-1">
            {(["h264", "h265"] as VideoCodec[]).map((c) => (
              <button
                key={c}
                type="button"
                data-testid={`compose-codec-${c}`}
                aria-pressed={codec === c}
                onClick={() => setCodec(c)}
                className={cn(
                  "rounded border px-2 py-1 font-mono text-[10px] font-semibold uppercase transition-colors",
                  codec === c
                    ? "border-[var(--color-accent-500)] bg-[var(--color-accent-500)]/12 text-[var(--color-accent-300)]"
                    : "border-[var(--color-border)] text-[var(--color-fg-muted)] hover:border-[var(--color-border-strong)]",
                )}
              >
                {c === "h264" ? "H.264" : "H.265"}
              </button>
            ))}
          </div>
        </div>
        <div className="flex items-center justify-between gap-2">
          <label
            htmlFor="compose-encoder-select"
            className="text-[10px] font-semibold uppercase tracking-wider text-[var(--color-fg-subtle)]"
          >
            {t("composeEncoderLabel")}
          </label>
          <select
            id="compose-encoder-select"
            data-testid="compose-encoder-select"
            value={encoder}
            onChange={(e) => setEncoder(e.target.value as Encoder)}
            className="rounded border border-[var(--color-border)] bg-[var(--color-bg-elevated)] px-2 py-1 text-[10px] text-[var(--color-fg)]"
          >
            <option value="cpu">{t("composeEncoderCpu")}</option>
            <option value="video-toolbox">
              {t("composeEncoderVideoToolbox")}
            </option>
            <option value="nvenc">{t("composeEncoderNvenc")}</option>
            <option value="quick-sync">{t("composeEncoderQuickSync")}</option>
          </select>
        </div>
      </div>

      {phase.kind !== "idle" && (
        <div
          role="dialog"
          aria-label={t("composeProgressTitle")}
          data-testid="compose-progress"
          className="fixed inset-0 z-[60] grid place-items-center bg-black/60 p-6"
        >
          <div className="w-full max-w-md rounded-xl border border-[var(--color-border)] bg-[var(--color-bg-elevated)] p-6 shadow-2xl">
            <div className="mb-4 flex items-center gap-2">
              <Clapperboard
                size={16}
                className="text-[var(--color-accent-400)]"
              />
              <h3 className="text-[var(--text-ui-md)] font-semibold">
                {t("composeProgressTitle")}
              </h3>
            </div>

            {phase.kind === "rendering" && (
              <>
                <div className="h-2 w-full overflow-hidden rounded-full bg-[var(--color-bg-surface)]">
                  <div
                    className="h-full rounded-full bg-[var(--color-accent-500)] transition-[width]"
                    style={{ width: `${phase.percent}%` }}
                    data-testid="compose-progress-bar"
                  />
                </div>
                <div className="mt-2 flex items-center justify-between">
                  <span className="font-mono text-[var(--text-ui-sm)] tabular-nums text-[var(--color-fg-muted)]">
                    {phase.percent}%
                  </span>
                  <button
                    type="button"
                    onClick={cancel}
                    disabled={phase.cancelling}
                    className="inline-flex items-center gap-1.5 rounded-md border border-[var(--color-border)] px-3 py-1.5 text-[var(--text-ui-sm)] font-medium text-[var(--color-fg-muted)] hover:text-[var(--color-fg)] disabled:opacity-50"
                  >
                    {phase.cancelling ? (
                      <Loader2 size={13} className="animate-spin" />
                    ) : (
                      <X size={13} />
                    )}
                    {phase.cancelling
                      ? t("composeCancelling")
                      : t("composeCancel")}
                  </button>
                </div>
              </>
            )}

            {phase.kind === "done" && (
              <>
                <p className="flex items-start gap-2 text-[var(--text-ui-sm)] text-[var(--color-fg)]">
                  <CheckCircle2
                    size={16}
                    className="mt-0.5 shrink-0 text-[var(--color-success)]"
                  />
                  <span data-testid="compose-done">
                    {t("composeDone", { path: phase.path })}
                  </span>
                </p>
                <div className="mt-4 flex justify-end">
                  <button
                    type="button"
                    onClick={() => setPhase({ kind: "idle" })}
                    className="rounded-md bg-[var(--color-accent-600)] px-4 py-1.5 text-[var(--text-ui-sm)] font-semibold text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)]"
                  >
                    {t("composeClose")}
                  </button>
                </div>
              </>
            )}

            {phase.kind === "cancelled" && (
              <>
                <p
                  className="flex items-start gap-2 text-[var(--text-ui-sm)] text-[var(--color-fg)]"
                  data-testid="compose-cancelled"
                >
                  <X
                    size={16}
                    className="mt-0.5 shrink-0 text-[var(--color-fg-muted)]"
                  />
                  {t("composeCancelled")}
                </p>
                <div className="mt-4 flex justify-end">
                  <button
                    type="button"
                    onClick={() => setPhase({ kind: "idle" })}
                    className="rounded-md border border-[var(--color-border)] px-4 py-1.5 text-[var(--text-ui-sm)] font-medium text-[var(--color-fg-muted)] hover:text-[var(--color-fg)]"
                  >
                    {t("composeClose")}
                  </button>
                </div>
              </>
            )}

            {phase.kind === "error" && (
              <>
                <p
                  className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]"
                  data-testid="compose-error"
                >
                  {phase.message}
                </p>
                <div className="mt-4 flex justify-end">
                  <button
                    type="button"
                    onClick={() => setPhase({ kind: "idle" })}
                    className="rounded-md border border-[var(--color-border)] px-4 py-1.5 text-[var(--text-ui-sm)] font-medium text-[var(--color-fg-muted)] hover:text-[var(--color-fg)]"
                  >
                    {t("composeClose")}
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
      )}
    </>
  );
}
