//! Core caption domain model.
//!
//! All operations on Project/Caption/Word live in `services::operations`
//! and are PURE FUNCTIONS — they take a state and return a new state,
//! never mutate in place. This makes undo trivial (keep the previous
//! state) and the model easy to reason about.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

// ── Word ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/Word.ts")]
pub struct Word {
    pub text: String,
    // i64 in Rust, but Tauri serializes it as a JSON number — tell ts-rs to
    // emit `number` (not `bigint`) so the wire format and the type agree.
    #[ts(type = "number")]
    pub start_ms: i64,
    #[ts(type = "number")]
    pub end_ms: i64,
    /// 0–100 normalized confidence from ASR.
    pub confidence: f32,
    /// User has changed this word from the ASR output.
    pub edited: bool,
    /// User has confirmed — do not surface as uncertain even if confidence is low.
    pub locked: bool,
    /// AI polish (Phase 4.1) adjusted this word's punctuation/casing. Not a
    /// content change, so it does NOT trust the word like `edited` does —
    /// it only drives the "polished" dot in the editor.
    #[serde(default)]
    pub polished: bool,
    /// Top alternates from ASR (max 3).
    #[serde(default)]
    pub alternates: Vec<AlternateRead>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/AlternateRead.ts")]
pub struct AlternateRead {
    pub text: String,
    pub confidence: f32,
}

impl Word {
    pub fn new(text: impl Into<String>, start_ms: i64, end_ms: i64, confidence: f32) -> Self {
        Self {
            text: text.into(),
            start_ms,
            end_ms,
            confidence,
            edited: false,
            locked: false,
            polished: false,
            alternates: Vec::new(),
        }
    }

