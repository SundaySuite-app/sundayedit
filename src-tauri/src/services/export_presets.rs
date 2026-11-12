//! Platform export presets — Phase 6.3.
//!
//! Quick "export for the platform I publish to" choices. Each preset
//! bundles a target resolution/aspect, duration limit, codec, and the
//! validation rules that platform enforces, so the user gets a warning
//! BEFORE a long render rather than a rejected upload after.
//!
//! Pure data + a `validate(project, preset)` function — fully testable.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::{Clip, Project};
use crate::services::burnin::{BurnInOptions, VideoCodec, default_encoder};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export, export_to = "../../src/lib/bindings/Aspect.ts")]
pub enum Aspect {
    /// 16:9 horizontal.
    Landscape,
    /// 9:16 vertical (Shorts/Reels/TikTok).
    Portrait,
    /// 1:1 square.
    Square,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/ExportPreset.ts")]
pub struct ExportPreset {
    pub id: String,
    pub name: String,
    pub description: String,
    pub aspect: Aspect,
    pub width: i32,
    pub height: i32,
    /// Platform's hard max video length in seconds (`None` = no limit).
    pub max_duration_sec: Option<i64>,
    pub codec: VideoCodec,
    /// Recommended video bitrate (kbps).
    pub bitrate_kbps: i32,
    /// Whether we also write an SRT sidecar (good for SEO/accessibility).
    pub also_srt_sidecar: bool,
}

impl ExportPreset {
    /// Turn a preset into the burn-in options that produce it.
    pub fn to_burnin_options(&self) -> BurnInOptions {
        BurnInOptions {
            codec: self.codec,
            encoder: default_encoder(),
            out_width: Some(self.width),
            out_height: Some(self.height),
            bitrate_kbps: Some(self.bitrate_kbps),
            clip_start_ms: None,
            clip_end_ms: None,
        }
    }

