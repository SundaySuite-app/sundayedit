/**
 * Runtime-contract pins for every `ipc.*` wrapper.
 *
 * The Tauri call boundary is dynamically typed: a wrong command name, a wrong
 * argument KEY, or the wrong case (camelCase JS ↔ snake_case Rust) compiles
 * green but fails silently at runtime. These tests mock the lowest layer
 * (`invoke` / `listen`) and assert the EXACT `(command, args)` each wrapper
 * sends, so any drift between `ipc.ts` and the registered `#[tauri::command]`
 * signatures in `src-tauri` is caught here instead of on the rig.
 *
 * The arg KEYS asserted below must match the camelCase form Tauri derives from
 * each Rust command's snake_case parameter names.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { ipc } from "./ipc";
import { SAMPLE_PROJECT } from "./sampleProject";
import type {
  AsrOptions,
  BurnInOptions,
  ClaudeModel,
  Clip,
  ClipPlan,
  CloudProvider,
  ExportPreset,
  FindOptions,
  Project,
  ReflowConfig,
  SecretProvider,
  Strictness,
  Suggestion,
  WhisperModel,
} from "./bindings";

const invoke = vi.fn();
const listen = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (...args: unknown[]) => listen(...args),
}));

const P: Project = SAMPLE_PROJECT;
const MODEL: ClaudeModel = "haiku45";

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue(undefined);
  listen.mockReset();
  listen.mockResolvedValue(() => {});
});

/** Assert the last invoke was `cmd` with exactly `args` (keys + values).
 *  `undefined` pins the no-arg commands, where the wrapper omits the 2nd arg. */
function expectCall(cmd: string, args: Record<string, unknown> | undefined) {
  expect(invoke).toHaveBeenLastCalledWith(cmd, args);
}

describe("ipc contract — caption operations", () => {
  it("op_split_caption", async () => {
    await ipc.ops.splitCaption(P, "c1", 2);
    expectCall("op_split_caption", {
      project: P,
      captionId: "c1",
      atWordIndex: 2,
    });
  });
  it("op_merge_captions", async () => {
    await ipc.ops.mergeCaptions(P, ["c1", "c2"]);
    expectCall("op_merge_captions", { project: P, captionIds: ["c1", "c2"] });
  });
  it("op_shift_all_captions", async () => {
    await ipc.ops.shiftAll(P, 250);
    expectCall("op_shift_all_captions", { project: P, offsetMs: 250 });
  });
  it("op_move_caption", async () => {
    await ipc.ops.moveCaption(P, "c1", -100);
    expectCall("op_move_caption", {
      project: P,
      captionId: "c1",
      deltaMs: -100,
    });
  });
  it("op_resize_caption", async () => {
    await ipc.ops.resizeCaption(P, "c1", 100, 900);
    expectCall("op_resize_caption", {
      project: P,
      captionId: "c1",
      newStartMs: 100,
      newEndMs: 900,
    });
  });
  it("op_edit_word", async () => {
    await ipc.ops.editWord(P, "c1", 0, "hi");
    expectCall("op_edit_word", {
      project: P,
      captionId: "c1",
      wordIndex: 0,
      newText: "hi",
    });
  });
  it("op_lock_word", async () => {
    await ipc.ops.lockWord(P, "c1", 0, true);
    expectCall("op_lock_word", {
      project: P,
      captionId: "c1",
      wordIndex: 0,
      locked: true,
    });
  });
  it("op_accept_alternate", async () => {
    await ipc.ops.acceptAlternate(P, "c1", 0, 1);
    expectCall("op_accept_alternate", {
      project: P,
      captionId: "c1",
      wordIndex: 0,
      alternateIndex: 1,
    });
  });
  it("op_retime_word", async () => {
    await ipc.ops.retimeWord(P, "c1", 0, 100, 200);
    expectCall("op_retime_word", {
      project: P,
      captionId: "c1",
      wordIndex: 0,
      newStartMs: 100,
      newEndMs: 200,
    });
  });
  it("op_apply_glossary", async () => {
    await ipc.ops.applyGlossary(P);
    expectCall("op_apply_glossary", { project: P });
  });
});

