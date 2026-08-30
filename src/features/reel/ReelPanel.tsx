/**
 * Sermon Highlight Reel Studio — the UI for `services/highlight_reel.rs`.
 *
 * One talk in, a week of vertical social posts out. The flow the Rust side
 * actually offers, in order:
 *
 *   1. `reel_storyboard` — propose a reviewable storyboard from the transcript.
 *      With a resolvable Anthropic key Claude picks the clips; without one (or
 *      when the AI call fails) the backend falls back to a pure pause heuristic
 *      and says so via `used_ai` / `ai_error`. We LABEL which mode produced the
 *      clips rather than quietly serving the weaker output as if it were the
 *      strong one.
 *   2. The operator curates: drop clips they don't want. (Titles/hooks are
 *      edited in the AI-clips panel, which owns the project's clip list — this
 *      studio renders, it never writes the project.)
 *   3. `reel_build_plan` — pure fan-out: kept clips × chosen platform presets →
 *      the exact list of files that will be written. Recomputed on every change
 *      so the preview can never promise a file the render won't produce.
 *   4. `reel_render_all` — burn every item, streaming `reel-render-progress`;
 *      `reel_cancel_render` stops the queue after the current clip.
 *
 * Progress/cancel semantics mirror `features/export/ComposeExport.tsx` with one
 * faithful difference: a cancelled batch RESOLVES here (with `cancelled: true`
 * and the files finished before the stop) instead of rejecting, so a partial
 * success stays visible. The error state is reserved for a real rejection.
 *
 * Everything here needs the native backend (ffmpeg + a real filesystem), so the
 * whole surface is guarded by `isTauri()`.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  Clapperboard,
  Check,
  CircleAlert,
  Clock,
  Film,
  FolderOpen,
  KeyRound,
  Loader2,
  Sparkles,
  Trash2,
  X,
} from "lucide-react";

import { ipc, IPCError } from "@/lib/ipc";
import type {
  ClaudeModel,
  Clip,
  ExportPreset,
  Project,
  ReelRenderProgress,
  ReelRenderResult,
  ReelStoryboard,
  RenderPlan,
} from "@/lib/bindings";
import { useT, type TKey } from "@/lib/i18n";
import { cn } from "@/lib/cn";
import { subscribeReelProgress } from "./reelProgress";

const MODELS: { id: ClaudeModel; name: string }[] = [
  { id: "haiku45", name: "Haiku 4.5" },
  { id: "sonnet46", name: "Sonnet 4.6" },
  { id: "opus47", name: "Opus 4.7" },
];

type Phase =
  | { kind: "idle" }
  | {
      kind: "rendering";
      progress: ReelRenderProgress | null;
      cancelling: boolean;
    }
  | { kind: "settled"; result: ReelRenderResult }
  | { kind: "error"; message: string };

export function ReelPanel({ project }: { project: Project }) {
  const t = useT();

  const [model, setModel] = useState<ClaudeModel>("haiku45");
  const [apiKey, setApiKey] = useState("");
  const [generating, setGenerating] = useState(false);
  const [genError, setGenError] = useState<string | null>(null);
  const [storyboard, setStoryboard] = useState<ReelStoryboard | null>(null);
  /** The curated clip list — starts as the storyboard's, minus anything dropped. */
  const [clips, setClips] = useState<Clip[]>([]);

  const [presets, setPresets] = useState<ExportPreset[]>([]);
  const [presetIds, setPresetIds] = useState<string[]>([]);
  const [outputDir, setOutputDir] = useState<string | null>(null);
  const [plan, setPlan] = useState<RenderPlan | null>(null);

  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  const unsubRef = useRef<(() => void) | null>(null);
  useEffect(() => () => unsubRef.current?.(), []);

  const tauri = isTauri();

  // Platform catalog. The backend's own default for an empty selection is the
  // vertical (9:16) set, so we preselect exactly that — the UI never sends an
  // empty list, and what is ticked is what gets rendered.
  useEffect(() => {
    if (!tauri) return;
    let cancelled = false;
    ipc.render
      .listExportPresets()
      .then((list) => {
        if (cancelled) return;
        setPresets(list);
        setPresetIds(
          list.filter((p) => p.aspect === "portrait").map((p) => p.id),
        );
      })
      .catch(() => {
        if (!cancelled) setPresets([]);
      });
    return () => {
      cancelled = true;
    };
  }, [tauri]);

  // Pure fan-out preview: recomputed whenever the curated clips, the platform
  // selection or the output folder move. This is the same command the render
  // consumes, so the file list on screen IS the file list ffmpeg will write.
  useEffect(() => {
    if (!tauri || clips.length === 0 || presetIds.length === 0 || !outputDir) {
      setPlan(null);
      return;
    }
    let cancelled = false;
    ipc.reel
      .buildPlan(
        { talk_summary: storyboard?.plan.talk_summary ?? "", clips },
        presetIds,
        outputDir,
      )
      .then((p) => !cancelled && setPlan(p))
      .catch(() => !cancelled && setPlan(null));
    return () => {
      cancelled = true;
    };
  }, [tauri, clips, presetIds, outputDir, storyboard]);

  async function generate() {
    setGenError(null);
    setGenerating(true);
    try {
      const board = await ipc.reel.storyboard(
        project,
        model,
        apiKey || undefined,
      );
      setStoryboard(board);
      setClips(board.plan.clips);
    } catch (e) {
      setGenError(messageOf(e));
    } finally {
      setGenerating(false);
    }
  }

  async function chooseFolder() {
    const dir = await openDialog({ directory: true, multiple: false });
    if (typeof dir === "string") setOutputDir(dir);
  }

  function togglePreset(id: string) {
    setPresetIds((cur) =>
      cur.includes(id) ? cur.filter((p) => p !== id) : [...cur, id],
    );
  }

  async function renderAll() {
    if (!plan || plan.total === 0) return;
    setPhase({ kind: "rendering", progress: null, cancelling: false });
    unsubRef.current = subscribeReelProgress((p) =>
      setPhase((cur) =>
        cur.kind === "rendering" ? { ...cur, progress: p } : cur,
      ),
    );
    try {
      // The batch resolves even when cancelled or partly failed — the result
      // carries which files landed. Only a genuine rejection is an error.
      const result = await ipc.reel.renderAll(project, plan);
      setPhase({ kind: "settled", result });
    } catch (e) {
      setPhase({ kind: "error", message: messageOf(e) });
    } finally {
      unsubRef.current?.();
      unsubRef.current = null;
    }
  }

  function cancelRender() {
    setPhase((cur) =>
      cur.kind === "rendering" ? { ...cur, cancelling: true } : cur,
    );
    void ipc.reel.cancelRender().catch(() => {});
  }

  const modeKey: TKey | null = !storyboard
    ? null
    : storyboard.used_ai
      ? "reelModeAi"
      : storyboard.ai_error
        ? "reelModeAiFailed"
        : "reelModeHeuristic";

  if (!tauri) {
    return (
      <div className="space-y-3 p-4">
        <PanelHeader />
        <p
          data-testid="reel-unavailable"
          className="rounded-md border border-[var(--color-border)] bg-[var(--color-bg-surface)] px-3 py-2 text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]"
        >
          {t("reelNeedsDesktop")}
        </p>
      </div>
    );
  }

  return (
    <div className="space-y-5 p-4" data-testid="reel-panel">
      <PanelHeader />

      {/* Storyboard source: model + optional key. No key is a supported mode,
          not a failure — the label below says which one actually ran. */}
      <section className="space-y-2">
        <label className="block text-[var(--text-ui-xs)] font-semibold text-[var(--color-fg-muted)]">
          {t("reelModelLabel")}
        </label>
        <select
          value={model}
          aria-label={t("reelModelLabel")}
          onChange={(e) => setModel(e.target.value as ClaudeModel)}
          className="w-full rounded-md border border-[var(--color-border)] bg-[var(--color-bg-input)] px-2 py-1.5 text-[var(--text-ui-sm)] outline-none focus:border-[var(--color-accent-500)]"
        >
          {MODELS.map((m) => (
            <option key={m.id} value={m.id}>
              {m.name}
            </option>
          ))}
        </select>
        <label className="flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-bg-input)] px-3 py-1.5">
          <KeyRound size={14} className="text-[var(--color-fg-subtle)]" />
          <input
            type="password"
            value={apiKey}
            onChange={(e) => setApiKey(e.target.value)}
            placeholder={t("apiKeyPlaceholder")}
            aria-label={t("apiKeyPlaceholder")}
            className="flex-1 bg-transparent text-[var(--text-ui-sm)] outline-none placeholder:text-[var(--color-fg-subtle)]"
          />
        </label>
        <p className="text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
          {t("reelKeyHint")}
        </p>
        <button
          type="button"
          onClick={() => void generate()}
          disabled={generating}
          className="flex w-full items-center justify-center gap-1.5 rounded-md bg-[var(--color-accent-600)] px-4 py-2 text-[var(--text-ui-sm)] font-medium text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)] disabled:opacity-50"
        >
          {generating ? (
            <Loader2 size={14} className="animate-spin" />
          ) : (
            <Sparkles size={14} />
          )}
          {generating
            ? t("reelGenerating")
            : storyboard
              ? t("reelRegenerate")
              : t("reelGenerate")}
        </button>
      </section>

      {genError && (
        <p
          data-testid="reel-error"
          className="rounded-md border border-[var(--color-danger)]/40 bg-[var(--color-danger)]/10 px-3 py-2 text-[var(--text-ui-sm)] text-[var(--color-danger)]"
        >
          {genError}
        </p>
      )}

      {/* Which mode produced these clips — never left to the operator to guess. */}
      {modeKey && (
        <p
          data-testid="reel-mode"
          className={cn(
            "flex items-start gap-2 rounded-md px-3 py-2 text-[var(--text-ui-xs)]",
            storyboard?.used_ai
              ? "bg-[var(--color-accent-500)]/10 text-[var(--color-accent-300)]"
              : "bg-[var(--color-bg-surface)] text-[var(--color-fg-muted)]",
          )}
        >
          {storyboard?.used_ai ? (
            <Sparkles size={13} className="mt-0.5 shrink-0" />
          ) : (
            <CircleAlert size={13} className="mt-0.5 shrink-0" />
          )}
          <span>{t(modeKey, { error: storyboard?.ai_error ?? "" })}</span>
        </p>
      )}

      {storyboard && clips.length === 0 && (
        <p
          data-testid="reel-empty"
          className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]"
        >
          {t("reelNoClips")}
        </p>
      )}

      {clips.length > 0 && (
        <section className="space-y-2">
          <h3 className="text-[var(--text-ui-sm)] font-semibold">
            {t("reelClipsHeader", { n: clips.length })}
          </h3>
          <ul className="space-y-2">
            {clips.map((clip) => (
              <ClipRow
                key={clip.id}
                project={project}
                clip={clip}
                onDrop={() =>
                  setClips((cur) => cur.filter((c) => c.id !== clip.id))
                }
              />
            ))}
          </ul>
        </section>
      )}

      {clips.length > 0 && (
        <>
          {/* Platforms — every preset the export catalog offers. */}
          <section className="space-y-2">
            <h3 className="text-[var(--text-ui-sm)] font-semibold">
              {t("reelPlatformsHeader")}
            </h3>
            <ul className="space-y-1">
              {presets.map((p) => (
                <li key={p.id}>
                  <label className="flex cursor-pointer items-center gap-2 text-[var(--text-ui-xs)]">
                    <input
                      type="checkbox"
                      checked={presetIds.includes(p.id)}
                      onChange={() => togglePreset(p.id)}
                      data-testid={`reel-preset-${p.id}`}
                    />
                    <span className="font-medium">{p.name}</span>
                    <span className="text-[var(--color-fg-subtle)]">
                      {p.width}×{p.height}
                    </span>
                  </label>
                </li>
              ))}
            </ul>
            {presetIds.length === 0 && (
              <p className="text-[var(--text-ui-xs)] text-[var(--color-fg-muted)]">
                {t("reelPlatformsNone")}
              </p>
            )}
          </section>

          {/* Output folder */}
          <section className="space-y-2">
            <h3 className="text-[var(--text-ui-sm)] font-semibold">
              {t("reelOutputFolder")}
            </h3>
            <button
              type="button"
              onClick={() => void chooseFolder()}
              className="flex w-full items-center gap-2 rounded-md border border-[var(--color-border)] px-3 py-1.5 text-left text-[var(--text-ui-xs)] hover:border-[var(--color-accent-600)]"
            >
              <FolderOpen size={13} className="shrink-0" />
              <span
                data-testid="reel-outdir"
                className="truncate font-mono text-[var(--color-fg-muted)]"
              >
                {outputDir ?? t("reelNoFolder")}
              </span>
            </button>
          </section>

          {/* The exact files the render will write. */}
          {plan && plan.total > 0 && (
            <section className="space-y-2" data-testid="reel-plan">
              <h3 className="text-[var(--text-ui-sm)] font-semibold">
                {t("reelPlanHeader", {
                  total: plan.total,
                  clips: clips.length,
                  platforms: presetIds.length,
                })}
              </h3>
              <ul className="max-h-40 space-y-0.5 overflow-y-auto rounded-md border border-[var(--color-border)] bg-[var(--color-bg-surface)] p-2">
                {plan.items.map((item) => (
                  <li
                    key={item.id}
                    className="truncate font-mono text-[10px] text-[var(--color-fg-subtle)]"
                    title={item.output_path}
                  >
                    {basename(item.output_path)}
                  </li>
                ))}
              </ul>
            </section>
          )}

          <button
            type="button"
            onClick={() => void renderAll()}
            disabled={!plan || plan.total === 0 || phase.kind === "rendering"}
            data-testid="reel-render-all"
            className="flex w-full items-center justify-center gap-1.5 rounded-md border border-[var(--color-accent-500)]/50 bg-[var(--color-accent-500)]/8 px-4 py-2 text-[var(--text-ui-sm)] font-semibold text-[var(--color-accent-300)] hover:border-[var(--color-accent-500)] hover:bg-[var(--color-accent-500)]/12 disabled:opacity-50"
          >
            <Film size={14} />
            {t("reelRenderAll")}
          </button>
        </>
      )}

      {phase.kind !== "idle" && plan && (
        <RenderModal
          plan={plan}
          phase={phase}
          outputDir={outputDir}
          onCancel={cancelRender}
          onClose={() => setPhase({ kind: "idle" })}
        />
      )}
    </div>
  );
}