    /// Per-product convention — see `docs/ARCHITECTURE.md` confidence-tier
    /// table. Tier 1 = high confidence (don't touch). Tier 4 = very low
    /// (demands attention).
    pub fn confidence_tier(&self) -> u8 {
        if self.locked || self.edited {
            return 1;
        }
        match self.confidence {
            c if c >= 85.0 => 1,
            c if c >= 70.0 => 2,
            c if c >= 50.0 => 3,
            _ => 4,
        }
    }
}

// ── Caption ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/Caption.ts")]
pub struct Caption {
    pub id: String,
    #[ts(type = "number")]
    pub start_ms: i64,
    #[ts(type = "number")]
    pub end_ms: i64,
    pub words: Vec<Word>,
    pub speaker_id: Option<String>,
    pub style_id: Option<String>,
    pub notes: Option<String>,
    pub ai_generated: bool,
    #[ts(type = "number")]
    pub last_edited_at: i64,
    /// Which caption/overlay track this caption belongs to (NLE multi-track).
    /// `#[serde(default)]` so pre-multitrack JSON deserializes.
    #[serde(default)]
    pub track_id: Option<String>,
}

impl Caption {
    /// The rendered text — derived from words on read. Kept here for
    /// convenience but never persisted as a separate field; words are
    /// the source of truth.
    pub fn text(&self) -> String {
        self.words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Number of words below the given confidence threshold (excluding
    /// locked/edited words — those are trusted).
    pub fn uncertain_word_count(&self, threshold: f32) -> usize {
        self.words
            .iter()
            .filter(|w| !w.locked && !w.edited && w.confidence < threshold)
            .count()
    }
}

// ── Speaker ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/Speaker.ts")]
pub struct Speaker {
    pub id: String,
    pub display_name: String,
    pub color_hex: Option<String>,
}

// ── GlossaryTerm ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/GlossaryTerm.ts")]
pub struct GlossaryTerm {
    pub id: String,
    /// Canonical form — what we want in the final output.
    pub term: String,
    /// Likely misrecognitions to auto-correct to `term`.
    pub aliases: Vec<String>,
    pub definition: Option<String>,
    pub pronunciation_hint: Option<String>,
}

// ── Clip ──────────────────────────────────────────────────────────────────────

/// A social-media clip carved out of the talk (Phase: SundayEdit clips).
/// `caption_ids` are the source captions the clip covers; `start_ms`/`end_ms`
/// are derived from those captions' real timings (never model-invented). The
/// `title` is the clip's main point, rendered as a large on-screen overlay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/Clip.ts")]
pub struct Clip {
    pub id: String,
    /// The main point — shown as a large title overlay on the clip.
    pub title: String,
    /// One-line summary / hook for the clip.
    pub hook: String,
    /// Source captions this clip covers.
    pub caption_ids: Vec<String>,
    #[ts(type = "number")]
    pub start_ms: i64,
    #[ts(type = "number")]
    pub end_ms: i64,
}

// ── Style ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/Style.ts")]
pub struct Style {
    pub id: String,
    pub name: String,
    pub font_family: String,
    pub font_size_px: i32,
    pub font_weight: i32,
    pub italic: bool,
    pub color_fg: String, // hex
    pub outline_color: String,
    pub outline_width_px: i32,
    pub shadow_color: String,
    pub shadow_offset_x: i32,
    pub shadow_offset_y: i32,
    pub shadow_blur: i32,
    pub background_color: Option<String>,
    pub background_padding_px: i32,
    pub background_radius_px: i32,
    pub align_h: String, // "left" | "center" | "right"
    pub align_v: String, // "top" | "middle" | "bottom"
    pub anchor: String,  // 9-grid: "tl","tc","tr","ml","mc","mr","bl","bc","br"
    pub max_width_pct: f32,
    pub line_spacing: f32,
    pub letter_spacing: f32,
    pub animation: Option<AnimationSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/AnimationSpec.ts")]
pub struct AnimationSpec {
    /// "fade" | "slide_left" | "slide_right" | "slide_up" | "slide_down" |
    /// "karaoke" | "popup" | "none"
    pub kind: String,
    pub duration_ms: i32,
    pub per_word_delay_ms: i32,
}

impl Style {
    /// "Broadcast News" — sober, accessibility-focused. SundayEdit's safe default.
    pub fn broadcast_news() -> Self {
        Self {
            id: "preset:broadcast_news".to_string(),
            name: "Broadcast News".to_string(),
            font_family: "Helvetica Neue".to_string(),
            font_size_px: 42,
            font_weight: 600,
            italic: false,
            color_fg: "#FFFFFF".to_string(),
            outline_color: "#000000".to_string(),
            outline_width_px: 3,
            shadow_color: "#00000080".to_string(),
            shadow_offset_x: 0,
            shadow_offset_y: 2,
            shadow_blur: 6,
            background_color: None,
            background_padding_px: 0,
            background_radius_px: 0,
            align_h: "center".into(),
            align_v: "bottom".into(),
            anchor: "bc".into(),
            max_width_pct: 80.0,
            line_spacing: 1.1,
            letter_spacing: 0.0,
            animation: Some(AnimationSpec {
                kind: "fade".into(),
                duration_ms: 200,
                per_word_delay_ms: 0,
            }),
        }
    }

    /// Title-overlay style for social clips — large, bold, top-centre, so the
    /// clip's main point reads at a glance above the captions.
    pub fn title_overlay() -> Self {
        Self {
            id: "preset:title_overlay".to_string(),
            name: "Title".to_string(),
            font_family: "Helvetica Neue".to_string(),
            font_size_px: 72,
            font_weight: 800,
            italic: false,
            color_fg: "#FFFFFF".to_string(),
            outline_color: "#000000".to_string(),
            outline_width_px: 4,
            shadow_color: "#000000A0".to_string(),
            shadow_offset_x: 0,
            shadow_offset_y: 3,
            shadow_blur: 10,
            background_color: None,
            background_padding_px: 0,
            background_radius_px: 0,
            align_h: "center".into(),
            align_v: "top".into(),
            anchor: "tc".into(),
            max_width_pct: 88.0,
            line_spacing: 1.1,
            letter_spacing: 0.0,
            animation: Some(AnimationSpec {
                kind: "fade".into(),
                duration_ms: 250,
                per_word_delay_ms: 0,
            }),
        }
    }
}

// ── ExportConfig ─────────────────────────────────────────────────────────────

/// Persisted export preferences for sidecar text format + burn-in style.
/// Stored per-project; sane defaults so it's always valid on first use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/ExportConfig.ts")]
pub struct ExportConfig {
    /// Default sidecar format: "srt" | "vtt" | "ass"
    pub format: String,
    /// Whether to add captions as burn-in when using a platform preset.
    pub burn_in: bool,
    /// Caption font size in px (16 / 20 / 24 / 28).
    pub caption_size_px: i32,
    /// Caption text colour: "white" | "yellow" | "green"
    pub caption_color: String,
    /// Caption background: "black" | "semitransparent" | "none"
    pub caption_background: String,
    /// Maximum characters per caption line: 32 | 42 | 52
    pub max_chars_per_line: i32,
    /// Karaoke (per-word `\k` highlighting) settings for the ASS sidecar AND
    /// every burn-in path. Persisted here — not on `Style` — because this is
    /// the container that already carries burn-in preferences, so the sidecar
    /// export, the final render and the preview proxy all read one value and
    /// cannot disagree.
    ///
    /// `Option` + `#[serde(default)]` so pre-E4a `.sundayedit` files (and the
    /// existing frontend `ExportConfig` literals) stay valid; `None` means the
    /// same thing as `KaraokeOptions::disabled()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub karaoke: Option<crate::services::karaoke::KaraokeOptions>,
}

impl Default for ExportConfig {
    fn default() -> Self {
        Self {
            format: "srt".into(),
            burn_in: false,
            caption_size_px: 24,
            caption_color: "white".into(),
            caption_background: "semitransparent".into(),
            max_chars_per_line: 42,
            karaoke: None,
        }
    }
}

// ── ProjectMeta ──────────────────────────────────────────────────────────────

/// User-editable project metadata: title, video description (used as AI
/// context), glossary names for Whisper priming, and preferred language.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/ProjectMeta.ts")]
pub struct ProjectMeta {
    /// Human-readable title (overrides the bare filename in the UI).
    pub title: String,
    /// Prose description of the video — fed to AI as context.
    pub description: String,
    /// Comma-separated list of proper nouns / glossary hints for Whisper.
    pub proper_nouns: String,
    /// Transcription/translation language: "auto" | ISO 639-1 code
    pub language: String,
}

impl Default for ProjectMeta {
    fn default() -> Self {
        Self {
            title: String::new(),
            description: String::new(),
            proper_nouns: String::new(),
            language: "auto".into(),
        }
    }
}

// ── NLE multi-track domain ────────────────────────────────────────────────────
//
// The foundation for SundayEdit's multi-track timeline. A project owns a pool of
// `MediaItem`s (imported source files), a set of `Track`s, and the `TimelineItem`s
// placed on those tracks. Geometry (`Transform`, `CropRect`) is expressed as
// fractions of the output frame so it's resolution-independent.

/// An imported source media file. The `content_hash` gives path-stable identity
/// (same as the scalar `video_content_hash`); `audio_wav_path` caches the
/// extracted PCM used for waveform + ASR.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/MediaItem.ts")]
pub struct MediaItem {
    pub id: String,
    pub path: String,
    pub content_hash: String,
    pub kind: crate::services::video::MediaKind,
    #[ts(type = "number")]
    pub duration_ms: i64,
    pub width: i32,
    pub height: i32,
    pub fps: f32,
    pub has_audio: bool,
    pub audio_wav_path: Option<String>,
    pub original_filename: String,
    #[ts(type = "number")]
    pub added_at: i64,
}

/// The kind of a track — governs which items may live on it and how it renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/lib/bindings/TrackKind.ts")]
pub enum TrackKind {
    Video,
    Audio,
    Caption,
    Overlay,
}

/// A horizontal lane on the timeline. `index` is the stacking order (0 = bottom).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/Track.ts")]
pub struct Track {
    pub id: String,
    pub kind: TrackKind,
    pub name: String,
    pub index: i32,
    pub enabled: bool,
    pub locked: bool,
    pub muted: bool,
    pub solo: bool,
}

/// A rectangular crop, as fractions of the source frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/CropRect.ts")]
pub struct CropRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Geometric transform for a timeline item, as fractions of the output frame
/// (resolution-independent). `Default` is the identity transform.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/Transform.ts")]
pub struct Transform {
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    pub rotation_deg: f32,
    pub opacity: f32,
    pub crop: Option<CropRect>,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            scale: 1.0,
            rotation_deg: 0.0,
            opacity: 1.0,
            crop: None,
        }
    }
}

/// A processing effect applied to a timeline item. `params` is an opaque JSON
/// bag keyed by effect `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/Effect.ts")]
pub struct Effect {
    pub id: String,
    pub kind: String,
    #[ts(type = "unknown")]
    pub params: serde_json::Value,
    pub enabled: bool,
}

/// A transition (e.g. crossfade) at the leading edge of a timeline item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/Transition.ts")]
pub struct Transition {
    pub kind: String,
    #[ts(type = "number")]
    pub duration_ms: i64,
}

/// What a `TimelineItem` represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/lib/bindings/TimelineItemKind.ts")]
pub enum TimelineItemKind {
    Av,
    Text,
    Graphic,
}

/// Minimal text spec for Text/Graphic overlay items.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/TextSpec.ts")]
pub struct TextSpec {
    pub text: String,
    pub style_id: Option<String>,
}

/// A single clip placed on a track. `in_ms`/`out_ms` index into the source
/// media; `timeline_start_ms` is where it sits on the timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/TimelineItem.ts")]
pub struct TimelineItem {
    pub id: String,
    pub track_id: String,
    pub kind: TimelineItemKind,
    pub source_media_id: Option<String>,
    #[ts(type = "number")]
    pub in_ms: i64,
    #[ts(type = "number")]
    pub out_ms: i64,
    #[ts(type = "number")]
    pub timeline_start_ms: i64,
    pub speed: f32,
    pub transform: Transform,
    pub effects: Vec<Effect>,
    pub transition_in: Option<Transition>,
    pub text: Option<TextSpec>,
    pub enabled: bool,
    pub locked: bool,
}

impl TimelineItem {
    /// Where this item ends on the timeline, accounting for `speed`.
    ///
    /// Computed in **f64** with truncation toward zero — the exact arithmetic
    /// of the TypeScript mirror `previewMap.timelineEndMs` (`Math.trunc` on JS
    /// numbers). f32 here diverges from the UI by 1 ms around integer
    /// quotients (e.g. 1100 ms at speed 1.1) and above 2^24 ms durations,
    /// making validate_timeline reject layouts the UI showed as legal — see
    /// tests/timeline_end_parity.rs.
    pub fn timeline_end_ms(&self) -> i64 {
        self.timeline_start_ms
            + (((self.out_ms - self.in_ms) as f64) / (self.speed as f64).max(0.01)) as i64
    }
}

// ── Project ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/Project.ts")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub video_path: String,
    pub video_content_hash: String,
    #[ts(type = "number")]
    pub video_duration_ms: i64,
    pub video_width: i32,
    pub video_height: i32,
    pub video_fps: f32,
    pub audio_wav_path: Option<String>,
    pub language: String,
    pub default_style: Style,
    pub context_description: Option<String>,
    pub captions: Vec<Caption>,
    pub speakers: Vec<Speaker>,
    pub glossary: Vec<GlossaryTerm>,
    /// AI-generated social clips carved from the talk (SundayEdit).
    #[serde(default)]
    pub clips: Vec<Clip>,
    /// Short AI summary of the whole talk (SundayEdit).
    #[serde(default)]
    pub talk_summary: Option<String>,
    /// Configurable export pipeline settings (format, burn-in, style).
    #[serde(default)]
    pub export_config: ExportConfig,
    /// Editable project metadata (title, description, proper-noun hints).
    #[serde(default)]
    pub project_meta: ProjectMeta,
    /// NLE multi-track: pool of imported source media.
    #[serde(default)]
    pub media: Vec<MediaItem>,
    /// NLE multi-track: the timeline's tracks.
    #[serde(default)]
    pub tracks: Vec<Track>,
    /// NLE multi-track: clips placed on the tracks.
    #[serde(default)]
    pub timeline_items: Vec<TimelineItem>,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

impl Project {
    /// Validate the invariants documented in `docs/ARCHITECTURE.md`:
    ///   1. Captions never overlap in time
    ///   2. Captions are sorted by start_ms
    ///   3. start < end on every caption
    ///   4. Word ranges within their caption are non-decreasing
    pub fn validate(&self) -> Result<(), String> {
        let mut last_end = i64::MIN;
        for (i, c) in self.captions.iter().enumerate() {
            if c.start_ms >= c.end_ms {
                return Err(format!("caption[{}] has start >= end", i));
            }
            if c.start_ms < last_end {
                return Err(format!(
                    "caption[{}] starts at {} but previous ended at {} — overlap",
                    i, c.start_ms, last_end
                ));
            }
            // words timing
            let mut prev_word_end = c.start_ms;
            for (wi, w) in c.words.iter().enumerate() {
                if w.start_ms < prev_word_end {
                    return Err(format!(
                        "caption[{}].word[{}] starts at {} before previous word end {}",
                        i, wi, w.start_ms, prev_word_end
                    ));
                }
                if w.end_ms <= w.start_ms {
                    return Err(format!("caption[{}].word[{}] has end <= start", i, wi));
                }
                if w.end_ms > c.end_ms {
                    return Err(format!(
                        "caption[{}].word[{}] ends at {} after caption end {}",
                        i, wi, w.end_ms, c.end_ms
                    ));
                }
                prev_word_end = w.end_ms;
            }
            last_end = c.end_ms;
        }
        Ok(())
    }

