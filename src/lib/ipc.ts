/**
 * Typed wrappers around Tauri's invoke().
 *
 * For v1, the project state lives in the renderer; operations are pure
 * round-trips: send the project + the operation params, get the new
 * project back. This keeps the Rust layer stateless and makes undo a
 * matter of keeping previous states client-side.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AppError,
  AsrOptions,
  BurnInOptions,
  Caption,
  ClaudeModel,
  Clip,
  ClipPlan,
  CloudCostEstimate,
  CloudProvider,
  CloudProviderInfo,
  DownloadProgress,
  ExportPreset,
  ExportWarning,
  ExtractedDocument,
  FillerHit,
  FindMatch,
  FindOptions,
  Gap,
  GlossaryApplyResult,
  ImportRequest,
  MediaAvailability,
  TimelineItemKind,
  TrackKind,
  Transform,
  PolishEstimate,
  PolishResult,
  Project,
  ReflowConfig,
  ReflowIssue,
  ReplaceResult,
  SecretProvider,
  SecretStatus,
  SilenceGap,
  Strictness,
  Suggestion,
  SuggestedTerm,
  StylePreset,
  TranscribeProgress,
  TranslationLanguage,
  TranslationResult,
  VideoMetadata,
  WaveformData,
  WhisperModel,
  WhisperModelInfo,
} from "./bindings";

export class IPCError extends Error {
  readonly code: AppError["code"];
  constructor(err: AppError) {
    super(err.message);
    this.code = err.code;
    this.name = "IPCError";
  }
}

async function call<T>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (raw) {
    if (raw && typeof raw === "object" && "code" in raw && "message" in raw) {
      throw new IPCError(raw as AppError);
    }
    throw raw instanceof Error ? raw : new Error(String(raw));
  }
}

// ── Caption operations ────────────────────────────────────────────────────────
export const ops = {
  splitCaption: (project: Project, captionId: string, atWordIndex: number) =>
    call<Project>("op_split_caption", { project, captionId, atWordIndex }),
  mergeCaptions: (project: Project, captionIds: string[]) =>
    call<Project>("op_merge_captions", { project, captionIds }),
  shiftAll: (project: Project, offsetMs: number) =>
    call<Project>("op_shift_all_captions", { project, offsetMs }),
  /** Slide one caption (+ its words) along the timeline; clamped to neighbours. */
  moveCaption: (project: Project, captionId: string, deltaMs: number) =>
    call<Project>("op_move_caption", { project, captionId, deltaMs }),
  /** Drag a caption's start/end edges; clamped to neighbours + its own words. */
  resizeCaption: (
    project: Project,
    captionId: string,
    newStartMs: number,
    newEndMs: number,
  ) =>
    call<Project>("op_resize_caption", {
      project,
      captionId,
      newStartMs,
      newEndMs,
    }),
  editWord: (
    project: Project,
    captionId: string,
    wordIndex: number,
    newText: string,
  ) =>
    call<Project>("op_edit_word", { project, captionId, wordIndex, newText }),
  lockWord: (
    project: Project,
    captionId: string,
    wordIndex: number,
    locked: boolean,
  ) => call<Project>("op_lock_word", { project, captionId, wordIndex, locked }),
  acceptAlternate: (
    project: Project,
    captionId: string,
    wordIndex: number,
    alternateIndex: number,
  ) =>
    call<Project>("op_accept_alternate", {
      project,
      captionId,
      wordIndex,
      alternateIndex,
    }),
  retimeWord: (
    project: Project,
    captionId: string,
    wordIndex: number,
    newStartMs: number,
    newEndMs: number,
  ) =>
    call<Project>("op_retime_word", {
      project,
      captionId,
      wordIndex,
      newStartMs,
      newEndMs,
    }),
  /** Killer feature #2 post-pass: apply glossary aliases → canonical terms. */
  applyGlossary: (project: Project) =>
    call<GlossaryApplyResult>("op_apply_glossary", { project }),
};

