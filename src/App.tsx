import { useCallback, useEffect, useMemo, useState } from "react";
import { appDataDir, join } from "@tauri-apps/api/path";
import { convertFileSrc, isTauri } from "@tauri-apps/api/core";
import {
  Download,
  Settings as SettingsIcon,
  BookText,
  FileVideo,
  Clock,
  Cpu,
  Palette,
  Wand2,
  Sparkles,
  Lightbulb,
  Languages,
  Users,
  Gauge,
  RefreshCw,
  Scissors,
  PanelRightClose,
  FileText,
  Save,
  FolderOpen,
  Clapperboard,
  type LucideIcon,
} from "lucide-react";
import {
  open as openFileDialog,
  save as saveFileDialog,
} from "@tauri-apps/plugin-dialog";

import { CaptionEditor } from "@/features/editor/CaptionEditor";
import { Timeline } from "@/features/timeline/Timeline";
import { ClipInspector } from "@/features/timeline/ClipInspector";
import { MediaBin } from "@/features/media/MediaBin";
import { MissingMediaBanner } from "@/features/media/MissingMediaBanner";
import { ContextPanel } from "@/features/context/ContextPanel";
import { SettingsPanel } from "@/features/settings/SettingsPanel";
import { Onboarding } from "@/features/onboarding/Onboarding";
import { ImportScreen } from "@/features/project/ImportScreen";
import { seedProjectFromImport } from "@/features/project/deepLinkImport";
import { ModelPicker } from "@/features/transcribe/ModelPicker";
import { LocalPanel } from "@/features/transcribe/LocalPanel";
import { CloudPanel } from "@/features/transcribe/CloudPanel";
import { StyleEditor } from "@/features/style/StyleEditor";
import { ExportPanel } from "@/features/export/ExportPanel";
import { ProjectMetaPanel } from "@/features/project/ProjectMetaPanel";
import { CleanupPanel } from "@/features/cleanup/CleanupPanel";
import { ReflowPanel } from "@/features/reflow/ReflowPanel";
import { PolishPanel } from "@/features/polish/PolishPanel";
import { SuggestPanel } from "@/features/suggest/SuggestPanel";
import { ClipsPanel } from "@/features/clips/ClipsPanel";
import { TranslatePanel } from "@/features/translate/TranslatePanel";
import { SpeakersPanel } from "@/features/speakers/SpeakersPanel";
import { SAMPLE_PROJECT } from "@/lib/sampleProject";
import { useProjectStore, useUndoHotkeys } from "@/lib/useProjectStore";
import { ipc } from "@/lib/ipc";
import type {
  Caption,
  DownloadProgress,
  Project,
  Style,
  WhisperModel,
} from "@/lib/bindings";
import { checkForUpdate, installAndRelaunch, type Update } from "@/lib/updater";
import { Modal } from "@/components/Modal";
import {
  CommandPalette,
  type PaletteCommand,
} from "@/components/CommandPalette";
import { useT, type TKey } from "@/lib/i18n";
import { cn } from "@/lib/cn";
import logoUrl from "@/assets/logo.svg";

// The editing tools that live in the right-hand dock — each operates on the
// captions visible in the centre workspace.
type DockTool =
  | "media"
  | "context"
  | "style"
  | "speakers"
  | "polish"
  | "suggest"
  | "translate"
  | "cleanup"
  | "reflow"
  | "projectmeta";

// Pipeline / output / config operations that open as a modal over the
// workspace rather than docking beside it.
type ModalKind = "transcribe" | "clips" | "export" | "settings";

type DockToolDef = { id: DockTool; icon: LucideIcon; labelKey: TKey };