    /// Validate the multi-track timeline invariants:
    ///   1. Every `TimelineItem.track_id` resolves to a `Track`.
    ///   2. Every `Some(source_media_id)` resolves to a `MediaItem`.
    ///   3. `in_ms < out_ms`, both within `[0, media.duration_ms]`.
    ///   4. `timeline_start_ms >= 0`.
    ///   5. Per Video/Audio track, items are sorted by `timeline_start_ms` and
    ///      do not overlap (using `timeline_end_ms`). Exact adjacency is OK — a
    ///      `transition_in` crossfade is a boundary, not a geometric overlap.
    pub fn validate_timeline(&self) -> Result<(), String> {
        // 1–4: per-item checks.
        for (i, it) in self.timeline_items.iter().enumerate() {
            let track = self
                .tracks
                .iter()
                .find(|t| t.id == it.track_id)
                .ok_or_else(|| {
                    format!(
                        "timeline_item[{}] references unknown track_id {}",
                        i, it.track_id
                    )
                })?;
            let _ = track;

            if let Some(mid) = &it.source_media_id {
                let media = self.media.iter().find(|m| &m.id == mid).ok_or_else(|| {
                    format!(
                        "timeline_item[{}] references unknown source_media_id {}",
                        i, mid
                    )
                })?;
                if it.in_ms >= it.out_ms {
                    return Err(format!("timeline_item[{}] has in_ms >= out_ms", i));
                }
                if it.in_ms < 0 || it.out_ms > media.duration_ms {
                    return Err(format!(
                        "timeline_item[{}] range [{}, {}] out of media bounds [0, {}]",
                        i, it.in_ms, it.out_ms, media.duration_ms
                    ));
                }
            } else if it.in_ms >= it.out_ms {
                return Err(format!("timeline_item[{}] has in_ms >= out_ms", i));
            }

            if it.timeline_start_ms < 0 {
                return Err(format!(
                    "timeline_item[{}] has negative timeline_start_ms",
                    i
                ));
            }
        }

        // 5: non-overlap per Video/Audio track.
        for track in self
            .tracks
            .iter()
            .filter(|t| matches!(t.kind, TrackKind::Video | TrackKind::Audio))
        {
            let mut items: Vec<&TimelineItem> = self
                .timeline_items
                .iter()
                .filter(|it| it.track_id == track.id)
                .collect();
            items.sort_by_key(|it| it.timeline_start_ms);
            let mut prev_end = i64::MIN;
            for it in items {
                if it.timeline_start_ms < prev_end {
                    return Err(format!(
                        "track {} has overlapping items at {} (previous ended {})",
                        track.id, it.timeline_start_ms, prev_end
                    ));
                }
                prev_end = it.timeline_end_ms();
            }
        }

        Ok(())
    }

