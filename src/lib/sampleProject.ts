/**
 * A bundled sample project so the editor has something to render before
 * the video-import + Whisper pipeline (Phases 1–2) exist. Confidence
 * values are hand-picked to demonstrate all four tiers.
 *
 * Phase 9.1's onboarding replaces this with a real 60-second demo video
 * the user transcribes as a tour.
 */

import type { Project, Word } from "./bindings";

function w(
  text: string,
  start: number,
  end: number,
  confidence: number,
  extra?: Partial<Word>,
): Word {
  return {
    text,
    start_ms: start,
    end_ms: end,
    confidence,
    edited: false,
    locked: false,
    polished: false,
    alternates: [],
    ...extra,
  };
}

export const SAMPLE_PROJECT: Project = {
  id: "sample",
  name: "Demo — Sunday sermon clip.mp4",
  video_path: "/demo/sermon.mp4",
  video_content_hash: "demo",
  video_duration_ms: 18_000,
  video_width: 1920,
  video_height: 1080,
  video_fps: 30,
  audio_wav_path: null,
  language: "no",
  context_description:
    "A sermon excerpt discussing Christology and soteriology.",
  default_style: {
    id: "preset:broadcast_news",
    name: "Broadcast News",
    font_family: "Helvetica Neue",
    font_size_px: 42,
    font_weight: 600,
    italic: false,
    color_fg: "#FFFFFF",
    outline_color: "#000000",
    outline_width_px: 3,
    shadow_color: "#00000080",
    shadow_offset_x: 0,
    shadow_offset_y: 2,
    shadow_blur: 6,
    background_color: null,
    background_padding_px: 0,
    background_radius_px: 0,
    align_h: "center",
    align_v: "bottom",
    anchor: "bc",
    max_width_pct: 80,
    line_spacing: 1.1,
    letter_spacing: 0,
    animation: { kind: "fade", duration_ms: 200, per_word_delay_ms: 0 },
  },
  captions: [
    {
      id: "c1",
      start_ms: 0,
      end_ms: 4200,
      words: [
        w("Velkommen", 0, 700, 96),
        w("til", 700, 900, 98),
        w("gudstjenesten", 900, 1800, 91),
        w("denne", 1800, 2100, 88),
        w("søndagen", 2100, 2900, 72), // tier 2
        w("morgen", 2900, 4200, 64), // tier 3
      ],
      speaker_id: "s1",
      style_id: null,
      notes: null,
      ai_generated: true,
      last_edited_at: 0,
      track_id: "tc",
    },
    {
      id: "c2",
      start_ms: 4500,
      end_ms: 9800,
      words: [
        w("I", 4500, 4700, 97),
        w("dag", 4700, 5000, 95),
        w("skal", 5000, 5300, 94),
        w("vi", 5300, 5500, 96),
        w("snakke", 5500, 6000, 90),
        w("om", 6000, 6200, 97),
        w("kerigma", 6200, 7100, 38, {
          // tier 4 — the demo case from the plan
          alternates: [
            { text: "kerygma", confidence: 71 },
            { text: "karisma", confidence: 44 },
            { text: "kerigma", confidence: 38 },
          ],
        }),
        w("og", 7100, 7300, 98),
        w("frelse", 7300, 9800, 86),
      ],
      speaker_id: "s1",
      style_id: null,
      notes:
        "Speaker uses the theological term — context priming would fix 'kerigma' → 'kerygma'.",
      ai_generated: true,
      last_edited_at: 0,
      track_id: "tc",
    },
    {
      id: "c3",
      start_ms: 10_200,
      end_ms: 15_000,
      words: [
        w("La", 10_200, 10_400, 93),
        w("oss", 10_400, 10_700, 95),
        w("be", 10_700, 11_100, 55), // tier 3
        w("sammen", 11_100, 11_900, 89),
        w("før", 11_900, 12_300, 81), // tier 2
        w("vi", 12_300, 12_600, 96),
        w("begynner", 12_600, 15_000, 87),
      ],
      speaker_id: "s1",
      style_id: null,
      notes: null,
      ai_generated: true,
      last_edited_at: 0,
      track_id: "tc",
    },
  ],
  speakers: [{ id: "s1", display_name: "Pastor Lars", color_hex: "#4FD1C5" }],
  glossary: [
    {
      id: "g1",
      term: "kerygma",
      aliases: ["kerigma", "karisma"],
      definition: "The proclamation of the gospel.",
      pronunciation_hint: null,
    },
    {
      id: "g2",
      term: "soteriologi",
      aliases: ["soteorologi"],
      definition: "The study of salvation.",
      pronunciation_hint: null,
    },
  ],
  clips: [],
  talk_summary: null,
  export_config: {
    format: "srt",
    burn_in: false,
    caption_size_px: 24,
    caption_color: "white",
    caption_background: "semitransparent",
    max_chars_per_line: 42,
  },
  project_meta: {
    title: "",
    description: "",
    proper_nouns: "",
    language: "auto",
  },
  media: [
    {
      id: "m1",
      path: "/demo/sermon.mp4",
      content_hash: "demo",
      kind: "video",
      duration_ms: 18_000,
      width: 1920,
      height: 1080,
      fps: 30,
      has_audio: true,
      audio_wav_path: null,
      original_filename: "sermon.mp4",
      added_at: 0,
    },
  ],
  tracks: [
    {
      id: "tv",
      kind: "video",
      name: "Video",
      index: 0,
      enabled: true,
      locked: false,
      muted: false,
      solo: false,
      volume_db: 0,
    },
    {
      id: "tc",
      kind: "caption",
      name: "Captions",
      index: 1,
      enabled: true,
      locked: false,
      muted: false,
      solo: false,
      volume_db: 0,
    },
  ],
  timeline_items: [
    {
      id: "ti1",
      track_id: "tv",
      kind: "av",
      source_media_id: "m1",
      in_ms: 0,
      out_ms: 18_000,
      timeline_start_ms: 0,
      speed: 1,
      gain_db: 0,
      fade_in_ms: 0,
      fade_out_ms: 0,
      transform: {
        x: 0,
        y: 0,
        scale: 1,
        rotation_deg: 0,
        opacity: 1,
        crop: null,
      },
      effects: [],
      transition_in: null,
      text: null,
      enabled: true,
      locked: false,
    },
  ],
  created_at: 0,
  updated_at: 0,
};