describe("ipc contract — export", () => {
  it("export_srt", async () => {
    await ipc.exporters.srt(P);
    expectCall("export_srt", {
      project: P,
      includeSpeakers: false,
      stripEmpty: true,
    });
  });
  it("export_vtt", async () => {
    await ipc.exporters.vtt(P, true, false);
    expectCall("export_vtt", {
      project: P,
      includeSpeakers: true,
      stripEmpty: false,
    });
  });
  it("export_ass", async () => {
    await ipc.exporters.ass(P);
    expectCall("export_ass", { project: P });
  });
  it("export_txt", async () => {
    await ipc.exporters.txt(P);
    expectCall("export_txt", {
      project: P,
      includeSpeakers: false,
      stripEmpty: true,
    });
  });
  it("export_json", async () => {
    await ipc.exporters.json(P);
    expectCall("export_json", { project: P, stripEmpty: true });
  });
  it("save_export", async () => {
    await ipc.exporters.save(P, "/out.srt", "srt");
    expectCall("save_export", {
      project: P,
      path: "/out.srt",
      format: "srt",
      includeSpeakers: true,
      stripEmpty: true,
    });
  });
});

describe("ipc contract — project lifecycle", () => {
  it("video_probe", async () => {
    await ipc.project.probe("/v.mp4");
    expectCall("video_probe", { path: "/v.mp4" });
  });
  it("project_create_from_video", async () => {
    await ipc.project.createFromVideo("/v.mp4");
    expectCall("project_create_from_video", { path: "/v.mp4" });
  });
  it("project_save maps proj → project", async () => {
    await ipc.project.save(P, "/p.sundayedit");
    expectCall("project_save", { project: P, path: "/p.sundayedit" });
  });
  it("project_open", async () => {
    await ipc.project.open("/p.sundayedit");
    expectCall("project_open", { path: "/p.sundayedit" });
  });
  it("waveform_compute", async () => {
    await ipc.project.waveform("/v.mp4", "/cache");
    expectCall("waveform_compute", { videoPath: "/v.mp4", cacheDir: "/cache" });
  });
  it("extract_audio", async () => {
    await ipc.project.extractAudio("/v.mp4", "/cache");
    expectCall("extract_audio", { videoPath: "/v.mp4", cacheDir: "/cache" });
  });
  it("check_media_paths", async () => {
    await ipc.project.checkMediaPaths(SAMPLE_PROJECT);
    expectCall("check_media_paths", { project: SAMPLE_PROJECT });
  });
  it("op_relink_media", async () => {
    await ipc.timeline.relinkMedia(SAMPLE_PROJECT, "m1", "/moved/talk.mp4");
    expectCall("op_relink_media", {
      project: SAMPLE_PROJECT,
      mediaId: "m1",
      newPath: "/moved/talk.mp4",
    });
  });
  it("project_relink (with + without optional filename)", async () => {
    await ipc.project.relink("hash", ["/a", "/b"]);
    expectCall("project_relink", {
      targetHash: "hash",
      searchDirs: ["/a", "/b"],
      originalFilename: undefined,
    });
    await ipc.project.relink("hash", ["/a"], "orig.mp4");
    expectCall("project_relink", {
      targetHash: "hash",
      searchDirs: ["/a"],
      originalFilename: "orig.mp4",
    });
  });
  it("accepted_media_extensions (no args)", async () => {
    await ipc.project.acceptedExtensions();
    expectCall("accepted_media_extensions", undefined);
  });
});

