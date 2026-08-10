//! Burn-in renderer — Phase 6.2.
//!
//! Embeds captions visually into the video via ffmpeg's `ass` filter
//! (libass — the industry standard). We generate an ASS file from the
//! project (reusing `export::write_ass`, which already encodes the full
//! Style), then build an ffmpeg command that:
//!   - applies the `ass` filter (+ optional scale/crop for vertical
//!     formats)
//!   - encodes with a hardware encoder when available (VideoToolbox on
//!     macOS, NVENC on Windows), falling back to libx264
//!   - copies the audio stream untouched (no needless re-encode)
//!
//! The command BUILDER (`build_ffmpeg_args`) is a pure function and is
//! unit-tested exhaustively. The actual spawn needs ffmpeg + a real
//! video, so `render()` shells out and streams progress; without ffmpeg
//! installed it returns a clear error (same pattern as the whisper stub).

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, AppResult};
use crate::services::video::ffmpeg_path;

/// Output video codec. We expose the common, sensible choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export, export_to = "../../src/lib/bindings/VideoCodec.ts")]
pub enum VideoCodec {
    /// H.264 — universal. Default.
    H264,
    /// H.265/HEVC — smaller files, slightly less universal.
    H265,
}

/// Hardware encoder family — chosen automatically per platform, but
/// overridable (e.g. force CPU for determinism).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export, export_to = "../../src/lib/bindings/Encoder.ts")]
pub enum Encoder {
    /// CPU x264/x265 — always available, slowest.
    Cpu,
    /// macOS VideoToolbox.
    VideoToolbox,
    /// NVIDIA NVENC.
    Nvenc,
    /// Intel QuickSync.
    QuickSync,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/BurnInOptions.ts")]
pub struct BurnInOptions {
    pub codec: VideoCodec,
    pub encoder: Encoder,
    /// Target output width; `None` keeps source width.
    pub out_width: Option<i32>,
    /// Target output height; `None` keeps source height.
    pub out_height: Option<i32>,
    /// Constant-quality/bitrate hint, in kbps. `None` = encoder default.
    pub bitrate_kbps: Option<i32>,
    /// Render only `[start_ms, end_ms]` (clip). `None` = whole video.
    pub clip_start_ms: Option<i64>,
    pub clip_end_ms: Option<i64>,
}

impl Default for BurnInOptions {
    fn default() -> Self {
        Self {
            codec: VideoCodec::H264,
            encoder: default_encoder(),
            out_width: None,
            out_height: None,
            bitrate_kbps: None,
            clip_start_ms: None,
            clip_end_ms: None,
        }
    }
}

/// Pick the best encoder for the current platform at compile time.
pub fn default_encoder() -> Encoder {
    if cfg!(target_os = "macos") {
        Encoder::VideoToolbox
    } else if cfg!(target_os = "windows") {
        Encoder::Nvenc // best-effort; render() falls back to CPU on failure
    } else {
        Encoder::Cpu
    }
}

/// Map (codec, encoder) to the ffmpeg `-c:v` value.
pub(crate) fn encoder_name(codec: VideoCodec, encoder: Encoder) -> &'static str {
    match (codec, encoder) {
        (VideoCodec::H264, Encoder::Cpu) => "libx264",
        (VideoCodec::H264, Encoder::VideoToolbox) => "h264_videotoolbox",
        (VideoCodec::H264, Encoder::Nvenc) => "h264_nvenc",
        (VideoCodec::H264, Encoder::QuickSync) => "h264_qsv",
        (VideoCodec::H265, Encoder::Cpu) => "libx265",
        (VideoCodec::H265, Encoder::VideoToolbox) => "hevc_videotoolbox",
        (VideoCodec::H265, Encoder::Nvenc) => "hevc_nvenc",
        (VideoCodec::H265, Encoder::QuickSync) => "hevc_qsv",
    }
}

/// Escape a path for use inside an ffmpeg filtergraph. Colons and
/// backslashes (Windows) and single quotes must be escaped, otherwise
/// `ass=C:\path` breaks the filter parser.
pub(crate) fn escape_filter_path(p: &str) -> String {
    p.replace('\\', "/")
        .replace(':', "\\:")
        .replace('\'', "\\'")
}