// ── NLE timeline / clip-track operations (multi-track) ───────────────────────
// Same pure-round-trip shape as `ops`: send the project + params, get the new
// project back. New entity ids are generated on the Rust side.
export const timeline = {
  /** Probe + hash a media file and add it to the project's media pool. */
  importMedia: (project: Project, path: string) =>
    call<Project>("op_import_media", { project, path }),
  /** Remove a media item — rejected if any timeline item still references it. */
  removeMedia: (project: Project, mediaId: string) =>
    call<Project>("op_remove_media", { project, mediaId }),
  /**
   * Repoint a pool entry at a file that moved or was renamed. The media **id**
   * is kept, so every clip that referenced it stays attached; the new file is
   * re-probed and re-hashed, clips the (possibly shorter) new duration no
   * longer fits are clamped, and the legacy `video_*` scalars follow along
   * when this was the project's primary video.
   *
   * Pair with `project.checkMediaPaths` (what is missing) and
   * `project.relink` (auto-search by content hash) — this is the commit step,
   * so route it through `useProjectStore.run` to keep it undoable.
   */
  relinkMedia: (project: Project, mediaId: string, newPath: string) =>
    call<Project>("op_relink_media", { project, mediaId, newPath }),
  /** Add a track of the given kind; it gets the next stacking index. */
  addTrack: (project: Project, kind: TrackKind, name: string) =>
    call<Project>("op_add_track", { project, kind, name }),
  /** Remove a track — rejected unless it's empty. */
  removeTrack: (project: Project, trackId: string) =>
    call<Project>("op_remove_track", { project, trackId }),
  /** Restack a track to `newIndex`; every track is renumbered densely. */
  reorderTrack: (project: Project, trackId: string, newIndex: number) =>
    call<Project>("op_reorder_track", { project, trackId, newIndex }),
  /** Toggle any subset of a track's flags; omit a flag to leave it unchanged. */
  setTrackFlags: (
    project: Project,
    trackId: string,
    flags: {
      enabled?: boolean;
      locked?: boolean;
      muted?: boolean;
      solo?: boolean;
    },
  ) =>
    call<Project>("op_set_track_flags", {
      project,
      trackId,
      enabled: flags.enabled ?? null,
      locked: flags.locked ?? null,
      muted: flags.muted ?? null,
      solo: flags.solo ?? null,
    }),
  /** Place a clip on a track; in/out clamp to the source media duration. */
  addTimelineItem: (
    project: Project,
    trackId: string,
    sourceMediaId: string | null,
    inMs: number,
    outMs: number,
    timelineStartMs: number,
    kind: TimelineItemKind,
  ) =>
    call<Project>("op_add_timeline_item", {
      project,
      trackId,
      sourceMediaId,
      inMs,
      outMs,
      timelineStartMs,
      kind,
    }),
  /** Split a clip at a timeline position; the split maps back into the source. */
  splitTimelineItem: (project: Project, itemId: string, atTimelineMs: number) =>
    call<Project>("op_split_timeline_item", { project, itemId, atTimelineMs }),
  /** Edge-drag trim; each edge clamps to source bounds + neighbours. */
  trimTimelineItem: (
    project: Project,
    itemId: string,
    edges: {
      newInMs?: number;
      newOutMs?: number;
      newTimelineStartMs?: number;
    },
  ) =>
    call<Project>("op_trim_timeline_item", {
      project,
      itemId,
      newInMs: edges.newInMs ?? null,
      newOutMs: edges.newOutMs ?? null,
      newTimelineStartMs: edges.newTimelineStartMs ?? null,
    }),
  /** Move a clip along time and/or across tracks; clamped to avoid overlap. */
  moveTimelineItem: (
    project: Project,
    itemId: string,
    newTrackId: string,
    newTimelineStartMs: number,
  ) =>
    call<Project>("op_move_timeline_item", {
      project,
      itemId,
      newTrackId,
      newTimelineStartMs,
    }),
  /** Delete a clip and slide later same-track clips left to close the gap. */
  rippleDeleteItem: (project: Project, itemId: string) =>
    call<Project>("op_ripple_delete_item", { project, itemId }),
  /** Query the empty stretches on one track. Read-only — no undo entry. */
  detectGaps: (project: Project, trackId: string) =>
    call<Gap[]>("timeline_detect_gaps", { project, trackId }),
  /** Open `durationMs` of empty time at `atMs`; ripple stops at a locked clip. */
  insertGap: (
    project: Project,
    trackId: string,
    atMs: number,
    durationMs: number,
  ) => call<Project>("op_insert_gap", { project, trackId, atMs, durationMs }),
  /** Close the gap containing `atMs`; no-op if `atMs` isn't in an unprotected gap. */
  removeGap: (project: Project, trackId: string, atMs: number) =>
    call<Project>("op_remove_gap", { project, trackId, atMs }),
  /** Close every gap on the track, left to right; locked clips stay anchored. */
  packTrack: (project: Project, trackId: string) =>
    call<Project>("op_pack_track", { project, trackId }),
  /** Set the leading-edge transition; duration clamps to the clip length. */
  setTransition: (
    project: Project,
    itemId: string,
    kind: string,
    durationMs: number,
  ) =>
    call<Project>("op_set_transition", { project, itemId, kind, durationMs }),
  /** Remove a clip's leading transition. */
  clearTransition: (project: Project, itemId: string) =>
    call<Project>("op_clear_transition", { project, itemId }),
  /** Replace a clip's geometric transform (opacity/scale clamp server-side). */
  setTransform: (project: Project, itemId: string, transform: Transform) =>
    call<Project>("op_set_transform", { project, itemId, transform }),
  /**
   * Add or update ONE curated colour effect on a clip (E6). `kind` must be an
   * id from `features/timeline/effects/registry.ts`; the backend rejects
   * anything else, because an effect the ffmpeg export cannot render has no
   * business in a project file. Params are clamped server-side.
   */
  setEffect: (
    project: Project,
    itemId: string,
    kind: string,
    params: Record<string, number>,
    enabled: boolean,
  ) =>
    call<Project>("op_set_effect", { project, itemId, kind, params, enabled }),
  /** Remove a clip's effect of the given kind (no-op when absent). */
  removeEffect: (project: Project, itemId: string, kind: string) =>
    call<Project>("op_remove_effect", { project, itemId, kind }),
  /**
   * Set any subset of a clip's audio parameters (R2). Omit a field to leave it
   * unchanged — so one slider can be dragged without resending the others.
   *
   * Everything is clamped server-side: `gainDb` to [-60, +12], each fade to
   * [0, the clip's own timeline length]. The clamp also re-runs on every LATER
   * op, so a fade survives a trim by shrinking rather than by lying.
   */
  setItemAudio: (
    project: Project,
    itemId: string,
    audio: { gainDb?: number; fadeInMs?: number; fadeOutMs?: number },
  ) =>
    call<Project>("op_set_item_audio", {
      project,
      itemId,
      gainDb: audio.gainDb ?? null,
      fadeInMs: audio.fadeInMs ?? null,
      fadeOutMs: audio.fadeOutMs ?? null,
    }),
  /**
   * Set a track's fader, in dB (clamped to [-60, +12] server-side). Separate
   * from `setTrackFlags` on purpose: mute/solo are switches, this is a
   * continuous value dragged in bursts — pass a `coalesceKey` when committing.
   */
  setTrackVolume: (project: Project, trackId: string, volumeDb: number) =>
    call<Project>("op_set_track_volume", { project, trackId, volumeDb }),
  /** Add a standalone text overlay clip (no source media). */
  addTextItem: (
    project: Project,
    trackId: string,
    timelineStartMs: number,
    durationMs: number,
    text: string,
  ) =>
    call<Project>("op_add_text_item", {
      project,
      trackId,
      timelineStartMs,
      durationMs,
      text,
    }),
};