describe("ipc contract — deeplink", () => {
  it("deeplink_parse_import", async () => {
    await ipc.deeplink.parseImport("sundayedit://import?path=/v.mp4");
    expectCall("deeplink_parse_import", {
      url: "sundayedit://import?path=/v.mp4",
    });
  });
  it("deeplink_captions_callback_url", async () => {
    await ipc.deeplink.captionsCallbackUrl("sundayrec", "/side.srt");
    expectCall("deeplink_captions_callback_url", {
      returnTo: "sundayrec",
      sidecarPath: "/side.srt",
      recordingPath: null,
    });
  });
  it("deeplink_captions_callback_url echoes the recording path", async () => {
    await ipc.deeplink.captionsCallbackUrl(
      "sundayrec",
      "/side.srt",
      "/rec.mp4",
    );
    expectCall("deeplink_captions_callback_url", {
      returnTo: "sundayrec",
      sidecarPath: "/side.srt",
      recordingPath: "/rec.mp4",
    });
  });
  it("onImport subscribes to the deep-link event", async () => {
    await ipc.deeplink.onImport(() => {});
    expect(listen).toHaveBeenLastCalledWith(
      "deep-link://import",
      expect.any(Function),
    );
  });
});

describe("ipc contract — ASR / transcription", () => {
  const MODELS_DIR = "/models";
  const WMODEL: WhisperModel = "base";
  const PROVIDER: CloudProvider = "openai-whisper";
  it("asr_list_models (no args)", async () => {
    await ipc.asr.listModels();
    expectCall("asr_list_models", undefined);
  });
  it("cloud_providers (no args)", async () => {
    await ipc.asr.cloudProviders();
    expectCall("cloud_providers", undefined);
  });
  it("cloud_cost_estimate", async () => {
    await ipc.asr.cloudCostEstimate(PROVIDER, 60_000);
    expectCall("cloud_cost_estimate", {
      provider: PROVIDER,
      durationMs: 60_000,
    });
  });
  it("cloud_transcribe (optionals → null)", async () => {
    await ipc.asr.cloudTranscribe(P, PROVIDER);
    expectCall("cloud_transcribe", {
      project: P,
      provider: PROVIDER,
      apiKey: null,
      language: null,
    });
    await ipc.asr.cloudTranscribe(P, PROVIDER, "k", "en");
    expectCall("cloud_transcribe", {
      project: P,
      provider: PROVIDER,
      apiKey: "k",
      language: "en",
    });
  });
  it("asr_downloaded_models", async () => {
    await ipc.asr.downloadedModels(MODELS_DIR);
    expectCall("asr_downloaded_models", { modelsDir: MODELS_DIR });
  });
  it("asr_download_model", async () => {
    await ipc.asr.downloadModel(MODELS_DIR, WMODEL);
    expectCall("asr_download_model", { modelsDir: MODELS_DIR, model: WMODEL });
  });
  it("asr_cancel_download (no args)", async () => {
    await ipc.asr.cancelDownload();
    expectCall("asr_cancel_download", undefined);
  });
  it("asr_transcribe_local", async () => {
    const options: AsrOptions = {
      language: "en",
      beam_size: 5,
      priming_terms: [],
      context_description: null,
    };
    await ipc.asr.transcribeLocal("/a.wav", MODELS_DIR, WMODEL, options);
    expectCall("asr_transcribe_local", {
      audioPath: "/a.wav",
      modelsDir: MODELS_DIR,
      model: WMODEL,
      options,
    });
  });
  it("onDownloadProgress / onTranscribeProgress subscribe to the right events", async () => {
    await ipc.asr.onDownloadProgress(() => {});
    expect(listen).toHaveBeenLastCalledWith(
      "model-download-progress",
      expect.any(Function),
    );
    await ipc.asr.onTranscribeProgress(() => {});
    expect(listen).toHaveBeenLastCalledWith(
      "transcribe-progress",
      expect.any(Function),
    );
  });
});

describe("ipc contract — style + secrets", () => {
  it("style_list_presets (no args)", async () => {
    await ipc.style.listPresets();
    expectCall("style_list_presets", undefined);
  });
  it("secret_status (no args)", async () => {
    await ipc.secrets.status();
    expectCall("secret_status", undefined);
  });
  it("secret_set", async () => {
    const provider: SecretProvider = "anthropic";
    await ipc.secrets.set(provider, "sk-x");
    expectCall("secret_set", { provider, value: "sk-x" });
  });
  it("secret_delete", async () => {
    const provider: SecretProvider = "anthropic";
    await ipc.secrets.delete(provider);
    expectCall("secret_delete", { provider });
  });
});

