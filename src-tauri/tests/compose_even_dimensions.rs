//! Seam guard: export settings must carry EVEN dimensions end-to-end.
//!
//! H.264/`yuv420p` requires even output dimensions. Sources with odd geometry
//! are real (screen/web captures — the importer's ffprobe reports the dims
//! verbatim into `project.video_width/height`), and the frontend derives its
//! default `ComposeSettings` from those numbers.
//!
//! Regression (seam-compose-settings-missing-even-up /
//! diff-compose-settings-odd-dims): `compose::even_up` existed but was applied
//! ONLY in `proxy_settings` — the export path never rounded. On odd settings
//! the lavfi black canvas (`color=black:s={w}x{h}`, yuv420p) silently rounds
//! itself DOWN while the xfade transition branch scales the incoming clip to
//! the raw odd size; xfade then aborts the whole render ("First input link
//! main parameters … do not match … Failed to inject frame into filter
//! network") — and without a transition the render "succeeded" one pixel
//! short of the requested geometry. Fixed on BOTH sides of the seam: the TS
//! `defaultComposeSettings` rounds up (see the mirror below), and
//! `build_filter_complex` / `run_compose` sanitize whatever a caller supplies.
//!
//! Run (needs `ffmpeg` + `ffprobe` on PATH; generates its own samples):
//! ```sh
//! cargo test --manifest-path src-tauri/Cargo.toml \
//!   --test compose_even_dimensions -- --ignored --nocapture
//! ```

use std::path::Path;
use std::process::Command;

use sundayedit_lib::model::{
    Caption, MediaItem, Project, Style, TimelineItem, TimelineItemKind, Track, TrackKind,
    Transform, Transition,
};
use sundayedit_lib::services::burnin::{Encoder, VideoCodec};
use sundayedit_lib::services::compose::ComposeSettings;
use sundayedit_lib::services::video::{parse_ffprobe_json, MediaKind};

/// Test shim: the real builder is fallible (it refuses item kinds the compose
/// graph cannot render — see `compose::validate_composable`); every fixture in
/// this file is composable.
fn build_filter_complex(
    project: &Project,
    settings: &ComposeSettings,
    ass_file: Option<&str>,
    output: &str,
) -> Vec<String> {
    sundayedit_lib::services::compose::build_filter_complex(project, settings, ass_file, output)
        .expect("fixture must be composable")
}

// ── Builders (mirroring the in-module compose.rs test helpers) ───────────────

fn project(
    media: Vec<MediaItem>,
    tracks: Vec<Track>,
    items: Vec<TimelineItem>,
    captions: Vec<Caption>,
    width: i32,
    height: i32,
) -> Project {
    Project {
        id: "p".into(),
        name: "t".into(),
        video_path: "/x.mp4".into(),
        video_content_hash: "h".into(),
        video_duration_ms: 60_000,
        video_width: width,
        video_height: height,
        video_fps: 30.0,
        audio_wav_path: None,
        language: "no".into(),
        default_style: Style::broadcast_news(),
        context_description: None,
        captions,
        speakers: vec![],
        glossary: vec![],
        clips: vec![],
        talk_summary: None,
        export_config: sundayedit_lib::model::ExportConfig::default(),
        project_meta: sundayedit_lib::model::ProjectMeta::default(),
        created_at: 0,
        updated_at: 0,
        media,
        tracks,
        timeline_items: items,
    }
}

fn media(id: &str, path: &str, width: i32, height: i32, audio: bool) -> MediaItem {
    MediaItem {
        id: id.into(),
        path: path.into(),
        content_hash: "h".into(),
        kind: MediaKind::Video,
        duration_ms: 2_000,
        width,
        height,
        fps: 30.0,
        has_audio: audio,
        audio_wav_path: None,
        original_filename: format!("{id}.mkv"),
        added_at: 0,
    }
}

fn item(
    id: &str,
    track_id: &str,
    media_id: &str,
    start: i64,
    in_ms: i64,
    out_ms: i64,
) -> TimelineItem {
    TimelineItem {
        id: id.into(),
        track_id: track_id.into(),
        kind: TimelineItemKind::Av,
        source_media_id: Some(media_id.into()),
        in_ms,
        out_ms,
        timeline_start_ms: start,
        speed: 1.0,
        gain_db: 0.0,
        fade_in_ms: 0,
        fade_out_ms: 0,
        transform: Transform::default(),
        effects: vec![],
        transition_in: None,
        text: None,
        enabled: true,
        locked: false,
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
        volume_db: 0.0,
    }
}