// The dock tools, grouped by intent so the 56px rail reads as three clusters
// (Content · Format · AI) separated by hairlines, not nine icons competing.
const DOCK_GROUPS: Array<DockToolDef[]> = [
  [{ id: "media", icon: Clapperboard, labelKey: "navMedia" }],
  [
    { id: "context", icon: BookText, labelKey: "navContext" },
    { id: "projectmeta", icon: FileText, labelKey: "navProjectMeta" },
  ],
  [
    { id: "style", icon: Palette, labelKey: "navStyle" },
    { id: "speakers", icon: Users, labelKey: "navSpeakers" },
  ],
  [
    { id: "polish", icon: Sparkles, labelKey: "navPolish" },
    { id: "suggest", icon: Lightbulb, labelKey: "navSuggest" },
    { id: "translate", icon: Languages, labelKey: "navTranslate" },
    { id: "cleanup", icon: Wand2, labelKey: "navCleanup" },
    { id: "reflow", icon: Gauge, labelKey: "navReflow" },
  ],
];

// Flat view for label lookups.
const DOCK_TOOLS: DockToolDef[] = DOCK_GROUPS.flat();

function dockLabelKey(tool: DockTool): TKey {
  return DOCK_TOOLS.find((d) => d.id === tool)?.labelKey ?? "navContext";
}

// Route freshly-transcribed captions onto a Caption track. Transcription (local
// or cloud) hands back a flat caption list with no track binding, but the NLE
// wants every caption to live on a Caption track. If the project already has one
// (the schema-v4 backfill creates one when opening any file, so it usually
// does), reuse it; otherwise synthesize one in the SAME store update so the
// result stays atomic — no async `addTrack` round-trip that could interleave
// with the caption assignment. Legacy caption-only routing keeps working: the
// captions still land on the project, now additionally tagged with a track id.
function routeCaptionsToCaptionTrack(
  prev: Project,
  captions: Caption[],
): Project {
  let tracks = prev.tracks;
  let captionTrack = tracks.find((tr) => tr.kind === "caption");
  if (!captionTrack) {
    const nextIndex =
      tracks.reduce((max, tr) => Math.max(max, tr.index), -1) + 1;
    captionTrack = {
      id: crypto.randomUUID(),
      kind: "caption",
      name: "Undertekster",
      index: nextIndex,
      enabled: true,
      locked: false,
      muted: false,
      solo: false,
    };
    tracks = [...tracks, captionTrack];
  }
  const trackId = captionTrack.id;
  return {
    ...prev,
    tracks,
    captions: captions.map((c) => ({ ...c, track_id: trackId })),
  };
}