describe("ipc contract — render / burn-in", () => {
  const PRESET = { id: "yt" } as unknown as ExportPreset;
  const OPTS = { codec: "h264" } as unknown as BurnInOptions;
  const CLIP = { id: "clip1" } as unknown as Clip;
  it("export_list_presets (no args)", async () => {
    await ipc.render.listExportPresets();
    expectCall("export_list_presets", undefined);
  });
  it("export_validate", async () => {
    await ipc.render.validate(P, PRESET);
    expectCall("export_validate", { project: P, preset: PRESET });
  });
  it("burnin_render", async () => {
    await ipc.render.burnIn(P, "/out.mp4", OPTS);
    expectCall("burnin_render", {
      project: P,
      output: "/out.mp4",
      options: OPTS,
    });
  });
  it("burnin_render_preset", async () => {
    await ipc.render.burnInPreset(P, "/out.mp4", PRESET);
    expectCall("burnin_render_preset", {
      project: P,
      output: "/out.mp4",
      preset: PRESET,
    });
  });
  it("clip_burnin_render", async () => {
    await ipc.clips.render(P, CLIP, "/clip.mp4", PRESET);
    expectCall("clip_burnin_render", {
      project: P,
      clip: CLIP,
      output: "/clip.mp4",
      preset: PRESET,
    });
  });
});

describe("ipc contract — cleanup / find-replace", () => {
  const OPTIONS: FindOptions = {
    query: "um",
    case_sensitive: false,
    whole_word: true,
    regex: false,
  };
  it("find_in_project", async () => {
    await ipc.cleanup.find(P, OPTIONS);
    expectCall("find_in_project", { project: P, options: OPTIONS });
  });
  it("replace_in_project", async () => {
    await ipc.cleanup.replace(P, OPTIONS, "uh");
    expectCall("replace_in_project", {
      project: P,
      options: OPTIONS,
      replacement: "uh",
    });
  });
  it("bulk_delete_captions", async () => {
    await ipc.cleanup.bulkDelete(P, ["c1"]);
    expectCall("bulk_delete_captions", { project: P, captionIds: ["c1"] });
  });
  it("bulk_set_speaker (null clears)", async () => {
    await ipc.cleanup.bulkSetSpeaker(P, ["c1"], null);
    expectCall("bulk_set_speaker", {
      project: P,
      captionIds: ["c1"],
      speakerId: null,
    });
  });
  it("detect_fillers", async () => {
    await ipc.cleanup.detectFillers(P, "en");
    expectCall("detect_fillers", { project: P, language: "en" });
  });
  it("detect_silences", async () => {
    await ipc.cleanup.detectSilences(P, 1000);
    expectCall("detect_silences", { project: P, minGapMs: 1000 });
  });
  it("apply_ripple_cuts (tuple array)", async () => {
    await ipc.cleanup.applyRippleCuts(P, [
      [100, 200],
      [300, 400],
    ]);
    expectCall("apply_ripple_cuts", {
      project: P,
      cuts: [
        [100, 200],
        [300, 400],
      ],
    });
  });
});

describe("ipc contract — reflow", () => {
  const CFG: ReflowConfig = {
    max_cps: 17,
    max_chars_per_line: 37,
    max_lines: 2,
    min_duration_ms: 833,
  };
  it("reflow_analyze", async () => {
    await ipc.reflow.analyze(P, CFG);
    expectCall("reflow_analyze", { project: P, cfg: CFG });
  });
  it("reflow_repair", async () => {
    await ipc.reflow.repair(P, CFG);
    expectCall("reflow_repair", { project: P, cfg: CFG });
  });
});