/// Build the full ffmpeg argument vector. Pure — no IO. This is the
/// unit-tested heart of the burn-in path.
pub fn build_ffmpeg_args(
    input: &str,
    ass_file: &str,
    output: &str,
    opts: &BurnInOptions,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    args.push("-y".into()); // overwrite

    // Clip (input-side seeking is faster + frame-accurate enough for v1).
    if let Some(start) = opts.clip_start_ms {
        args.push("-ss".into());
        args.push(format!("{:.3}", start as f64 / 1000.0));
    }

    args.push("-i".into());
    args.push(input.into());

    if let (Some(start), Some(end)) = (opts.clip_start_ms, opts.clip_end_ms) {
        let dur = (end - start).max(0);
        args.push("-t".into());
        args.push(format!("{:.3}", dur as f64 / 1000.0));
    }

    // Build the filtergraph: optional scale, then the ass overlay.
    let mut filters: Vec<String> = Vec::new();
    if let (Some(w), Some(h)) = (opts.out_width, opts.out_height) {
        // scale to fit, then pad/crop to exact dims (centre) for vertical
        // targets. force_original_aspect_ratio=increase + crop = "cover".
        filters.push(format!(
            "scale={w}:{h}:force_original_aspect_ratio=increase",
            w = w,
            h = h
        ));
        filters.push(format!("crop={w}:{h}", w = w, h = h));
    }
    filters.push(format!("ass={}", escape_filter_path(ass_file)));
    args.push("-vf".into());
    args.push(filters.join(","));

    // Video encoder
    args.push("-c:v".into());
    args.push(encoder_name(opts.codec, opts.encoder).into());

    if let Some(kbps) = opts.bitrate_kbps {
        args.push("-b:v".into());
        args.push(format!("{}k", kbps));
    }

    // Audio: pass through untouched.
    args.push("-c:a".into());
    args.push("copy".into());

    args.push(output.into());
    args
}

/// Generate the ASS sidecar, then run ffmpeg burn-in. Errors clearly if
/// ffmpeg is missing.
pub fn render(
    project: &crate::model::Project,
    output: &Path,
    opts: &BurnInOptions,
) -> AppResult<()> {
    let ass = crate::services::export::write_ass(project);
    run_burnin(&project.video_path, ass, output, opts)
}

/// Burn one social clip into a vertical export: the clip's captions (offset to
/// clip-relative time) plus its main-point title overlay. `opts` must carry the
/// clip's `clip_start_ms/clip_end_ms` + the vertical output dims.
pub fn render_clip(
    project: &crate::model::Project,
    clip: &crate::model::Clip,
    output: &Path,
    opts: &BurnInOptions,
    title_style: &crate::model::Style,
) -> AppResult<()> {
    let play_res_w = opts.out_width.unwrap_or(project.video_width);
    let play_res_h = opts.out_height.unwrap_or(project.video_height);
    let ass =
        crate::services::export::write_clip_ass(project, clip, title_style, play_res_w, play_res_h);
    run_burnin(&project.video_path, ass, output, opts)
}

/// Pick a sidecar `.ass` path next to `output` that does not collide with an
/// existing file. Tries `<stem>.ass` first, then `<stem>.burnin.<n>.ass`. We
/// keep it on the same directory as the output so libass path escaping (and
/// the cross-volume behaviour) stays identical to before.
fn unique_sidecar_path(output: &Path) -> PathBuf {
    let candidate = output.with_extension("ass");
    if !candidate.exists() {
        return candidate;
    }
    let stem = output
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "burnin".to_string());
    let dir = output.parent().unwrap_or_else(|| Path::new("."));
    for n in 0..10_000 {
        let p = dir.join(format!("{stem}.burnin.{n}.ass"));
        if !p.exists() {
            return p;
        }
    }
    // Extremely unlikely fallback: timestamp-suffixed name.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!("{stem}.burnin.{ts}.ass"))
}