function PanelHeader() {
  const t = useT();
  return (
    <header>
      <h2 className="mb-1 flex items-center gap-2 text-[var(--text-ui-md)] font-semibold">
        <Clapperboard size={15} className="text-[var(--color-accent-400)]" />
        {t("reelTitle")}
      </h2>
      <p className="text-[var(--text-ui-xs)] text-[var(--color-fg-muted)]">
        {t("reelIntro")}
      </p>
    </header>
  );
}

/** One proposed clip: its title, the words it covers, and its real time range. */
function ClipRow({
  project,
  clip,
  onDrop,
}: {
  project: Project;
  clip: Clip;
  onDrop: () => void;
}) {
  const t = useT();
  const text = useMemo(() => clipText(project, clip), [project, clip]);
  return (
    <li
      data-testid="reel-clip"
      className="rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-elevated)] p-2.5"
    >
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">
          <p className="truncate text-[var(--text-ui-sm)] font-semibold">
            {clip.title}
          </p>
          <p className="mt-0.5 flex items-center gap-1.5 text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
            <Clock size={11} />
            {fmtRange(clip.start_ms, clip.end_ms)} ·{" "}
            {fmtDuration(clip.end_ms - clip.start_ms)}
          </p>
          {text && (
            <p className="mt-1 line-clamp-3 text-[var(--text-ui-xs)] text-[var(--color-fg-muted)]">
              {text}
            </p>
          )}
        </div>
        <button
          type="button"
          onClick={onDrop}
          title={t("reelClipRemove")}
          aria-label={t("reelClipRemove")}
          className="shrink-0 rounded p-1 text-[var(--color-fg-subtle)] hover:bg-[var(--color-danger)]/10 hover:text-[var(--color-danger)]"
        >
          <Trash2 size={13} />
        </button>
      </div>
    </li>
  );
}