// ── Compose / render engine (stub — backed in the Motor+UI phase) ────────────
// The Rust `compose_render` / `compose_cancel` commands land later; the string
// names are wired here now so the timeline UI can reference the engine.
export const compose = {
  /** Render the timeline to `output` with the given settings (ffmpeg pipeline). */
  render: (project: Project, output: string, settings: unknown) =>
    call<void>("compose_render", { project, output, settings }),
  /** Render a fast LOW-RES preview proxy of the timeline to `output`. Settings
   *  are derived from the project on the backend; progress streams on the
   *  `compose-proxy-progress` event. */
  previewProxy: (project: Project, output: string) =>
    call<void>("compose_preview_proxy", { project, output }),
  /** Cancel an in-flight compose (or proxy) render. */
  cancel: () => call<void>("compose_cancel"),
};

// ── Media utilities (thumbnails, filmstrip tiles) ────────────────────────────
export const media = {
  /** Grab a single 120px-tall thumbnail frame from `mediaPath` at `atMs`,
   *  writing a JPEG to `outPath`. Returns the written path. */
  thumbnail: (mediaPath: string, atMs: number, outPath: string) =>
    call<string>("extract_thumbnail", { mediaPath, atMs, outPath }),
  /** Render one filmstrip tile — `cols` frames from `[startMs, endMs)` tiled
   *  into a single JPEG at `outPath`. Ranges should come from the fixed
   *  per-zoom-tier grid (`tiles.ts`), so rendered files stay addressable and
   *  reusable across scroll and zoom. Returns the written path. */
  filmstripTile: (
    mediaPath: string,
    startMs: number,
    endMs: number,
    cols: number,
    outPath: string,
  ) =>
    call<string>("extract_filmstrip_tile", {
      mediaPath,
      startMs,
      endMs,
      cols,
      outPath,
    }),
};