    /// Backfill the minimal multi-track shape for a project whose scalar
    /// `video_*` fields are populated but whose NLE arrays are empty — a
    /// freshly imported video (`project_create_from_video`) or a v<=3 project
    /// file (`project_file::load`). Synthesizes one `MediaItem` from the
    /// scalars, a Video + Caption track pair, stamps every caption with the
    /// caption track's id, and places the video as ONE full-length `Av` clip
    /// at timeline 0 — so the media bin, lanes and preview all see the
    /// imported video immediately.
    ///
    /// No-op when the project already has tracks: a v4 file (or an in-memory
    /// project that has been edited) must never be double-backfilled.
    ///
    /// `has_audio` is the caller's best knowledge of the source's audio
    /// stream: probe metadata at import time, `audio_wav_path` presence when
    /// loading an old file.
    pub fn backfill_default_timeline(&mut self, has_audio: bool) {
        if !self.tracks.is_empty() {
            return;
        }
        let new_id = || uuid::Uuid::now_v7().to_string();

        // A real video stream (has dimensions) → Video; otherwise audio-only.
        let kind = if self.video_width > 0 && self.video_height > 0 {
            crate::services::video::MediaKind::Video
        } else {
            crate::services::video::MediaKind::AudioOnly
        };
        let original_filename = std::path::Path::new(&self.video_path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.video_path.clone());
        let media_id = new_id();
        self.media = vec![MediaItem {
            id: media_id.clone(),
            path: self.video_path.clone(),
            content_hash: self.video_content_hash.clone(),
            kind,
            duration_ms: self.video_duration_ms,
            width: self.video_width,
            height: self.video_height,
            fps: self.video_fps,
            has_audio,
            audio_wav_path: self.audio_wav_path.clone(),
            original_filename,
            added_at: self.created_at,
        }];

        let video_track_id = new_id();
        let caption_track_id = new_id();
        self.tracks = vec![
            Track {
                id: video_track_id.clone(),
                kind: TrackKind::Video,
                name: "Video".into(),
                index: 0,
                enabled: true,
                locked: false,
                muted: false,
                solo: false,
            },
            Track {
                id: caption_track_id.clone(),
                kind: TrackKind::Caption,
                name: "Captions".into(),
                index: 1,
                enabled: true,
                locked: false,
                muted: false,
                solo: false,
            },
        ];
        for c in self.captions.iter_mut() {
            c.track_id = Some(caption_track_id.clone());
        }

        // Place the imported video as one full-length clip. Skip when the
        // duration is unknown/zero — an empty span would violate
        // `validate_timeline` (`in_ms < out_ms`).
        if self.video_duration_ms > 0 {
            self.timeline_items = vec![TimelineItem {
                id: new_id(),
                track_id: video_track_id,
                kind: TimelineItemKind::Av,
                source_media_id: Some(media_id),
                in_ms: 0,
                out_ms: self.video_duration_ms,
                timeline_start_ms: 0,
                speed: 1.0,
                transform: Transform::default(),
                effects: vec![],
                transition_in: None,
                text: None,
                enabled: true,
                locked: false,
            }];
        }
    }
}

#[cfg(test)]
mod timeline_tests {
    use super::*;
    use crate::services::video::MediaKind;

