//! Shared fixtures + measuring helpers for the real-media compose tests.
//!
//! House style here is MEASURE, don't assert on strings: these tests render
//! through the real `build_filter_complex` and then read the result back with
//! ffmpeg — average RGB of a probed patch, container duration, stream
//! geometry. The helpers below are only the plumbing for that.
//!
//! Not every test file uses every helper (Cargo compiles this module once per
//! integration-test binary), hence the blanket allow.
#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

pub use sundayedit_lib::model::{TrackKind, Transform};

use sundayedit_lib::model::{
    Caption, MediaItem, Project, Style, TimelineItem, TimelineItemKind, Track,
};
use sundayedit_lib::services::burnin::{Encoder, VideoCodec};
use sundayedit_lib::services::compose::ComposeSettings;
use sundayedit_lib::services::video::MediaKind;

// ── project fixtures ─────────────────────────────────────────────────────────

pub fn project(
    media: Vec<MediaItem>,
    tracks: Vec<Track>,
    items: Vec<TimelineItem>,
    width: i32,
    height: i32,
) -> Project {
    Project {
        id: "p".into(),
        name: "t".into(),
        video_path: "/x.mp4".into(),
        video_content_hash: "h".into(),
        video_duration_ms: 4_000,
        video_width: width,
        video_height: height,
        video_fps: 25.0,
        audio_wav_path: None,
        language: "no".into(),
        default_style: Style::broadcast_news(),
        context_description: None,
        captions: Vec::<Caption>::new(),
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

/// A pooled media item whose stored geometry is stated explicitly — the
/// numbers `build_filter_complex` reads to decide whether fitting this source
/// to the canvas leaves letterbox bars.
pub fn media_sized(id: &str, path: &str, width: i32, height: i32, duration_ms: i64) -> MediaItem {
    MediaItem {
        id: id.into(),
        path: path.into(),
        content_hash: format!("h-{id}"),
        kind: MediaKind::Video,
        duration_ms,
        width,
        height,
        fps: 25.0,
        has_audio: false,
        audio_wav_path: None,
        original_filename: format!("{id}.mp4"),
        added_at: 0,
    }
}

/// Same, but the pool entry declares an audio stream so the compose graph
/// builds the `adelay`/`amix` branch for it too.
pub fn media_sized_with_audio(
    id: &str,
    path: &str,
    width: i32,
    height: i32,
    duration_ms: i64,
) -> MediaItem {
    MediaItem {
        has_audio: true,
        ..media_sized(id, path, width, height, duration_ms)
    }
}

pub fn item(
    id: &str,
    track_id: &str,
    media_id: &str,
    start: i64,
    in_ms: i64,
    out_ms: i64,
    transform: Transform,
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
        transform,
        effects: vec![],
        transition_in: None,
        text: None,
        enabled: true,
        locked: false,
    }
}

pub fn track(id: &str, kind: TrackKind, index: i32) -> Track {
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

pub fn canvas_settings(width: i32, height: i32, fps: f32) -> ComposeSettings {
    ComposeSettings {
        width,
        height,
        fps,
        codec: VideoCodec::H264,
        encoder: Encoder::Cpu,
        bitrate_kbps: None,
    }
}

// ── ffmpeg measuring helpers ─────────────────────────────────────────────────

/// Render a SOLID-colour clip. Solid colours make a sampled mean unambiguous:
/// "is this region clip A, clip B, or the canvas?" has a numeric answer that
/// no codec ringing can blur past the thresholds these tests use.
pub fn generate_solid(dst: &Path, colour: &str, width: i32, height: i32, seconds: f64) {
    let _ = std::fs::remove_file(dst);
    let st = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c={colour}:s={width}x{height}:r=25:d={seconds}"),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-an",
        ])
        .arg(dst)
        .status()
        .expect("spawn ffmpeg (solid fixture)");
    assert!(st.success(), "solid fixture generation failed for {colour}");
}

/// A solid-colour clip that also carries a steady tone, so the muxed output's
/// audio stream is real and its length is measurable.
pub fn generate_solid_with_tone(
    dst: &Path,
    colour: &str,
    width: i32,
    height: i32,
    seconds: f64,
    hz: i32,
) {
    let _ = std::fs::remove_file(dst);
    let st = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c={colour}:s={width}x{height}:r=25:d={seconds}"),
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency={hz}:sample_rate=48000:duration={seconds}"),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(dst)
        .status()
        .expect("spawn ffmpeg (tone fixture)");
    assert!(st.success(), "tone fixture generation failed for {colour}");
}

/// Mean RGB of a `w`x`h` patch at (`x`,`y`) in the frame at `t` seconds.
/// Output-side `-ss` (after `-i`) so the seek is frame-accurate.
pub fn sample_rgb(video: &Path, t: f64, x: i64, y: i64, w: i64, h: i64) -> (f64, f64, f64) {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(video)
        .args([
            "-ss",
            &format!("{t}"),
            "-frames:v",
            "1",
            "-vf",
            &format!("crop={w}:{h}:{x}:{y}"),
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-an",
            "-",
        ])
        .output()
        .expect("spawn ffmpeg (sample)");
    assert!(
        out.status.success(),
        "sampling {video:?} at t={t} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let px = out.stdout;
    let expect = (w * h * 3) as usize;
    assert_eq!(
        px.len(),
        expect,
        "expected {expect} raw bytes at t={t} ({w}x{h}), got {}",
        px.len()
    );
    let (mut r, mut g, mut b) = (0f64, 0f64, 0f64);
    for chunk in px.as_chunks::<3>().0 {
        r += chunk[0] as f64;
        g += chunk[1] as f64;
        b += chunk[2] as f64;
    }
    let n = (w * h) as f64;
    (r / n, g / n, b / n)
}

/// The container's own duration, in seconds — what a player's scrub bar shows.
pub fn container_duration_secs(path: &Path) -> f64 {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("spawn ffprobe (duration)");
    assert!(
        out.status.success(),
        "ffprobe could not read {path:?}:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .unwrap_or_else(|e| panic!("unparsable duration for {path:?}: {e}"))
}

/// The duration of one stream (`v:0` / `a:0`), in seconds. The container's
/// duration is the longer of the two, so a drifting audio branch only shows
/// up when you ask each stream separately.
pub fn stream_duration_secs(path: &Path, stream: &str) -> f64 {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            stream,
            "-show_entries",
            "stream=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("spawn ffprobe (stream duration)");
    assert!(out.status.success(), "ffprobe failed for {path:?}");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .unwrap_or_else(|e| panic!("unparsable stream duration for {path:?}: {e}"))
}

/// Run a built compose argv, asserting ffmpeg succeeded.
pub fn run_compose_argv(args: &[String], out: &Path) {
    let _ = std::fs::remove_file(out);
    let res = Command::new("ffmpeg")
        .args(args)
        .output()
        .expect("spawn ffmpeg (compose)");
    assert!(
        res.status.success(),
        "compose render failed; argv: {args:?}\nstderr:\n{}",
        String::from_utf8_lossy(&res.stderr)
    );
    assert!(out.exists(), "compose wrote nothing to {out:?}");
}