// ── Export ──────────────────────────────────────────────────────────────────
export const exporters = {
  srt: (project: Project, includeSpeakers = false, stripEmpty = true) =>
    call<string>("export_srt", { project, includeSpeakers, stripEmpty }),
  vtt: (project: Project, includeSpeakers = false, stripEmpty = true) =>
    call<string>("export_vtt", { project, includeSpeakers, stripEmpty }),
  ass: (project: Project) => call<string>("export_ass", { project }),
  txt: (project: Project, includeSpeakers = false, stripEmpty = true) =>
    call<string>("export_txt", { project, includeSpeakers, stripEmpty }),
  /** Developer-facing JSON: per-word timing + confidence, stable schema. */
  json: (project: Project, stripEmpty = true) =>
    call<string>("export_json", { project, stripEmpty }),
  /** Regenerate `format` (srt/vtt/ass/txt/json/docx) and write it to `path`. */
  save: (
    project: Project,
    path: string,
    format: string,
    includeSpeakers = true,
    stripEmpty = true,
  ) =>
    call<void>("save_export", {
      project,
      path,
      format,
      includeSpeakers,
      stripEmpty,
    }),
};

// ── Project lifecycle + video import (Phase 1) ───────────────────────────────
export const project = {
  probe: (path: string) => call<VideoMetadata>("video_probe", { path }),
  createFromVideo: (path: string) =>
    call<Project>("project_create_from_video", { path }),
  save: (proj: Project, path: string) =>
    call<void>("project_save", { project: proj, path }),
  open: (path: string) => call<Project>("project_open", { path }),
  waveform: (videoPath: string, cacheDir: string) =>
    call<WaveformData>("waveform_compute", { videoPath, cacheDir }),
  /** Extract the source media's audio to a 16 kHz mono WAV (what local
   *  Whisper + diarization need) and return its path. Shares the cached WAV
   *  with `waveform`. */
  extractAudio: (videoPath: string, cacheDir: string) =>
    call<string>("extract_audio", { videoPath, cacheDir }),
  /**
   * Which pooled media files are still on disk. One `exists` stat per
   * `MediaItem`, no hashing — cheap enough to run on every project open, so
   * a moved source surfaces the relink affordance instead of a dead preview
   * and a failing export. Feed the missing rows to `relink` (auto-search),
   * then `timeline.relinkMedia` (commit).
   */
  checkMediaPaths: (proj: Project) =>
    call<MediaAvailability[]>("check_media_paths", { project: proj }),
  relink: (
    targetHash: string,
    searchDirs: string[],
    originalFilename?: string,
  ) =>
    call<string | null>("project_relink", {
      targetHash,
      searchDirs,
      originalFilename,
    }),
  acceptedExtensions: () => call<string[]>("accepted_media_extensions"),
};

// ── Sunday-link deep-link import (Phase 8) ───────────────────────────────────
/** Inbound `sundayedit://import?…` links from sister Sunday-suite apps. The
 *  native handler emits the raw URL on `EVENT_DEEP_LINK_IMPORT`; the app then
 *  validates it via `parseImport` and drives the normal import + context flow. */