function App() {
  const t = useT();
  // The open project + its undo/redo history live in one shared store. `reset`
  // replaces the whole project and clears history (open/import/demo/back);
  // `setProject` is the non-undoable direct replace for edits that were never
  // part of the undo stack (dock panels, style, transcription results).
  const project = useProjectStore((s) => s.project);
  const setProject = useProjectStore((s) => s.setProject);
  const resetProject = useProjectStore((s) => s.reset);
  // ⌘Z / ⌘⇧Z, mounted once for the whole app (no-op until a project exists).
  useUndoHotkeys();
  // Which tool the right dock shows, and whether the dock is open.
  const [dockTool, setDockTool] = useState<DockTool>("context");
  const [dockOpen, setDockOpen] = useState(true);
  // The active modal (transcribe / clips / export / settings), or null.
  const [modal, setModal] = useState<ModalKind | null>(null);
  // The timeline clip selected for the inspector (or null). Cleared whenever the
  // open project changes so a stale id never points into a different project.
  const [selectedClipId, setSelectedClipId] = useState<string | null>(null);
  const projectId = project?.id;
  useEffect(() => {
    setSelectedClipId(null);
  }, [projectId]);
  // Scheme of the Sunday-suite app that deep-linked us here (Phase 8), so the
  // Export panel can offer to hand the captions back. Null for normal launches.
  const [returnTo, setReturnTo] = useState<string | null>(null);
  const [model, setModel] = useState<WhisperModel | null>("large-v3-turbo");
  const [update, setUpdate] = useState<Update | null>(null);
  // App-level toast — Save/Open had NO success/failure feedback (a failed save
  // looked identical to a successful one → silent data loss). Auto-dismisses.
  const [toast, setToast] = useState<{
    kind: "ok" | "err";
    text: string;
  } | null>(null);
  // Show a brief toast; auto-dismiss after a few seconds (unless replaced).
  // Defined here (above the early returns) so launch-time effects — deep-link
  // import, model download — can surface failures instead of dying in the
  // console. Stable identity keeps effect deps honest.
  const notify = useCallback((kind: "ok" | "err", text: string) => {
    setToast({ kind, text });
    window.setTimeout(
      () => setToast((cur) => (cur && cur.text === text ? null : cur)),
      kind === "err" ? 6000 : 3500,
    );
  }, []);
  const [downloadedModels, setDownloadedModels] = useState<WhisperModel[]>([]);
  const [downloading, setDownloading] = useState<{
    model: WhisperModel;
    progress: DownloadProgress;
  } | null>(null);
  const [onboarded, setOnboarded] = useState<boolean>(() => {
    try {
      return localStorage.getItem("sundayedit.onboarded") === "1";
    } catch {
      return false;
    }
  });

  // Asset URL for the source video, fed to the timeline's <video>. Only built
  // under Tauri (the asset protocol) and for a real on-disk path — the demo
  // sentinel (/demo/…) and browser dev fall back to the timecode placeholder.
  const videoSrc = useMemo(() => {
    const path = project?.video_path;
    if (!path || path.startsWith("/demo/") || !isTauri()) return undefined;
    try {
      return convertFileSrc(path);
    } catch {
      return undefined;
    }
  }, [project?.video_path]);

  // Check for a newer signed build once on launch (no-op outside Tauri /
  // before any release exists).
  useEffect(() => {
    checkForUpdate().then(setUpdate);
  }, []);

  const refreshDownloaded = useCallback(async () => {
    try {
      const dir = await join(await appDataDir(), "models");
      setDownloadedModels(await ipc.asr.downloadedModels(dir));
    } catch {
      // Not in Tauri (browser dev) — no local models dir; leave empty.
    }
  }, []);

  useEffect(() => {
    void refreshDownloaded();
  }, [refreshDownloaded]);

  // Sunday-link: a sister app launched us with `sundayedit://import?…`. Create
  // the project from the carried video and seed its language/context/glossary,
  // then drop the user straight on the Transcribe tab (Phase 8). No-op outside
  // Tauri / when no link arrives.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void (async () => {
      try {
        unlisten = await ipc.deeplink.onImport(async (url) => {
          try {
            const req = await ipc.deeplink.parseImport(url);
            const proj = await ipc.project.createFromVideo(req.path);
            if (cancelled) return;
            resetProject(seedProjectFromImport(proj, req));
            setReturnTo(req.return_to);
            setModal("transcribe");
          } catch (e) {
            console.error("deep-link import failed", e);
            if (!cancelled)
              notify("err", `Import failed: ${(e as Error).message}`);
          }
        });
      } catch {
        // Not in Tauri (browser dev) — no deep-link bridge; ignore.
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [notify, resetProject]);

  // Fetch the chosen Whisper model on first use. Streams progress; refreshes
  // the downloaded set on completion.
  async function handleDownloadModel(m: WhisperModel) {
    let unlisten: (() => void) | undefined;
    try {
      const dir = await join(await appDataDir(), "models");
      setDownloading({
        model: m,
        progress: { downloaded_bytes: 0, total_bytes: null, fraction: null },
      });
      unlisten = await ipc.asr.onDownloadProgress((progress) =>
        setDownloading((d) =>
          d && d.model === m ? { model: m, progress } : d,
        ),
      );
      await ipc.asr.downloadModel(dir, m);
      await refreshDownloaded();
    } catch (e) {
      console.error("model download failed", e);
      notify("err", `Model download failed: ${(e as Error).message}`);
    } finally {
      unlisten?.();
      setDownloading(null);
    }
  }

  // First run (no project yet): walk onboarding once, then the import screen.
  if (!project && !onboarded) {
    return (
      <Onboarding
        selected={model}
        onSelect={setModel}
        downloadedModels={downloadedModels}
        downloading={downloading}
        onDownload={handleDownloadModel}
        onImported={(proj) => {
          try {
            localStorage.setItem("sundayedit.onboarded", "1");
          } catch {
            /* private mode — onboarding just shows again next launch */
          }
          setOnboarded(true);
          resetProject(proj);
        }}
        onTryDemo={() => resetProject(SAMPLE_PROJECT)}
      />
    );
  }

  // Import screen until a project exists. "Try the demo" loads SAMPLE_PROJECT
  // so the editor is explorable without a real video / ffmpeg.
  if (!project) {
    return (
      <div className="flex h-screen w-screen flex-col bg-[var(--color-bg)] text-[var(--color-fg)]">
        {update && (
          <UpdateBanner update={update} onDismiss={() => setUpdate(null)} />
        )}
        <div className="flex-1">
          <ImportScreen onProjectReady={resetProject} />
        </div>
        <div className="border-t border-[var(--color-border)] px-6 py-3 text-center">
          <button
            type="button"
            onClick={() => resetProject(SAMPLE_PROJECT)}
            className="text-[var(--text-ui-sm)] text-[var(--color-fg-muted)] underline-offset-4 hover:text-[var(--color-accent-400)] hover:underline"
          >
            {t("importDemoLink")}
          </button>
        </div>
      </div>
    );
  }

  // Save the current project to a `.sundayedit` file the user picks. No-op
  // outside Tauri (no native dialog) or if the user cancels.
  async function saveProjectAs() {
    if (!project) return;
    const suggested = project.name.replace(/\.[^.]+$/, "");
    try {
      const path = await saveFileDialog({
        defaultPath: `${suggested}.sundayedit`,
        filters: [{ name: "SundayEdit", extensions: ["sundayedit"] }],
      });
      if (typeof path !== "string") return; // cancelled
      await ipc.project.save(project, path);
      // A successful save MUST be acknowledged — without this, a failed save was
      // indistinguishable from a successful one (silent data loss).
      notify("ok", `Saved to ${path}`);
    } catch (e) {
      console.error("project save failed", e);
      notify("err", `Save failed: ${(e as Error).message}`);
    }
  }

  // Open a `.sundayedit` file the user picks, replacing the current project.
  async function openProject() {
    try {
      const path = await openFileDialog({
        multiple: false,
        filters: [{ name: "SundayEdit", extensions: ["sundayedit"] }],
      });
      if (typeof path !== "string") return; // cancelled
      const opened = await ipc.project.open(path);
      resetProject(opened);
      setReturnTo(null);
    } catch (e) {
      console.error("project open failed", e);
      notify("err", `Could not open project: ${(e as Error).message}`);
    }
  }

  // Click a dock tool: focus it, opening the dock; click the active one again
  // to collapse the dock.
  function selectDockTool(tool: DockTool) {
    if (dockOpen && dockTool === tool) {
      setDockOpen(false);
    } else {
      setDockTool(tool);
      setDockOpen(true);
    }
  }

  function openDockTool(tool: DockTool) {
    setDockTool(tool);
    setDockOpen(true);
  }

  const paletteCommands: PaletteCommand[] = [
    ...DOCK_TOOLS.map(({ id, icon, labelKey }) => ({
      id: `tool-${id}`,
      label: t(labelKey),
      group: t("paletteGroupTools"),
      icon,
      run: () => openDockTool(id),
    })),
    {
      id: "m-transcribe",
      label: t("navTranscribe"),
      group: t("paletteGroupPipeline"),
      icon: Cpu,
      run: () => setModal("transcribe"),
    },
    {
      id: "m-clips",
      label: t("navClips"),
      group: t("paletteGroupPipeline"),
      icon: Scissors,
      run: () => setModal("clips"),
    },
    {
      id: "m-export",
      label: t("navExport"),
      group: t("paletteGroupPipeline"),
      icon: Download,
      run: () => setModal("export"),
    },
    {
      id: "m-settings",
      label: t("navSettings"),
      group: t("paletteGroupPipeline"),
      icon: SettingsIcon,
      run: () => setModal("settings"),
    },
    {
      id: "p-open",
      label: t("projectOpen"),
      group: t("paletteGroupProject"),
      icon: FolderOpen,
      run: () => void openProject(),
    },
    {
      id: "p-save",
      label: t("projectSave"),
      group: t("paletteGroupProject"),
      icon: Save,
      run: () => void saveProjectAs(),
    },
    {
      id: "p-import",
      label: t("navBackToImport"),
      group: t("paletteGroupProject"),
      icon: FileVideo,
      run: () => {
        resetProject(null);
        setReturnTo(null);
      },
    },
  ];

  // The live timeline item behind the current selection (or null if it was
  // deleted / never set). Read fresh each render so committed ops reflect at once.
  const selectedItem = selectedClipId
    ? (project.timeline_items.find((i) => i.id === selectedClipId) ?? null)
    : null;

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-[var(--color-bg)] text-[var(--color-fg)]">
      {update && (
        <UpdateBanner update={update} onDismiss={() => setUpdate(null)} />
      )}

      {/* App-level toast (Save/Open success + failure). */}
      {toast && (
        <div
          role="status"
          onClick={() => setToast(null)}
          className={`fixed bottom-5 left-1/2 z-50 -translate-x-1/2 cursor-pointer rounded-lg px-4 py-2.5 text-[var(--text-ui-sm)] shadow-lg ${
            toast.kind === "err"
              ? "bg-[var(--color-danger,#b3261e)] text-white"
              : "bg-[var(--color-bg-elevated)] text-[var(--color-fg)] ring-1 ring-[var(--color-border)]"
          }`}
        >
          {toast.text}
        </div>
      )}

      {/* Left rail — picks which tool the right dock shows, grouped into
          Content · Format · AI clusters separated by hairlines. */}
      <nav className="flex w-14 flex-col items-center gap-1 border-r border-[var(--color-border)] bg-[var(--color-bg-elevated)] py-3">
        <button
          type="button"
          onClick={() => {
            resetProject(null);
            setReturnTo(null);
          }}
          title={t("navBackToImport")}
          aria-label={t("navBackToImport")}
          className="mb-3"
        >
          <img
            src={logoUrl}
            width={36}
            height={36}
            alt=""
            aria-hidden="true"
            className="block rounded-[22%]"
          />
        </button>
        {DOCK_GROUPS.map((group, gi) => (
          <div key={gi} className="flex flex-col items-center gap-1">
            {gi > 0 && (
              <div className="my-1 h-px w-6 bg-[var(--color-border)]" />
            )}
            {group.map(({ id, icon: Icon, labelKey }) => (
              <NavIcon
                key={id}
                active={dockOpen && dockTool === id}
                onClick={() => selectDockTool(id)}
                title={t(labelKey)}
              >
                <Icon size={18} />
              </NavIcon>
            ))}
          </div>
        ))}
      </nav>

      {/* Centre workspace — always: preview, editor, timeline. */}
      <main className="relative flex min-w-0 flex-1 flex-col overflow-hidden">
        <Topbar
          project={project}
          onOpenProject={openProject}
          onSaveProject={saveProjectAs}
          onTranscribe={() => setModal("transcribe")}
          onClips={() => setModal("clips")}
          onExport={() => setModal("export")}
          onSettings={() => setModal("settings")}
        />
        <MissingMediaBanner project={project} />
        <div className="min-h-0 flex-1 overflow-y-auto border-t border-[var(--color-border)]">
          <CaptionEditor key={project.id} />
        </div>
        <div className="h-56 shrink-0 border-t border-[var(--color-border)]">
          <Timeline
            project={project}
            videoSrc={videoSrc}
            onSelectClip={setSelectedClipId}
          />
        </div>

        {/* Clip inspector — floats above the timeline when a clip is selected. */}
        {selectedItem && (
          <div className="pointer-events-none absolute right-4 top-16 bottom-[15rem] z-40 flex items-start justify-end">
            <ClipInspector
              item={selectedItem}
              onClose={() => setSelectedClipId(null)}
            />
          </div>
        )}
      </main>

      {/* Right dock — the focused editing tool. */}
      {dockOpen && (
        <aside className="flex w-[380px] shrink-0 flex-col border-l border-[var(--color-border)] bg-[var(--color-bg)]">
          <div className="flex shrink-0 items-center justify-between border-b border-[var(--color-border)] bg-[var(--color-bg-elevated)] px-4 py-2.5">
            <span className="text-[var(--text-ui-sm)] font-semibold">
              {t(dockLabelKey(dockTool))}
            </span>
            <button
              type="button"
              onClick={() => setDockOpen(false)}
              title={t("actionClose")}
              aria-label={t("actionClose")}
              className="rounded-md p-1 text-[var(--color-fg-subtle)] hover:bg-[var(--color-bg-surface)] hover:text-[var(--color-fg)]"
            >
              <PanelRightClose size={16} />
            </button>
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto">
            {dockTool === "media" ? (
              <MediaBin project={project} />
            ) : dockTool === "context" ? (
              <ContextPanel project={project} onProjectChange={setProject} />
            ) : dockTool === "projectmeta" ? (
              <ProjectMetaPanel
                project={project}
                onProjectChange={setProject}
              />
            ) : dockTool === "style" ? (
              <StyleEditor
                style={project.default_style}
                onChange={(s: Style) =>
                  setProject({ ...project, default_style: s })
                }
              />
            ) : dockTool === "speakers" ? (
              <SpeakersPanel project={project} onProjectChange={setProject} />
            ) : dockTool === "polish" ? (
              <PolishPanel project={project} onProjectChange={setProject} />
            ) : dockTool === "suggest" ? (
              <SuggestPanel project={project} onProjectChange={setProject} />
            ) : dockTool === "translate" ? (
              <TranslatePanel project={project} onProjectChange={setProject} />
            ) : dockTool === "reflow" ? (
              <ReflowPanel project={project} onProjectChange={setProject} />
            ) : (
              <CleanupPanel project={project} onProjectChange={setProject} />
            )}
          </div>
        </aside>
      )}

      {/* Pipeline / output / config — modal over the workspace. */}
      {modal === "transcribe" && (
        <Modal title={t("navTranscribe")} onClose={() => setModal(null)}>
          <div className="space-y-10 p-6">
            <ModelPicker
              selected={model}
              onSelect={setModel}
              downloadedModels={downloadedModels}
              downloading={downloading}
              onDownload={handleDownloadModel}
            />
            <LocalPanel
              project={project}
              model={model}
              downloadedModels={downloadedModels}
              onProjectChange={setProject}
              onTranscribed={(captions) => {
                // Functional merge so the audio_wav_path LocalPanel just set
                // (via onProjectChange) isn't clobbered by a stale closure. The
                // captions are routed onto a Caption track (created if missing).
                setProject((prev) =>
                  prev ? routeCaptionsToCaptionTrack(prev, captions) : prev,
                );
                setModal(null);
              }}
            />
            <CloudPanel
              project={project}
              onTranscribed={(captions) => {
                setProject((prev) =>
                  prev ? routeCaptionsToCaptionTrack(prev, captions) : prev,
                );
                setModal(null);
              }}
            />
          </div>
        </Modal>
      )}
      {modal === "clips" && (
        <Modal
          title={t("navClips")}
          onClose={() => setModal(null)}
          widthClass="max-w-4xl"
        >
          <ClipsPanel project={project} onProjectChange={setProject} />
        </Modal>
      )}
      {modal === "export" && (
        <Modal title={t("navExport")} onClose={() => setModal(null)}>
          <ExportPanel
            project={project}
            onProjectChange={setProject}
            returnTo={returnTo}
          />
        </Modal>
      )}
      {modal === "settings" && (
        <Modal title={t("navSettings")} onClose={() => setModal(null)}>
          <SettingsPanel />
        </Modal>
      )}

      <CommandPalette commands={paletteCommands} />
    </div>
  );
}