/** Full-screen batch progress, mirroring ComposeExport's modal + cancel. */
function RenderModal({
  plan,
  phase,
  outputDir,
  onCancel,
  onClose,
}: {
  plan: RenderPlan;
  phase: Phase;
  outputDir: string | null;
  onCancel: () => void;
  onClose: () => void;
}) {
  const t = useT();
  const progress = phase.kind === "rendering" ? phase.progress : null;
  const result = phase.kind === "settled" ? phase.result : null;
  const percent = Math.round((progress?.fraction ?? (result ? 1 : 0)) * 100);
  const failedById = new Map(result?.failed ?? []);
  const renderedPaths = new Set(result?.rendered ?? []);

  return (
    <div
      role="dialog"
      aria-label={t("reelProgressTitle")}
      data-testid="reel-progress"
      className="fixed inset-0 z-[60] grid place-items-center bg-black/60 p-6"
    >
      <div className="w-full max-w-lg rounded-xl border border-[var(--color-border)] bg-[var(--color-bg-elevated)] p-6 shadow-2xl">
        <div className="mb-4 flex items-center gap-2">
          <Film size={16} className="text-[var(--color-accent-400)]" />
          <h3 className="text-[var(--text-ui-md)] font-semibold">
            {t("reelProgressTitle")}
          </h3>
        </div>

        {phase.kind === "rendering" && (
          <>
            <div className="h-2 w-full overflow-hidden rounded-full bg-[var(--color-bg-surface)]">
              <div
                className="h-full rounded-full bg-[var(--color-accent-500)] transition-[width]"
                style={{ width: `${percent}%` }}
                data-testid="reel-progress-bar"
              />
            </div>
            <div className="mt-2 flex items-center justify-between">
              <span
                data-testid="reel-progress-count"
                className="font-mono text-[var(--text-ui-sm)] tabular-nums text-[var(--color-fg-muted)]"
              >
                {t("reelProgressCount", {
                  completed: progress?.completed ?? 0,
                  total: progress?.total ?? plan.total,
                })}
              </span>
              <button
                type="button"
                onClick={onCancel}
                disabled={phase.cancelling}
                className="inline-flex items-center gap-1.5 rounded-md border border-[var(--color-border)] px-3 py-1.5 text-[var(--text-ui-sm)] font-medium text-[var(--color-fg-muted)] hover:text-[var(--color-fg)] disabled:opacity-50"
              >
                {phase.cancelling ? (
                  <Loader2 size={13} className="animate-spin" />
                ) : (
                  <X size={13} />
                )}
                {phase.cancelling ? t("reelCancelling") : t("reelCancel")}
              </button>
            </div>
          </>
        )}

        {result && (
          <p
            data-testid={result.cancelled ? "reel-cancelled" : "reel-done"}
            className="flex items-start gap-2 text-[var(--text-ui-sm)] text-[var(--color-fg)]"
          >
            {result.cancelled ? (
              <X
                size={16}
                className="mt-0.5 shrink-0 text-[var(--color-fg-muted)]"
              />
            ) : (
              <Check
                size={16}
                className="mt-0.5 shrink-0 text-[var(--color-success)]"
              />
            )}
            <span>
              {result.cancelled
                ? t("reelCancelled", { n: result.rendered.length })
                : t("reelDone", {
                    n: result.rendered.length,
                    dir: outputDir ?? "",
                  })}
            </span>
          </p>
        )}

        {result && result.failed.length > 0 && (
          <p
            data-testid="reel-failed"
            className="mt-2 text-[var(--text-ui-sm)] text-[var(--color-danger)]"
          >
            {t("reelFailedCount", { n: result.failed.length })}
          </p>
        )}

        {phase.kind === "error" && (
          <p
            data-testid="reel-render-error"
            className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)]"
          >
            {phase.message}
          </p>
        )}

        {/* Per-item state: which file is on the encoder right now, which
            landed, which failed and why. */}
        <ul className="mt-4 max-h-56 space-y-1 overflow-y-auto">
          {plan.items.map((item, i) => {
            const state = itemState(i, item.id, progress, result, {
              failedById,
              renderedPaths,
              path: item.output_path,
            });
            return (
              <li
                key={item.id}
                data-testid="reel-item"
                data-item-state={state}
                className={cn(
                  "flex items-center gap-2 rounded-md px-2 py-1 text-[var(--text-ui-xs)]",
                  state === "rendering" && "bg-[var(--color-accent-500)]/10",
                )}
              >
                <span className="w-16 shrink-0 text-[var(--color-fg-subtle)]">
                  {t(ITEM_STATE_KEY[state])}
                </span>
                <span className="min-w-0 flex-1 truncate font-mono text-[10px] text-[var(--color-fg-muted)]">
                  {basename(item.output_path)}
                </span>
                {state === "failed" && (
                  <span className="max-w-[45%] truncate text-[var(--color-danger)]">
                    {failedById.get(item.id)}
                  </span>
                )}
              </li>
            );
          })}
        </ul>

        {phase.kind !== "rendering" && (
          <div className="mt-4 flex justify-end">
            <button
              type="button"
              onClick={onClose}
              className="rounded-md border border-[var(--color-border)] px-4 py-1.5 text-[var(--text-ui-sm)] font-medium text-[var(--color-fg-muted)] hover:text-[var(--color-fg)]"
            >
              {t("reelClose")}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

type ItemState = "pending" | "rendering" | "done" | "failed";

const ITEM_STATE_KEY: Record<ItemState, TKey> = {
  pending: "reelItemPending",
  rendering: "reelItemRendering",
  done: "reelItemDone",
  failed: "reelItemFailed",
};

/**
 * The queue is strictly sequential in Rust, so `completed` doubles as the index
 * of the item currently on the encoder. Once the batch settles the authority
 * shifts to the result: a path in `rendered` landed, an id in `failed` did not,
 * and anything neither (a cancelled tail) is back to pending.
 */
function itemState(
  index: number,
  id: string,
  progress: ReelRenderProgress | null,
  result: ReelRenderResult | null,
  lookup: {
    failedById: Map<string, string>;
    renderedPaths: Set<string>;
    path: string;
  },
): ItemState {
  if (result) {
    if (lookup.failedById.has(id)) return "failed";
    if (lookup.renderedPaths.has(lookup.path)) return "done";
    return "pending";
  }
  if (!progress) return "pending";
  if (index < progress.completed) return "done";
  if (index === progress.completed && progress.current_item_id !== null)
    return "rendering";
  return "pending";
}

/** The captions a clip covers, as one readable line. */
function clipText(project: Project, clip: Clip): string {
  const ids = new Set(clip.caption_ids);
  const chosen = project.captions.filter((c) =>
    ids.size > 0
      ? ids.has(c.id)
      : c.start_ms < clip.end_ms && c.end_ms > clip.start_ms,
  );
  return chosen
    .map((c) => c.words.map((w) => w.text).join(" "))
    .join(" ")
    .trim();
}

function messageOf(e: unknown): string {
  if (e instanceof IPCError) return e.message;
  return e instanceof Error ? e.message : String(e);
}

function basename(path: string): string {
  return path.split(/[/\\]/).pop() ?? path;
}

function fmtClock(ms: number): string {
  const totalSec = Math.floor(ms / 1000);
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

function fmtRange(startMs: number, endMs: number): string {
  return `${fmtClock(startMs)}–${fmtClock(endMs)}`;
}

function fmtDuration(ms: number): string {
  return `${Math.round(ms / 1000)}s`;
}