    fn media(id: &str, dur: i64) -> MediaItem {
        MediaItem {
            id: id.into(),
            path: format!("/v/{}.mp4", id),
            content_hash: "h".into(),
            kind: MediaKind::Video,
            duration_ms: dur,
            width: 1920,
            height: 1080,
            fps: 30.0,
            has_audio: true,
            audio_wav_path: None,
            original_filename: format!("{}.mp4", id),
            added_at: 0,
        }
    }

    fn track(id: &str, kind: TrackKind, index: i32) -> Track {
        Track {
            id: id.into(),
            kind,
            name: id.into(),
            index,
            enabled: true,
            locked: false,
            muted: false,
            solo: false,
        }
    }

    fn item(
        id: &str,
        track_id: &str,
        media_id: Option<&str>,
        start: i64,
        in_ms: i64,
        out_ms: i64,
    ) -> TimelineItem {
        TimelineItem {
            id: id.into(),
            track_id: track_id.into(),
            kind: TimelineItemKind::Av,
            source_media_id: media_id.map(|s| s.to_string()),
            in_ms,
            out_ms,
            timeline_start_ms: start,
            speed: 1.0,
            transform: Transform::default(),
            effects: vec![],
            transition_in: None,
            text: None,
            enabled: true,
            locked: false,
        }
    }