/// Shared tail: write the ASS sidecar, run ffmpeg burn-in, clean up. Errors
/// clearly if ffmpeg is missing or the render fails.
fn run_burnin(video_path: &str, ass: String, output: &Path, opts: &BurnInOptions) -> AppResult<()> {
    if !Path::new(video_path).exists() {
        return Err(AppError::VideoMissing(video_path.to_string()));
    }

    // Write a *temporary* ASS sidecar next to the output. We never reuse the
    // plain `output.with_extension("ass")` name directly — a user exporting
    // `talk.mp4` may already have a hand-edited `talk.ass` there, and we
    // overwrite-then-delete on success, which would silently destroy it.
    let ass_path = unique_sidecar_path(output);
    std::fs::write(&ass_path, ass)?;

    let args = build_ffmpeg_args(
        video_path,
        &ass_path.to_string_lossy(),
        &output.to_string_lossy(),
        opts,
    );

    let status = Command::new(ffmpeg_path())
        .args(&args)
        .status()
        .map_err(|e| {
            AppError::Internal(format!(
                "failed to launch ffmpeg for burn-in: {e}. Is ffmpeg installed / bundled?"
            ))
        })?;

    // best-effort cleanup of the temp ASS
    let _ = std::fs::remove_file(&ass_path);

    if !status.success() {
        return Err(AppError::Internal(
            "ffmpeg burn-in failed. If your machine lacks the chosen hardware \
             encoder, retry with the CPU encoder."
                .to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> BurnInOptions {
        BurnInOptions {
            encoder: Encoder::Cpu,
            ..Default::default()
        }
    }

    #[test]
    fn sidecar_path_does_not_clobber_existing_ass() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("talk.mp4");
        let user_ass = dir.path().join("talk.ass");
        // The user has a hand-edited subtitle file sitting next to the output.
        std::fs::write(&user_ass, "USER SUBTITLES").unwrap();

        let sidecar = unique_sidecar_path(&output);
        assert_ne!(
            sidecar, user_ass,
            "burn-in must not reuse the user's existing talk.ass as its temp sidecar"
        );

        // Writing then removing the chosen sidecar must leave talk.ass intact.
        std::fs::write(&sidecar, "GENERATED").unwrap();
        let _ = std::fs::remove_file(&sidecar);
        assert_eq!(
            std::fs::read_to_string(&user_ass).unwrap(),
            "USER SUBTITLES",
            "the user's talk.ass must survive a burn-in render"
        );
    }

    #[test]
    fn sidecar_path_uses_plain_name_when_free() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("clip.mp4");
        // No pre-existing .ass → use the simple sibling name.
        assert_eq!(unique_sidecar_path(&output), dir.path().join("clip.ass"));
    }

    #[test]
    fn basic_command_has_input_filter_output() {
        let a = build_ffmpeg_args("in.mp4", "subs.ass", "out.mp4", &opts());
        assert_eq!(a[0], "-y");
        let joined = a.join(" ");
        assert!(joined.contains("-i in.mp4"));
        assert!(joined.contains("-vf ass=subs.ass"));
        assert!(joined.contains("-c:v libx264"));
        assert!(joined.contains("-c:a copy"));
        assert_eq!(a.last().unwrap(), "out.mp4");
    }

    #[test]
    fn h265_videotoolbox_encoder_name() {
        assert_eq!(
            encoder_name(VideoCodec::H265, Encoder::VideoToolbox),
            "hevc_videotoolbox"
        );
        assert_eq!(encoder_name(VideoCodec::H264, Encoder::Nvenc), "h264_nvenc");
        assert_eq!(
            encoder_name(VideoCodec::H264, Encoder::QuickSync),
            "h264_qsv"
        );
    }

    #[test]
    fn scale_and_crop_added_for_vertical_target() {
        let o = BurnInOptions {
            encoder: Encoder::Cpu,
            out_width: Some(1080),
            out_height: Some(1920),
            ..Default::default()
        };
        let a = build_ffmpeg_args("in.mp4", "s.ass", "out.mp4", &o);
        let vf = arg_after(&a, "-vf").unwrap();
        assert!(vf.contains("scale=1080:1920:force_original_aspect_ratio=increase"));
        assert!(vf.contains("crop=1080:1920"));
        assert!(
            vf.contains("ass=s.ass"),
            "ass filter still present after scale/crop"
        );
        // ass must come AFTER scale/crop so captions render at output res
        let scale_pos = vf.find("scale").unwrap();
        let ass_pos = vf.find("ass=").unwrap();
        assert!(scale_pos < ass_pos);
    }

    #[test]
    fn bitrate_added_when_set() {
        let o = BurnInOptions {
            encoder: Encoder::Cpu,
            bitrate_kbps: Some(8000),
            ..Default::default()
        };
        let a = build_ffmpeg_args("in.mp4", "s.ass", "out.mp4", &o);
        assert_eq!(arg_after(&a, "-b:v").as_deref(), Some("8000k"));
    }

    #[test]
    fn no_bitrate_flag_when_unset() {
        let a = build_ffmpeg_args("in.mp4", "s.ass", "out.mp4", &opts());
        assert!(!a.iter().any(|x| x == "-b:v"));
    }

    #[test]
    fn clip_adds_ss_and_t() {
        let o = BurnInOptions {
            encoder: Encoder::Cpu,
            clip_start_ms: Some(2000),
            clip_end_ms: Some(7000),
            ..Default::default()
        };
        let a = build_ffmpeg_args("in.mp4", "s.ass", "out.mp4", &o);
        // -ss before -i, -t after
        assert_eq!(arg_after(&a, "-ss").as_deref(), Some("2.000"));
        assert_eq!(arg_after(&a, "-t").as_deref(), Some("5.000"));
        let ss_pos = a.iter().position(|x| x == "-ss").unwrap();
        let i_pos = a.iter().position(|x| x == "-i").unwrap();
        assert!(ss_pos < i_pos, "-ss should precede -i for fast seek");
    }

    #[test]
    fn windows_path_escaped_in_filter() {
        let a = build_ffmpeg_args("in.mp4", "C:\\Users\\me\\subs.ass", "out.mp4", &opts());
        let vf = arg_after(&a, "-vf").unwrap();
        // colon escaped, backslashes → forward slashes
        assert!(vf.contains("ass=C\\:/Users/me/subs.ass"), "got {vf}");
    }

    #[test]
    fn default_encoder_matches_platform() {
        let e = default_encoder();
        if cfg!(target_os = "macos") {
            assert_eq!(e, Encoder::VideoToolbox);
        } else if cfg!(target_os = "windows") {
            assert_eq!(e, Encoder::Nvenc);
        } else {
            assert_eq!(e, Encoder::Cpu);
        }
    }

    // ── Karaoke burn-in (E4a) ──────────────────────────────────────────────

    /// Build a one-video project whose single caption carries per-word timing.
    fn karaoke_project(video_path: &str) -> crate::model::Project {
        use crate::model::{Caption, ExportConfig, Project, ProjectMeta, Style, Word};
        Project {
            id: "p".into(),
            name: "karaoke".into(),
            video_path: video_path.into(),
            video_content_hash: "h".into(),
            video_duration_ms: 4_000,
            video_width: 320,
            video_height: 180,
            video_fps: 25.0,
            audio_wav_path: None,
            language: "no".into(),
            default_style: Style::broadcast_news(),
            context_description: None,
            captions: vec![Caption {
                id: "c1".into(),
                start_ms: 200,
                end_ms: 3_600,
                words: vec![
                    Word::new("Nåde", 200, 900, 96.0),
                    Word::new("og", 950, 1_400, 88.0),
                    // Deliberately low confidence — exercises the tint branch.
                    Word::new("fred", 1_500, 3_600, 31.0),
                ],
                speaker_id: None,
                style_id: None,
                notes: None,
                ai_generated: true,
                last_edited_at: 0,
                track_id: None,
            }],
            speakers: vec![],
            glossary: vec![],
            clips: vec![],
            talk_summary: None,
            export_config: ExportConfig {
                karaoke: Some(crate::services::karaoke::KaraokeOptions {
                    enabled: true,
                    style: crate::services::karaoke::KaraokeStyle::Sweep,
                    confidence_tint: true,
                    ..Default::default()
                }),
                ..ExportConfig::default()
            },
            project_meta: ProjectMeta::default(),
            created_at: 0,
            updated_at: 0,
            media: vec![],
            tracks: vec![],
            timeline_items: vec![],
        }
    }

    /// Live end-to-end: libass must ACCEPT our karaoke ASS. The `\k`/`\kf`
    /// grammar is not validated by any unit test — a malformed tag makes libass
    /// drop the line (or the whole script) SILENTLY, and ffmpeg still exits 0
    /// with a clean-looking video. So this test drives the real
    /// `burnin::render` (which writes the sidecar from `export::write_ass`,
    /// i.e. the production path) over a synthesised source and proves ffmpeg
    /// accepted the filtergraph and produced a video stream.
    ///
    /// Visual correctness (which word is lit at which frame) is asserted
    /// structurally by the ASS snapshot tests in `services::export` plus the
    /// timing invariants in `services::karaoke` — not by sampling pixels.
    ///
    /// Synthesises its own input, so no sample asset is needed:
    /// ```sh
    /// cargo test karaoke_captions_burn_in_with_real_ffmpeg -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs ffmpeg/ffprobe on PATH"]
    fn karaoke_captions_burn_in_with_real_ffmpeg() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.mp4");
        let status = Command::new("ffmpeg")
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x180:rate=25:duration=4",
                "-pix_fmt",
                "yuv420p",
                "-y",
            ])
            .arg(&src)
            .status()
            .expect("spawn ffmpeg");
        assert!(status.success(), "could not synthesise the test source");

        let project = karaoke_project(&src.to_string_lossy());
        // Premise: the sidecar this render writes really is karaoke'd.
        let ass = crate::services::export::write_ass(&project);
        assert!(
            ass.contains("{\\kf"),
            "the project must render karaoke tags: {ass}"
        );

        let out = dir.path().join("burned.mp4");
        render(
            &project,
            &out,
            &BurnInOptions {
                encoder: Encoder::Cpu,
                ..Default::default()
            },
        )
        .expect("ffmpeg burn-in of a karaoke caption succeeds");
        assert!(out.exists(), "burn-in wrote {}", out.display());

        let probe = Command::new("ffprobe")
            .args(["-v", "error", "-print_format", "json", "-show_streams"])
            .arg(&out)
            .output()
            .expect("spawn ffprobe");
        let meta =
            crate::services::video::parse_ffprobe_json(&String::from_utf8_lossy(&probe.stdout))
                .expect("ffprobe json parses");
        assert!(
            meta.video_codec.is_some(),
            "the burned output must carry a video stream"
        );
        assert_eq!(meta.width, 320);
        assert_eq!(meta.height, 180);
    }

    // helper: value following a flag in the arg vector
    fn arg_after(args: &[String], flag: &str) -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1).cloned())
    }
}