    /// Burn-in options that render just `clip` at this preset's vertical
    /// dimensions — the preset's output settings plus the clip's time range.
    pub fn to_clip_burnin_options(&self, clip: &Clip) -> BurnInOptions {
        BurnInOptions {
            clip_start_ms: Some(clip.start_ms),
            clip_end_ms: Some(clip.end_ms),
            ..self.to_burnin_options()
        }
    }
}

/// A validation finding shown before rendering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/ExportWarning.ts")]
pub struct ExportWarning {
    /// `error` blocks export; `warning` is advisory.
    pub severity: String,
    pub message: String,
}

/// Validate a project against a preset's platform rules.
pub fn validate(project: &Project, preset: &ExportPreset) -> Vec<ExportWarning> {
    let mut out = Vec::new();

    // Duration limit
    if let Some(max) = preset.max_duration_sec {
        // Compare in milliseconds: floor-dividing to whole seconds would let a
        // video up to ~999 ms over the cap slip through (e.g. 60_999 ms → 60 s).
        if project.video_duration_ms > max * 1000 {
            // Round up for the message so a 60.999 s clip reads as 61 s, not 60.
            let dur_sec = (project.video_duration_ms + 999) / 1000;
            out.push(ExportWarning {
                severity: "error".into(),
                message: format!(
                    "Videoen er {} s, men {} tillater maks {} s.",
                    dur_sec, preset.name, max
                ),
            });
        }
    }

    // Aspect mismatch between source and target → we'll crop, warn the user.
    if project.video_width > 0 && project.video_height > 0 {
        let src_landscape = project.video_width >= project.video_height;
        let target_landscape = matches!(preset.aspect, Aspect::Landscape);
        if src_landscape != target_landscape && !matches!(preset.aspect, Aspect::Square) {
            out.push(ExportWarning {
                severity: "warning".into(),
                message: format!(
                    "Kilden er {}, men {} er {}. Bildet blir beskåret — sjekk at teksten er innenfor.",
                    if src_landscape { "liggende" } else { "stående" },
                    preset.name,
                    if target_landscape { "liggende" } else { "stående" },
                ),
            });
        }
    }

    // No captions → probably a mistake.
    if project.captions.is_empty() {
        out.push(ExportWarning {
            severity: "warning".into(),
            message: "Prosjektet har ingen undertekster enda.".into(),
        });
    }

    out
}

pub fn catalog() -> Vec<ExportPreset> {
    use VideoCodec::H264;
    vec![
        ExportPreset {
            id: "export:youtube".into(),
            name: "YouTube".into(),
            description: "1080p liggende. SRT-sidecar for SEO + tilgjengelighet.".into(),
            aspect: Aspect::Landscape,
            width: 1920,
            height: 1080,
            max_duration_sec: None,
            codec: H264,
            bitrate_kbps: 12_000,
            also_srt_sidecar: true,
        },
        ExportPreset {
            id: "export:youtube_shorts".into(),
            name: "YouTube Shorts".into(),
            description: "1080×1920 stående, maks 60 s.".into(),
            aspect: Aspect::Portrait,
            width: 1080,
            height: 1920,
            max_duration_sec: Some(60),
            codec: H264,
            bitrate_kbps: 10_000,
            also_srt_sidecar: false,
        },
        ExportPreset {
            id: "export:reels".into(),
            name: "Instagram Reels".into(),
            description: "1080×1920 stående, maks 90 s.".into(),
            aspect: Aspect::Portrait,
            width: 1080,
            height: 1920,
            max_duration_sec: Some(90),
            codec: H264,
            bitrate_kbps: 10_000,
            also_srt_sidecar: false,
        },
        ExportPreset {
            id: "export:tiktok".into(),
            name: "TikTok".into(),
            description: "1080×1920 stående, maks 10 min.".into(),
            aspect: Aspect::Portrait,
            width: 1080,
            height: 1920,
            max_duration_sec: Some(600),
            codec: H264,
            bitrate_kbps: 10_000,
            also_srt_sidecar: false,
        },
        ExportPreset {
            id: "export:x".into(),
            name: "Twitter / X".into(),
            description: "720p liggende, maks 140 s.".into(),
            aspect: Aspect::Landscape,
            width: 1280,
            height: 720,
            max_duration_sec: Some(140),
            codec: H264,
            bitrate_kbps: 6_000,
            also_srt_sidecar: false,
        },
        ExportPreset {
            id: "export:square".into(),
            name: "Square (feed)".into(),
            description: "1080×1080 kvadratisk for feed-poster.".into(),
            aspect: Aspect::Square,
            width: 1080,
            height: 1080,
            max_duration_sec: None,
            codec: H264,
            bitrate_kbps: 8_000,
            also_srt_sidecar: false,
        },
        ExportPreset {
            id: "export:broadcast".into(),
            name: "Broadcast".into(),
            description: "1080p liggende, høy bitrate. SRT-sidecar.".into(),
            aspect: Aspect::Landscape,
            width: 1920,
            height: 1080,
            max_duration_sec: None,
            codec: H264,
            bitrate_kbps: 20_000,
            also_srt_sidecar: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Caption, Clip, Project, Style, Word};

    fn project(width: i32, height: i32, dur_ms: i64, captions: usize) -> Project {
        let caps = (0..captions)
            .map(|i| Caption {
                id: format!("c{i}"),
                start_ms: i as i64 * 1000,
                end_ms: i as i64 * 1000 + 900,
                words: vec![Word::new("hi", 0, 900, 95.0)],
                speaker_id: None,
                style_id: None,
                notes: None,
                ai_generated: true,
                last_edited_at: 0,
                track_id: None,
            })
            .collect();
        Project {
            id: "p".into(),
            name: "t".into(),
            video_path: "/x".into(),
            video_content_hash: "h".into(),
            video_duration_ms: dur_ms,
            video_width: width,
            video_height: height,
            video_fps: 30.0,
            audio_wav_path: None,
            language: "en".into(),
            default_style: Style::broadcast_news(),
            context_description: None,
            captions: caps,
            speakers: vec![],
            glossary: vec![],
            clips: vec![],
            talk_summary: None,
            export_config: crate::model::ExportConfig::default(),
            project_meta: crate::model::ProjectMeta::default(),
            created_at: 0,
            updated_at: 0,
            media: vec![],
            tracks: vec![],
            timeline_items: vec![],
        }
    }

    fn preset(id: &str) -> ExportPreset {
        catalog().into_iter().find(|p| p.id == id).unwrap()
    }

    #[test]
    fn clip_burnin_options_carry_range_and_vertical_dims() {
        let p = preset("export:youtube_shorts"); // 1080x1920 vertical
        let clip = Clip {
            id: "clip:0".into(),
            title: "T".into(),
            hook: "".into(),
            caption_ids: vec!["c1".into()],
            start_ms: 8000,
            end_ms: 23000,
        };
        let opts = p.to_clip_burnin_options(&clip);
        assert_eq!(opts.clip_start_ms, Some(8000));
        assert_eq!(opts.clip_end_ms, Some(23000));
        assert_eq!(opts.out_width, Some(1080));
        assert_eq!(opts.out_height, Some(1920));
    }

    #[test]
    fn catalog_has_presets_with_distinct_ids() {
        let c = catalog();
        assert!(c.len() >= 6);
        let ids: std::collections::HashSet<_> = c.iter().map(|p| p.id.clone()).collect();
        assert_eq!(ids.len(), c.len());
    }

    #[test]
    fn shorts_rejects_over_60s() {
        let p = project(1080, 1920, 75_000, 3); // 75s
        let w = validate(&p, &preset("export:youtube_shorts"));
        assert!(
            w.iter()
                .any(|x| x.severity == "error" && x.message.contains("60"))
        );
    }

    #[test]
    fn shorts_rejects_just_over_60s() {
        // 60_999 ms is 60.999 s — over the 60 s cap. Floor division to 60 s must
        // not let this slip past validation.
        let p = project(1080, 1920, 60_999, 3);
        let w = validate(&p, &preset("export:youtube_shorts"));
        assert!(
            w.iter().any(|x| x.severity == "error"),
            "a 60.999s video must be flagged as over the 60s Shorts limit"
        );
    }

    #[test]
    fn shorts_accepts_under_60s() {
        let p = project(1080, 1920, 45_000, 3);
        let w = validate(&p, &preset("export:youtube_shorts"));
        assert!(!w.iter().any(|x| x.severity == "error"));
    }

    #[test]
    fn landscape_source_to_portrait_target_warns_crop() {
        let p = project(1920, 1080, 30_000, 3); // landscape source
        let w = validate(&p, &preset("export:tiktok")); // portrait target
        assert!(
            w.iter()
                .any(|x| x.severity == "warning" && x.message.contains("beskåret"))
        );
    }

    #[test]
    fn matching_aspect_no_crop_warning() {
        let p = project(1920, 1080, 30_000, 3);
        let w = validate(&p, &preset("export:youtube")); // landscape → landscape
        assert!(!w.iter().any(|x| x.message.contains("beskåret")));
    }

    #[test]
    fn empty_captions_warns() {
        let p = project(1920, 1080, 30_000, 0);
        let w = validate(&p, &preset("export:youtube"));
        assert!(w.iter().any(|x| x.message.contains("ingen undertekster")));
    }

    #[test]
    fn youtube_no_duration_limit() {
        let p = project(1920, 1080, 3_600_000, 3); // 1 hour
        let w = validate(&p, &preset("export:youtube"));
        assert!(!w.iter().any(|x| x.severity == "error"));
    }

    #[test]
    fn preset_to_burnin_options_sets_dims_and_bitrate() {
        let pr = preset("export:reels");
        let o = pr.to_burnin_options();
        assert_eq!(o.out_width, Some(1080));
        assert_eq!(o.out_height, Some(1920));
        assert_eq!(o.bitrate_kbps, Some(10_000));
    }

    #[test]
    fn youtube_and_broadcast_request_srt_sidecar() {
        assert!(preset("export:youtube").also_srt_sidecar);
        assert!(preset("export:broadcast").also_srt_sidecar);
        assert!(!preset("export:tiktok").also_srt_sidecar);
    }
}