    fn base() -> Project {
        Project {
            id: "p".into(),
            name: "n".into(),
            video_path: "/v.mp4".into(),
            video_content_hash: "h".into(),
            video_duration_ms: 10_000,
            video_width: 1920,
            video_height: 1080,
            video_fps: 30.0,
            audio_wav_path: None,
            language: "no".into(),
            default_style: Style::broadcast_news(),
            context_description: None,
            captions: vec![],
            speakers: vec![],
            glossary: vec![],
            clips: vec![],
            talk_summary: None,
            export_config: ExportConfig::default(),
            project_meta: ProjectMeta::default(),
            media: vec![media("m1", 5000)],
            tracks: vec![track("t1", TrackKind::Video, 0)],
            timeline_items: vec![],
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn valid_timeline_passes() {
        let mut p = base();
        p.timeline_items = vec![
            item("i1", "t1", Some("m1"), 0, 0, 2000),
            item("i2", "t1", Some("m1"), 2000, 0, 1000), // exact adjacency OK
        ];
        assert!(p.validate_timeline().is_ok());
    }

    #[test]
    fn transform_default_is_identity() {
        let t = Transform::default();
        assert_eq!(t.scale, 1.0);
        assert_eq!(t.opacity, 1.0);
        assert_eq!(t.x, 0.0);
        assert_eq!(t.crop, None);
    }

    #[test]
    fn timeline_end_ms_accounts_for_speed() {
        let mut it = item("i", "t1", Some("m1"), 1000, 0, 2000);
        assert_eq!(it.timeline_end_ms(), 3000);
        it.speed = 2.0;
        assert_eq!(it.timeline_end_ms(), 2000);
    }

    #[test]
    fn unknown_track_fails() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "nope", Some("m1"), 0, 0, 1000)];
        assert!(p.validate_timeline().is_err());
    }