/// Line-for-line Rust mirror of `defaultComposeSettings` in
/// `src/lib/composeEngine.ts` — INCLUDING its `evenUp` guard (the TS half of
/// the even-dimension seam fix). If the TS function changes, keep this mirror
/// in sync.
fn default_compose_settings_ts_mirror(project: &Project) -> ComposeSettings {
    fn even_up_ts(n: i32) -> i32 {
        let m = n.max(2);
        if m % 2 == 1 {
            m + 1
        } else {
            m
        }
    }
    let width = if project.video_width > 0 {
        project.video_width
    } else {
        1920
    };
    let height = if project.video_height > 0 {
        project.video_height
    } else {
        1080
    };
    let fps = if project.video_fps > 0.0 {
        project.video_fps.round()
    } else {
        30.0
    };
    ComposeSettings {
        width: even_up_ts(width),
        height: even_up_ts(height),
        fps,
        codec: VideoCodec::H264,
        encoder: Encoder::Cpu,
        bitrate_kbps: None,
    }
}

/// Generate a genuinely odd-dimensioned source — ffv1 carries odd dims
/// losslessly (rgb24 before the crop so 4:2:0 chroma subsampling can't round
/// the odd geometry away), `.mkv` is an accepted import extension.
fn generate_odd_source(dst: &Path, width: i32, height: i32, with_audio: bool) {
    let vf = format!("format=rgb24,crop={width}:{height}:0:0");
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=1280x720:rate=30:duration=4",
    ]);
    if with_audio {
        cmd.args(["-f", "lavfi", "-i", "sine=frequency=440:duration=4"]);
    }
    cmd.args(["-vf", &vf, "-c:v", "ffv1"]);
    if with_audio {
        cmd.args(["-c:a", "aac", "-shortest"]);
    } else {
        cmd.arg("-an");
    }
    let st = cmd
        .arg(dst)
        .status()
        .expect("spawn ffmpeg (sample generation)");
    assert!(st.success(), "odd-dim sample generation failed");
}

/// End-to-end: an odd source imported through the app's own probe path, the
/// export button's default settings (TS mirror), a transitioned two-clip
/// timeline — the compose argv must run clean under real ffmpeg.
#[test]
#[ignore = "needs ffmpeg/ffprobe on PATH (generates its own odd-dimension sample)"]
fn compose_export_defaults_survive_odd_source_dimensions() {
    let dir = std::env::temp_dir();
    let src = dir.join("sundayedit_even_641x481.mkv");
    let out = dir.join("sundayedit_even_641_compose.mp4");
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);

    generate_odd_source(&src, 641, 481, true);

    // The app's own probe path reports the odd dimensions verbatim — a real
    // import sets project.video_width/height to exactly these numbers.
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(&src)
        .output()
        .expect("spawn ffprobe");
    let meta =
        parse_ffprobe_json(&String::from_utf8_lossy(&probe.stdout)).expect("ffprobe json parses");
    assert_eq!(meta.width, 641, "source must probe as odd-width");
    assert_eq!(meta.height, 481, "source must probe as odd-height");

    // Two clips back-to-back on one video track, the second entering via a
    // transition — routes down the filter_complex path with an xfade branch.
    let src_str = src.to_string_lossy().into_owned();
    let mut second = item("b", "t1", "m1", 2000, 0, 2000);
    second.transition_in = Some(Transition {
        kind: "fade".into(),
        duration_ms: 500,
    });
    let p = project(
        vec![media("m1", &src_str, meta.width, meta.height, true)],
        vec![track("t1", TrackKind::Video, 0)],
        vec![item("a", "t1", "m1", 0, 0, 2000), second],
        vec![],
        meta.width,
        meta.height,
    );

    // The settings the export button actually sends (TS mirror) round up.
    let settings = default_compose_settings_ts_mirror(&p);
    assert_eq!(
        settings.width, 642,
        "TS default rounds odd width UP to even"
    );
    assert_eq!(
        settings.height, 482,
        "TS default rounds odd height UP to even"
    );

    let out_str = out.to_string_lossy().into_owned();
    let args = build_filter_complex(&p, &settings, None, &out_str);
    let status = Command::new("ffmpeg")
        .args(&args)
        .status()
        .expect("spawn ffmpeg (compose)");

    assert!(
        status.success(),
        "compose export with project-derived default settings must not fail on \
         an odd-dimensioned source; ffmpeg argv: {args:?}"
    );
    assert!(out.exists(), "compose did not write {out_str}");

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