function Topbar({
  project,
  onOpenProject,
  onSaveProject,
  onTranscribe,
  onClips,
  onExport,
  onSettings,
}: {
  project: Project;
  onOpenProject: () => void;
  onSaveProject: () => void;
  onTranscribe: () => void;
  onClips: () => void;
  onExport: () => void;
  onSettings: () => void;
}) {
  const t = useT();
  return (
    <div className="flex shrink-0 items-center gap-3 border-b border-[var(--color-border)] bg-[var(--color-bg-elevated)] px-5 py-2">
      <FileVideo size={15} className="shrink-0 text-[var(--color-fg-muted)]" />
      <span className="truncate text-[var(--text-ui-sm)] font-medium">
        {project.name}
      </span>
      {project.video_width > 0 && (
        <span className="shrink-0 text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
          {project.video_width}×{project.video_height} ·{" "}
          {project.video_fps.toFixed(2)} fps
        </span>
      )}
      <span className="flex shrink-0 items-center gap-1 text-[var(--text-ui-xs)] text-[var(--color-fg-subtle)]">
        <Clock size={11} /> {fmtDuration(project.video_duration_ms)}
      </span>

      <div className="flex-1" />

      <TopbarButton
        icon={FolderOpen}
        label={t("projectOpen")}
        onClick={onOpenProject}
      />
      <TopbarButton
        icon={Save}
        label={t("projectSave")}
        onClick={onSaveProject}
      />
      <span className="h-5 w-px shrink-0 bg-[var(--color-border)]" />
      <TopbarButton
        icon={Cpu}
        label={t("navTranscribe")}
        onClick={onTranscribe}
      />
      <TopbarButton icon={Scissors} label={t("navClips")} onClick={onClips} />
      <TopbarButton
        icon={Download}
        label={t("navExport")}
        onClick={onExport}
        primary
      />
      <button
        type="button"
        onClick={onSettings}
        title={t("navSettings")}
        aria-label={t("navSettings")}
        className="grid h-8 w-8 place-items-center rounded-md text-[var(--color-fg-muted)] hover:bg-[var(--color-bg-surface)] hover:text-[var(--color-fg)]"
      >
        <SettingsIcon size={16} />
      </button>
    </div>
  );
}