    #[test]
    fn unknown_media_fails() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "t1", Some("nope"), 0, 0, 1000)];
        assert!(p.validate_timeline().is_err());
    }

    #[test]
    fn in_after_out_fails() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "t1", Some("m1"), 0, 1000, 1000)];
        assert!(p.validate_timeline().is_err());
    }

    #[test]
    fn out_beyond_media_duration_fails() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "t1", Some("m1"), 0, 0, 6000)]; // media is 5000
        assert!(p.validate_timeline().is_err());
    }

    #[test]
    fn negative_timeline_start_fails() {
        let mut p = base();
        p.timeline_items = vec![item("i1", "t1", Some("m1"), -1, 0, 1000)];
        assert!(p.validate_timeline().is_err());
    }

    #[test]
    fn overlapping_items_fail() {
        let mut p = base();
        p.timeline_items = vec![
            item("i1", "t1", Some("m1"), 0, 0, 2000),
            item("i2", "t1", Some("m1"), 1000, 0, 1000), // overlaps i1 (ends 2000)
        ];
        assert!(p.validate_timeline().is_err());
    }

    // ── backfill_default_timeline ───────────────────────────────────────────

    fn caption(id: &str, start: i64, end: i64) -> Caption {
        Caption {
            id: id.into(),
            start_ms: start,
            end_ms: end,
            words: vec![Word::new("ord", start, end, 90.0)],
            speaker_id: None,
            style_id: None,
            notes: None,
            ai_generated: true,
            last_edited_at: 0,
            track_id: None,
        }
    }

    /// The scalar-only shape both callers hand the helper: populated `video_*`
    /// fields, empty NLE arrays.
    fn scalar_only() -> Project {
        let mut p = base();
        p.media = vec![];
        p.tracks = vec![];
        p.timeline_items = vec![];
        p.created_at = 42;
        p
    }

    #[test]
    fn backfill_synthesizes_media_tracks_and_one_placed_item() {
        let mut p = scalar_only();
        p.backfill_default_timeline(true);

        // One media item mirroring the video_* scalars.
        assert_eq!(p.media.len(), 1);
        let m = &p.media[0];
        assert_eq!(m.path, "/v.mp4");
        assert_eq!(m.content_hash, "h");
        assert_eq!(m.duration_ms, 10_000);
        assert_eq!((m.width, m.height), (1920, 1080));
        assert_eq!(m.original_filename, "v.mp4");
        assert!(m.has_audio);
        assert_eq!(m.kind, MediaKind::Video);
        assert_eq!(m.added_at, 42);

        // A Video + Caption track pair.
        assert_eq!(p.tracks.len(), 2);
        assert_eq!(p.tracks[0].kind, TrackKind::Video);
        assert_eq!(p.tracks[0].index, 0);
        assert_eq!(p.tracks[1].kind, TrackKind::Caption);
        assert_eq!(p.tracks[1].index, 1);

        // ONE full-length Av clip placed at timeline 0.
        assert_eq!(p.timeline_items.len(), 1);
        let it = &p.timeline_items[0];
        assert_eq!(it.kind, TimelineItemKind::Av);
        assert_eq!(it.track_id, p.tracks[0].id);
        assert_eq!(it.source_media_id.as_deref(), Some(m.id.as_str()));
        assert_eq!((it.in_ms, it.out_ms, it.timeline_start_ms), (0, 10_000, 0));
        assert_eq!(it.speed, 1.0);
        assert_eq!(it.transform, Transform::default());
        assert!(it.effects.is_empty());
        assert!(it.transition_in.is_none());
        assert!(it.enabled && !it.locked);

        // The result satisfies the timeline invariants.
        assert!(p.validate_timeline().is_ok());
    }

    #[test]
    fn backfill_stamps_caption_track_ids() {
        let mut p = scalar_only();
        p.captions = vec![caption("c1", 0, 1000), caption("c2", 1500, 2000)];
        p.backfill_default_timeline(false);
        let cap_track = &p.tracks[1];
        for c in &p.captions {
            assert_eq!(c.track_id.as_ref(), Some(&cap_track.id));
        }
    }

    #[test]
    fn backfill_is_a_noop_when_tracks_exist() {
        let mut p = base(); // already has a track
        let before = p.clone();
        p.backfill_default_timeline(true);
        assert_eq!(p, before, "a v4-shaped project must never be re-backfilled");
    }

    #[test]
    fn backfill_marks_audio_only_media_and_respects_has_audio() {
        let mut p = scalar_only();
        p.video_width = 0;
        p.video_height = 0;
        p.backfill_default_timeline(false);
        assert_eq!(p.media[0].kind, MediaKind::AudioOnly);
        assert!(!p.media[0].has_audio);
    }

    #[test]
    fn backfill_places_no_item_when_duration_is_unknown() {
        let mut p = scalar_only();
        p.video_duration_ms = 0;
        p.backfill_default_timeline(true);
        assert_eq!(p.tracks.len(), 2, "tracks are still synthesized");
        assert!(
            p.timeline_items.is_empty(),
            "a zero-length clip would violate validate_timeline"
        );
        assert!(p.validate_timeline().is_ok());
    }
}