/// Hard-failure guard: multi-clip timeline with a transition. Odd settings
/// used to make the xfade branch (scale=1080:607) disagree with the silently
/// rounded canvas (1080x606) → non-zero exit. Sanitized settings compose fine.
#[test]
#[ignore = "needs ffmpeg/ffprobe on PATH (generates its own odd-dimension sample)"]
fn odd_source_dims_do_not_abort_transition_export() {
    let dir = std::env::temp_dir();
    let src = dir.join("sundayedit_even_1080x607.mkv");
    let out = dir.join("sundayedit_even_1080_xfade.mp4");
    let _ = std::fs::remove_file(&out);
    generate_odd_source(&src, 1080, 607, false);

    let src_str = src.to_string_lossy().into_owned();
    let mut second = item("b", "t1", "m1", 2000, 0, 2000);
    second.transition_in = Some(Transition {
        kind: "fade".into(),
        duration_ms: 500,
    });
    let p = project(
        vec![media("m1", &src_str, 1080, 607, false)],
        vec![track("t1", TrackKind::Video, 0)],
        vec![item("a", "t1", "m1", 0, 0, 2000), second],
        vec![],
        1080,
        607,
    );

    let settings = default_compose_settings_ts_mirror(&p);
    assert_eq!(settings.width, 1080);
    assert_eq!(settings.height, 608, "TS default rounds the odd height up");

    let out_str = out.to_string_lossy().into_owned();
    let args = build_filter_complex(&p, &settings, None, &out_str);
    let output = Command::new("ffmpeg")
        .args(&args)
        .output()
        .expect("spawn ffmpeg (compose)");

    assert!(
        output.status.success(),
        "transitioned compose export must not abort on an odd-dimensioned \
         source.\nffmpeg stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

/// Silent-divergence guard: the SAME odd source without a transition must
/// produce EXACTLY the requested (sanitized) geometry — "output must match
/// what the user saw in preview, pixel-perfect". Unsanitized odd settings used
/// to let the lavfi canvas round itself down (1080x607 requested → 1080x606
/// written).
#[test]
#[ignore = "needs ffmpeg/ffprobe on PATH (generates its own odd-dimension sample)"]
fn odd_source_dims_export_matches_requested_geometry() {
    let dir = std::env::temp_dir();
    let src = dir.join("sundayedit_even_1080x607b.mkv");
    let out = dir.join("sundayedit_even_1080_plain.mp4");
    let _ = std::fs::remove_file(&out);
    generate_odd_source(&src, 1080, 607, false);

    let src_str = src.to_string_lossy().into_owned();
    // Two overlapping clips on SEPARATE tracks — multi-track, non-simple,
    // no transition anywhere.
    let p = project(
        vec![media("m1", &src_str, 1080, 607, false)],
        vec![
            track("t1", TrackKind::Video, 0),
            track("t2", TrackKind::Video, 1),
        ],
        vec![
            item("a", "t1", "m1", 0, 0, 2000),
            item("b", "t2", "m1", 1000, 0, 2000),
        ],
        vec![],
        1080,
        607,
    );

    let settings = default_compose_settings_ts_mirror(&p);
    let out_str = out.to_string_lossy().into_owned();
    let args = build_filter_complex(&p, &settings, None, &out_str);
    let output = Command::new("ffmpeg")
        .args(&args)
        .output()
        .expect("spawn ffmpeg (compose)");
    assert!(
        output.status.success(),
        "plain multi-track compose ran.\nffmpeg stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(&out)
        .output()
        .expect("spawn ffprobe");
    let dims = String::from_utf8_lossy(&probe.stdout).trim().to_string();

    let requested = format!("{},{}", settings.width, settings.height);
    assert_eq!(
        dims, requested,
        "produced geometry must match the requested export settings"
    );

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}