function TopbarButton({
  icon: Icon,
  label,
  onClick,
  primary,
}: {
  icon: LucideIcon;
  label: string;
  onClick: () => void;
  primary?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "inline-flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-[var(--text-ui-sm)] font-medium transition-colors",
        primary
          ? "bg-[var(--color-accent-600)] text-[var(--color-neutral-950)] hover:bg-[var(--color-accent-500)]"
          : "text-[var(--color-fg-muted)] hover:bg-[var(--color-bg-surface)] hover:text-[var(--color-fg)]",
      )}
    >
      <Icon size={15} /> {label}
    </button>
  );
}

function UpdateBanner({
  update,
  onDismiss,
}: {
  update: Update;
  onDismiss: () => void;
}) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function install() {
    setBusy(true);
    setError(null);
    try {
      await installAndRelaunch(update);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  }

  return (
    <div className="fixed inset-x-0 top-0 z-50 flex items-center justify-center gap-3 bg-[var(--color-accent-600)] px-4 py-1.5 text-[var(--text-ui-sm)] text-[var(--color-neutral-950)] shadow-md">
      <RefreshCw size={14} className={cn(busy && "animate-spin")} />
      <span className="font-medium">
        {error
          ? t("updateFailed", { error })
          : busy
            ? t("updateInstalling")
            : t("updateAvailable", { version: update.version })}
      </span>
      {!busy && !error && (
        <>
          <button
            type="button"
            onClick={install}
            className="rounded bg-[var(--color-neutral-950)]/15 px-2.5 py-0.5 font-semibold hover:bg-[var(--color-neutral-950)]/25"
          >
            {t("updateNow")}
          </button>
          <button
            type="button"
            onClick={onDismiss}
            className="px-1 opacity-70 hover:opacity-100"
            aria-label={t("actionClose")}
          >
            {t("updateLater")}
          </button>
        </>
      )}
    </div>
  );
}

function NavIcon({
  active,
  onClick,
  title,
  children,
}: {
  active: boolean;
  onClick: () => void;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      aria-label={title}
      className={cn(
        "grid h-10 w-10 place-items-center rounded-lg transition-colors",
        active
          ? "bg-[var(--color-bg-surface)] text-[var(--color-accent-400)]"
          : "text-[var(--color-fg-muted)] hover:bg-[var(--color-bg-surface)] hover:text-[var(--color-fg)]",
      )}
    >
      {children}
    </button>
  );
}

function fmtDuration(ms: number): string {
  const totalSec = Math.floor(ms / 1000);
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = totalSec % 60;
  return h > 0
    ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`
    : `${m}:${String(s).padStart(2, "0")}`;
}

export default App;