export const EVENT_DEEP_LINK_IMPORT = "deep-link://import";
export const deeplink = {
  /** Validate + structure a raw `sundayedit://import?…` URL. */
  parseImport: (url: string) =>
    call<ImportRequest>("deeplink_parse_import", { url }),
  /** Build the `<returnTo>://captions?path=…[&recording=…]` hand-back URL for
   *  the caller. `recordingPath` echoes back the media we were sent so the
   *  source app attaches the captions to the right recording. */
  captionsCallbackUrl: (
    returnTo: string,
    sidecarPath: string,
    recordingPath?: string | null,
  ) =>
    call<string>("deeplink_captions_callback_url", {
      returnTo,
      sidecarPath,
      recordingPath: recordingPath ?? null,
    }),
  /** Subscribe to inbound deep-link import URLs (emitted by the native layer). */
  onImport: (cb: (url: string) => void): Promise<UnlistenFn> =>
    listen<string>(EVENT_DEEP_LINK_IMPORT, (e) => cb(e.payload)),
};

// ── ASR / transcription (Phase 2) ────────────────────────────────────────────
export const asr = {
  listModels: () => call<WhisperModelInfo[]>("asr_list_models"),
  /** Cloud provider catalog (names, price/min, privacy URL) — Phase 2.2. */
  cloudProviders: () => call<CloudProviderInfo[]>("cloud_providers"),
  /** Pre-submit cloud cost preview for `durationMs` of audio. */
  cloudCostEstimate: (provider: CloudProvider, durationMs: number) =>
    call<CloudCostEstimate>("cloud_cost_estimate", {
      provider,
      durationMs,
    }),
  /** Transcribe the project's audio via a cloud provider (BYOK). OpenAI is
   *  wired; others error clearly. Returns editor-ready captions. */
  cloudTranscribe: (
    project: Project,
    provider: CloudProvider,
    apiKey?: string,
    language?: string,
  ) =>
    call<Caption[]>("cloud_transcribe", {
      project,
      provider,
      apiKey: apiKey ?? null,
      language: language ?? null,
    }),
  downloadedModels: (modelsDir: string) =>
    call<WhisperModel[]>("asr_downloaded_models", { modelsDir }),
  /** Fetch a model into `modelsDir`. Resolves when it's on disk. Listen for
   *  progress via `onDownloadProgress`. */
  downloadModel: (modelsDir: string, model: WhisperModel) =>
    call<void>("asr_download_model", { modelsDir, model }),
  cancelDownload: () => call<void>("asr_cancel_download"),
  /** Subscribe to model-download progress. Returns an unlisten function. */
  onDownloadProgress: (
    cb: (p: DownloadProgress) => void,
  ): Promise<UnlistenFn> =>
    listen<DownloadProgress>("model-download-progress", (e) => cb(e.payload)),
  /** Transcribe a 16 kHz mono WAV with the local Whisper model. Streams
   *  progress via `onTranscribeProgress`. Returns editor-ready captions. On a
   *  build without the `whisper` feature this errors clearly. */
  transcribeLocal: (
    audioPath: string,
    modelsDir: string,
    model: WhisperModel,
    options: AsrOptions,
  ) =>
    call<Caption[]>("asr_transcribe_local", {
      audioPath,
      modelsDir,
      model,
      options,
    }),
  /** Cancel the in-flight local transcription (the run errors "cancelled"). */
  cancelTranscribe: () => call<void>("asr_cancel_transcribe"),
  /** Subscribe to local-transcription progress. Returns an unlisten function. */
  onTranscribeProgress: (
    cb: (p: TranscribeProgress) => void,
  ): Promise<UnlistenFn> =>
    listen<TranscribeProgress>("transcribe-progress", (e) => cb(e.payload)),
};

// ── Styling (Phase 5) ─────────────────────────────────────────────────────────
export const style = {
  listPresets: () => call<StylePreset[]>("style_list_presets"),
};

// ── API key storage (Phase 2.2) ──────────────────────────────────────────────
// The renderer only ever learns whether a key is set — never its value.
export const secrets = {
  status: () => call<SecretStatus[]>("secret_status"),
  set: (provider: SecretProvider, value: string) =>
    call<void>("secret_set", { provider, value }),
  delete: (provider: SecretProvider) =>
    call<void>("secret_delete", { provider }),
};