describe("ipc contract — AI estimates + runs", () => {
  it("polish_estimate / polish_captions", async () => {
    await ipc.polish.estimate(P, MODEL);
    expectCall("polish_estimate", { project: P, model: MODEL });
    await ipc.polish.run(P, MODEL);
    expectCall("polish_captions", { project: P, model: MODEL, apiKey: null });
  });
  it("clips_estimate / clips_generate / clips_apply_plan", async () => {
    await ipc.clips.estimate(P, MODEL);
    expectCall("clips_estimate", { project: P, model: MODEL });
    await ipc.clips.generate(P, MODEL, "k");
    expectCall("clips_generate", { project: P, model: MODEL, apiKey: "k" });
    const plan = { talk_summary: "", clips: [] } as unknown as ClipPlan;
    await ipc.clips.applyPlan(P, plan);
    expectCall("clips_apply_plan", { project: P, plan });
  });
  it("glossary suggest + document modes", async () => {
    await ipc.glossary.estimate(P, MODEL);
    expectCall("glossary_suggest_estimate", { project: P, model: MODEL });
    await ipc.glossary.suggest(P, MODEL);
    expectCall("glossary_suggest", { project: P, model: MODEL, apiKey: null });
    await ipc.glossary.extractDocument("/doc.txt");
    expectCall("glossary_extract_document", { path: "/doc.txt" });
    await ipc.glossary.estimateFromDocument(P, MODEL, "text");
    expectCall("glossary_from_document_estimate", {
      project: P,
      model: MODEL,
      documentText: "text",
    });
    await ipc.glossary.suggestFromDocument(P, MODEL, "text", "k");
    expectCall("glossary_from_document", {
      project: P,
      model: MODEL,
      documentText: "text",
      apiKey: "k",
    });
  });
  it("suggest_estimate / suggest_captions / apply_suggestion", async () => {
    const strictness: Strictness = "balanced";
    await ipc.suggest.estimate(P, MODEL, strictness);
    expectCall("suggest_estimate", { project: P, model: MODEL, strictness });
    await ipc.suggest.run(P, MODEL, strictness);
    expectCall("suggest_captions", {
      project: P,
      model: MODEL,
      strictness,
      apiKey: null,
    });
    const s = {
      caption_id: "c1",
      kind: "rephrase",
      suggestion: "x",
      reasoning: "y",
    } as unknown as Suggestion;
    await ipc.suggest.apply(P, s);
    expectCall("apply_suggestion", { project: P, suggestion: s });
  });
  it("translate languages / estimate / run", async () => {
    await ipc.translate.languages();
    expectCall("translate_supported_languages", undefined);
    await ipc.translate.estimate(P, "nb", MODEL);
    expectCall("translate_estimate", {
      project: P,
      targetLanguage: "nb",
      model: MODEL,
    });
    await ipc.translate.run(P, "nb", MODEL, "k");
    expectCall("translate_captions", {
      project: P,
      targetLanguage: "nb",
      model: MODEL,
      apiKey: "k",
    });
  });
});

describe("ipc contract — diarization + speakers", () => {
  it("diarize_run", async () => {
    await ipc.diarize.run(P, "/a.wav");
    expectCall("diarize_run", { project: P, audioPath: "/a.wav" });
  });
  it("speaker_merge", async () => {
    await ipc.diarize.mergeSpeakers(P, "keep", "drop");
    expectCall("speaker_merge", {
      project: P,
      keepId: "keep",
      removeId: "drop",
    });
  });
  it("speaker_rename", async () => {
    await ipc.diarize.renameSpeaker(P, "s1", "Pastor");
    expectCall("speaker_rename", {
      project: P,
      speakerId: "s1",
      name: "Pastor",
    });
  });
  it("speaker_set_color", async () => {
    await ipc.diarize.setSpeakerColor(P, "s1", "#ff0000");
    expectCall("speaker_set_color", {
      project: P,
      speakerId: "s1",
      colorHex: "#ff0000",
    });
  });
});