// ── Burn-in + platform export (Phase 6.2 / 6.3) ──────────────────────────────
export const render = {
  listExportPresets: () => call<ExportPreset[]>("export_list_presets"),
  validate: (project: Project, preset: ExportPreset) =>
    call<ExportWarning[]>("export_validate", { project, preset }),
  burnIn: (project: Project, output: string, options: BurnInOptions) =>
    call<void>("burnin_render", { project, output, options }),
  burnInPreset: (project: Project, output: string, preset: ExportPreset) =>
    call<void>("burnin_render_preset", { project, output, preset }),
};

// ── Find/replace + filler cleanup (Phase 7) ──────────────────────────────────
export const cleanup = {
  find: (project: Project, options: FindOptions) =>
    call<FindMatch[]>("find_in_project", { project, options }),
  replace: (project: Project, options: FindOptions, replacement: string) =>
    call<ReplaceResult>("replace_in_project", {
      project,
      options,
      replacement,
    }),
  bulkDelete: (project: Project, captionIds: string[]) =>
    call<Project>("bulk_delete_captions", { project, captionIds }),
  bulkSetSpeaker: (
    project: Project,
    captionIds: string[],
    speakerId: string | null,
  ) => call<Project>("bulk_set_speaker", { project, captionIds, speakerId }),
  detectFillers: (project: Project, language: string) =>
    call<FillerHit[]>("detect_fillers", { project, language }),
  detectSilences: (project: Project, minGapMs: number) =>
    call<SilenceGap[]>("detect_silences", { project, minGapMs }),
  applyRippleCuts: (project: Project, cuts: Array<[number, number]>) =>
    call<Project>("apply_ripple_cuts", { project, cuts }),
};

// ── Caption readability / re-flow (Phase 7) ──────────────────────────────────
/** Conservative broadcast defaults — mirrors `ReflowConfig::default()` in
 *  `services::reflow` (CPS 17, 37 chars/line, 2 lines, 833 ms min). Kept here
 *  so the panel can offer a one-click reset without a round-trip. */
export const DEFAULT_REFLOW_CONFIG: ReflowConfig = {
  max_cps: 17,
  max_chars_per_line: 37,
  max_lines: 2,
  min_duration_ms: 833,
};

export const reflow = {
  /** Flag every caption that breaks a readability limit. Empty array = clean. */
  analyze: (project: Project, cfg: ReflowConfig) =>
    call<ReflowIssue[]>("reflow_analyze", { project, cfg }),
  /** Auto-split offenders into the fewest broadcast-compliant captions. Clean
   *  captions pass through unchanged. Returns the repaired project. */
  repair: (project: Project, cfg: ReflowConfig) =>
    call<Project>("reflow_repair", { project, cfg }),
};

// ── AI punctuation polish (Phase 4.1) ────────────────────────────────────────
export const polish = {
  /** Pure cost/scope preview — no network, safe to call freely. */
  estimate: (project: Project, model: ClaudeModel) =>
    call<PolishEstimate>("polish_estimate", { project, model }),
  /** Run the polish. `apiKey` falls back to ANTHROPIC_API_KEY on the backend. */
  run: (project: Project, model: ClaudeModel, apiKey?: string) =>
    call<PolishResult>("polish_captions", {
      project,
      model,
      apiKey: apiKey ?? null,
    }),
};

// ── AI social clips (SundayEdit) ─────────────────────────────────────────────
export const clips = {
  /** Pure cost/scope preview — no network, safe to call freely. */
  estimate: (project: Project, model: ClaudeModel) =>
    call<PolishEstimate>("clips_estimate", { project, model }),
  /** Generate a reviewable clip plan from the transcript. Applies nothing. */
  generate: (project: Project, model: ClaudeModel, apiKey?: string) =>
    call<ClipPlan>("clips_generate", {
      project,
      model,
      apiKey: apiKey ?? null,
    }),
  /** Persist a reviewed plan (clips + talk summary) → updated project. */
  applyPlan: (project: Project, plan: ClipPlan) =>
    call<Project>("clips_apply_plan", { project, plan }),
  /** Render one clip as a vertical video with its title overlay burned in. */
  render: (
    project: Project,
    clip: Clip,
    output: string,
    preset: ExportPreset,
  ) => call<void>("clip_burnin_render", { project, clip, output, preset }),
};

// ── AI glossary suggestions (Phase 3.4 mode 3) ───────────────────────────────
export const glossary = {
  /** Pure cost/scope preview — no network. */
  estimate: (project: Project, model: ClaudeModel) =>
    call<PolishEstimate>("glossary_suggest_estimate", { project, model }),
  /** Scan the transcript → candidate terms for review. Adds nothing. */
  suggest: (project: Project, model: ClaudeModel, apiKey?: string) =>
    call<SuggestedTerm[]>("glossary_suggest", {
      project,
      model,
      apiKey: apiKey ?? null,
    }),
  /** Mode 4: read a reference document (.txt/.md/.docx) to plain text. */
  extractDocument: (path: string) =>
    call<ExtractedDocument>("glossary_extract_document", { path }),
  /** Mode 4: pure cost preview for a document pass — no network. */
  estimateFromDocument: (
    project: Project,
    model: ClaudeModel,
    documentText: string,
  ) =>
    call<PolishEstimate>("glossary_from_document_estimate", {
      project,
      model,
      documentText,
    }),
  /** Mode 4: propose terms from a reference document. Adds nothing. */
  suggestFromDocument: (
    project: Project,
    model: ClaudeModel,
    documentText: string,
    apiKey?: string,
  ) =>
    call<SuggestedTerm[]>("glossary_from_document", {
      project,
      model,
      documentText,
      apiKey: apiKey ?? null,
    }),
};

// ── AI smart suggestions (Phase 4.3) ─────────────────────────────────────────
export const suggest = {
  /** Pure cost/scope preview — no network. */
  estimate: (project: Project, model: ClaudeModel, strictness: Strictness) =>
    call<PolishEstimate>("suggest_estimate", { project, model, strictness }),
  /** Run a Smart Suggest pass → review queue. Applies nothing. */
  run: (
    project: Project,
    model: ClaudeModel,
    strictness: Strictness,
    apiKey?: string,
  ) =>
    call<Suggestion[]>("suggest_captions", {
      project,
      model,
      strictness,
      apiKey: apiKey ?? null,
    }),
  /** Apply one accepted suggestion → updated project. */
  apply: (project: Project, suggestion: Suggestion) =>
    call<Project>("apply_suggestion", { project, suggestion }),
};

// ── AI translation (Phase 7.1) ───────────────────────────────────────────────
export const translate = {
  languages: () => call<TranslationLanguage[]>("translate_supported_languages"),
  /** Pure cost/scope preview — no network. */
  estimate: (project: Project, targetLanguage: string, model: ClaudeModel) =>
    call<PolishEstimate>("translate_estimate", {
      project,
      targetLanguage,
      model,
    }),
  /** Translate the track → result (captions + warnings). Does not mutate. */
  run: (
    project: Project,
    targetLanguage: string,
    model: ClaudeModel,
    apiKey?: string,
  ) =>
    call<TranslationResult>("translate_captions", {
      project,
      targetLanguage,
      model,
      apiKey: apiKey ?? null,
    }),
};

// ── Speaker diarization (Phase 4.2) ──────────────────────────────────────────
export const diarize = {
  /** Detect speakers from extracted audio + attribute captions. Best-effort. */
  run: (project: Project, audioPath: string) =>
    call<Project>("diarize_run", { project, audioPath }),
  mergeSpeakers: (project: Project, keepId: string, removeId: string) =>
    call<Project>("speaker_merge", { project, keepId, removeId }),
  renameSpeaker: (project: Project, speakerId: string, name: string) =>
    call<Project>("speaker_rename", { project, speakerId, name }),
  setSpeakerColor: (project: Project, speakerId: string, colorHex: string) =>
    call<Project>("speaker_set_color", { project, speakerId, colorHex }),
};

export const ipc = {
  ops,
  timeline,
  compose,
  media,
  exporters,
  project,
  deeplink,
  asr,
  style,
  secrets,
  render,
  cleanup,
  reflow,
  clips,
  glossary,
  polish,
  suggest,
  translate,
  diarize,
};
