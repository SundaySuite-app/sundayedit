//! Video/audio import — Phase 1.1.
//!
//! Three concerns:
//!   1. Format validation — which files we accept, video vs audio-only.
//!   2. Metadata probing via `ffprobe` (parse its JSON output).
//!   3. Content hashing for path-stability (relink when a file moves).
//!
//! ffprobe/ffmpeg are invoked as external processes. In production they
//! ship as Tauri sidecar binaries (like SundayRec bundles ffmpeg-static);
//! in dev we fall back to whatever is on PATH. The path is resolved via
//! `ffprobe_path()` / `ffmpeg_path()` so the sidecar wiring (Phase 9.2)
//! is a one-line change.
//!
//! The JSON-parsing logic is split into a pure `parse_ffprobe_json`
//! function so it's unit-testable against captured fixtures WITHOUT
//! ffmpeg installed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, AppResult};

// ── Supported formats ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/lib/bindings/MediaKind.ts")]
pub enum MediaKind {
    /// Has a video stream — show the player.
    Video,
    /// Audio only — skip the player, go straight to transcribe.
    AudioOnly,
}

const VIDEO_EXTS: &[&str] = &["mp4", "mov", "mkv", "webm", "avi", "m4v"];
const AUDIO_EXTS: &[&str] = &["mp3", "wav", "m4a", "flac", "ogg"];

/// Classify a file by extension. Returns `None` for unsupported formats.
/// The authoritative check is the ffprobe result (a `.mp4` with no video
/// stream is really audio-only) — this is the fast pre-filter for the
/// file picker + drag-drop.
pub fn classify_extension(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if VIDEO_EXTS.contains(&ext.as_str()) {
        Some(MediaKind::Video)
    } else if AUDIO_EXTS.contains(&ext.as_str()) {
        Some(MediaKind::AudioOnly)
    } else {
        None
    }
}

/// All accepted extensions — used to build the native file-picker filter.
pub fn accepted_extensions() -> Vec<&'static str> {
    VIDEO_EXTS
        .iter()
        .chain(AUDIO_EXTS.iter())
        .copied()
        .collect()
}

// ── Metadata ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/VideoMetadata.ts")]
pub struct VideoMetadata {
    #[ts(type = "number")]
    pub duration_ms: i64,
    pub width: i32,
    pub height: i32,
    pub fps: f32,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub audio_channels: Option<i32>,
    pub audio_sample_rate: Option<i32>,
    pub container: Option<String>,
    pub kind: MediaKind,
}

/// Probe a media file's metadata via ffprobe.
pub fn probe(path: &Path) -> AppResult<VideoMetadata> {
    if !path.exists() {
        return Err(AppError::VideoMissing(path.to_string_lossy().to_string()));
    }
    let output = Command::new(ffprobe_path())
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .map_err(|e| AppError::Internal(format!("failed to launch ffprobe: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Validation(format!(
            "ffprobe could not read '{}': {}",
            path.display(),
            stderr.trim()
        )));
    }

    let json = String::from_utf8_lossy(&output.stdout);
    parse_ffprobe_json(&json)
}

/// Pure parser for ffprobe's `-print_format json -show_format -show_streams`
/// output. Unit-testable against fixtures without ffmpeg installed.
pub fn parse_ffprobe_json(json: &str) -> AppResult<VideoMetadata> {
    let v: serde_json::Value = serde_json::from_str(json)?;

    let streams = v
        .get("streams")
        .and_then(|s| s.as_array())
        .ok_or_else(|| AppError::Validation("ffprobe output has no streams array".to_string()))?;

    let video_stream = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|t| t.as_str()) == Some("video"));
    let audio_stream = streams
        .iter()
        .find(|s| s.get("codec_type").and_then(|t| t.as_str()) == Some("audio"));

    if video_stream.is_none() && audio_stream.is_none() {
        return Err(AppError::Validation(
            "file has neither a video nor an audio stream".to_string(),
        ));
    }

    // Duration: prefer format.duration, fall back to a stream's duration.
    let duration_secs = v
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(|d| d.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| {
            streams.iter().find_map(|s| {
                s.get("duration")
                    .and_then(|d| d.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
            })
        })
        .unwrap_or(0.0);
    let duration_ms = (duration_secs * 1000.0).round() as i64;

    let container = v
        .get("format")
        .and_then(|f| f.get("format_name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string());

    let (width, height, fps, video_codec) = match video_stream {
        Some(s) => (
            s.get("width").and_then(|w| w.as_i64()).unwrap_or(0) as i32,
            s.get("height").and_then(|h| h.as_i64()).unwrap_or(0) as i32,
            parse_fps(s.get("r_frame_rate").and_then(|r| r.as_str())),
            s.get("codec_name")
                .and_then(|c| c.as_str())
                .map(String::from),
        ),
        None => (0, 0, 0.0, None),
    };

    let (audio_codec, audio_channels, audio_sample_rate) = match audio_stream {
        Some(s) => (
            s.get("codec_name")
                .and_then(|c| c.as_str())
                .map(String::from),
            s.get("channels").and_then(|c| c.as_i64()).map(|c| c as i32),
            s.get("sample_rate")
                .and_then(|r| r.as_str())
                .and_then(|s| s.parse::<i32>().ok()),
        ),
        None => (None, None, None),
    };

    let kind = if video_stream.is_some() {
        MediaKind::Video
    } else {
        MediaKind::AudioOnly
    };

    Ok(VideoMetadata {
        duration_ms,
        width,
        height,
        fps,
        video_codec,
        audio_codec,
        audio_channels,
        audio_sample_rate,
        container,
        kind,
    })
}

/// ffprobe reports frame rate as a rational string like "30000/1001".
fn parse_fps(r: Option<&str>) -> f32 {
    match r {
        Some(s) => {
            if let Some((num, den)) = s.split_once('/') {
                let num: f32 = num.parse().unwrap_or(0.0);
                let den: f32 = den.parse().unwrap_or(1.0);
                if den != 0.0 {
                    num / den
                } else {
                    0.0
                }
            } else {
                s.parse().unwrap_or(0.0)
            }
        }
        None => 0.0,
    }
}

// ── Content hashing (path stability) ──────────────────────────────────────────

/// Compute a fast, stable fingerprint of a media file for relink matching.
///
/// We do NOT hash the whole file — a 4 GB video would take seconds. Instead
/// we hash the file size + the first 64 KB + the last 64 KB. This is
/// extremely unlikely to collide for distinct media files and is O(1) in
/// file size.
pub fn content_hash(path: &Path) -> AppResult<String> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom};

    const CHUNK: usize = 64 * 1024;

    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();

    let mut hasher = Sha256::new();
    hasher.update(len.to_le_bytes());

    // Head
    let mut head = vec![0u8; CHUNK.min(len as usize)];
    file.read_exact(&mut head)?;
    hasher.update(&head);

    // Tail (only if the file is bigger than one chunk)
    if len as usize > CHUNK {
        let tail_start = len.saturating_sub(CHUNK as u64);
        file.seek(SeekFrom::Start(tail_start))?;
        let mut tail = vec![0u8; CHUNK];
        file.read_exact(&mut tail)?;
        hasher.update(&tail);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// Search common locations for a file matching `target_hash`. Used when a
/// project's video has moved/renamed since last open.
///
/// Returns the first matching path. Only considers files whose extension
/// is a supported media format (cheap filter before the hash).
pub fn find_relink_candidate(
    target_hash: &str,
    search_dirs: &[PathBuf],
    original_filename: Option<&str>,
) -> AppResult<Option<PathBuf>> {
    for dir in search_dirs {
        if !dir.is_dir() {
            continue;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        // First pass: same filename (fast win — most moves keep the name)
        if let Some(name) = original_filename {
            let candidate = dir.join(name);
            if candidate.is_file() {
                if let Ok(h) = content_hash(&candidate) {
                    if h == target_hash {
                        return Ok(Some(candidate));
                    }
                }
            }
        }
        // Second pass: any supported media file in the dir
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() && classify_extension(&p).is_some() {
                if let Ok(h) = content_hash(&p) {
                    if h == target_hash {
                        return Ok(Some(p));
                    }
                }
            }
        }
    }
    Ok(None)
}

// ── Clip thumbnails ─────────────────────────────────────────────────────────────

/// Build the ffmpeg argument vector for a single-frame thumbnail grab at
/// `at_ms`, scaled to 120px tall (width auto, even). Input-side `-ss` seeking
/// keeps it fast on long media. Pure — no IO — so it's unit-testable.
pub fn thumbnail_args(media_path: &str, at_ms: i64, out_path: &str) -> Vec<String> {
    vec![
        "-ss".into(),
        format!("{:.3}", at_ms.max(0) as f64 / 1000.0),
        "-i".into(),
        media_path.into(),
        "-frames:v".into(),
        "1".into(),
        "-vf".into(),
        "scale=-2:120".into(),
        "-y".into(),
        out_path.into(),
    ]
}

/// Extract a single JPEG thumbnail frame from `media_path` at `at_ms` into
/// `out_path`. Returns `out_path` on success. Spawns the bundled ffmpeg.
pub fn extract_thumbnail(media_path: &str, at_ms: i64, out_path: &str) -> AppResult<String> {
    // ffmpeg won't create missing directories — the frontend asks for a
    // `<cache>/thumbnails/<id>.jpg` path (same guard as extract_audio_wav).
    if let Some(parent) = std::path::Path::new(out_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let args = thumbnail_args(media_path, at_ms, out_path);
    let status = Command::new(ffmpeg_path())
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| AppError::Internal(format!("failed to launch ffmpeg for thumbnail: {e}")))?;
    if !status.success() {
        return Err(AppError::Internal(format!(
            "ffmpeg thumbnail extraction failed for '{media_path}'"
        )));
    }
    Ok(out_path.to_string())
}

// ── Binary resolution ──────────────────────────────────────────────────────────
//
// Resolution order (first hit wins):
//   1. Env override (SUNDAYEDIT_FFMPEG / SUNDAYEDIT_FFPROBE) — dev + tests.
//   2. Bundled sidecar next to the app executable — production. Tauri's
//      `externalBin` drops `ffmpeg`/`ffprobe` into Contents/MacOS (or the
//      install dir on Windows) with the target-triple suffix stripped.
//   3. Bare name on PATH — a system ffmpeg, e.g. `brew install ffmpeg`.

/// Look for `name` (e.g. "ffmpeg") next to the current executable — that's
/// where Tauri places bundled `externalBin` sidecars at runtime.
fn sidecar_path(name: &str) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let candidate = dir.join(file);
    candidate
        .is_file()
        .then(|| candidate.to_string_lossy().into_owned())
}

fn ffprobe_path() -> String {
    if let Ok(p) = std::env::var("SUNDAYEDIT_FFPROBE") {
        return p;
    }
    sidecar_path("ffprobe").unwrap_or_else(|| "ffprobe".to_string())
}

/// Path to the ffmpeg binary (used by the waveform extractor + burn-in).
pub fn ffmpeg_path() -> String {
    if let Ok(p) = std::env::var("SUNDAYEDIT_FFMPEG") {
        return p;
    }
    sidecar_path("ffmpeg").unwrap_or_else(|| "ffmpeg".to_string())
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ── Extension classification ───────────────────────────────────────────
    #[test]
    fn classifies_video_extensions() {
        assert_eq!(
            classify_extension(Path::new("/x/clip.mp4")),
            Some(MediaKind::Video)
        );
        assert_eq!(
            classify_extension(Path::new("/x/clip.MOV")),
            Some(MediaKind::Video)
        );
        assert_eq!(
            classify_extension(Path::new("/x/clip.mkv")),
            Some(MediaKind::Video)
        );
        assert_eq!(
            classify_extension(Path::new("/x/clip.webm")),
            Some(MediaKind::Video)
        );
    }

    #[test]
    fn classifies_audio_extensions() {
        assert_eq!(
            classify_extension(Path::new("/x/pod.mp3")),
            Some(MediaKind::AudioOnly)
        );
        assert_eq!(
            classify_extension(Path::new("/x/voice.wav")),
            Some(MediaKind::AudioOnly)
        );
        assert_eq!(
            classify_extension(Path::new("/x/sound.flac")),
            Some(MediaKind::AudioOnly)
        );
    }

    #[test]
    fn rejects_unsupported_extensions() {
        assert_eq!(classify_extension(Path::new("/x/doc.pdf")), None);
        assert_eq!(classify_extension(Path::new("/x/no-extension")), None);
        assert_eq!(classify_extension(Path::new("/x/image.png")), None);
    }

    #[test]
    fn accepted_extensions_covers_both_kinds() {
        let exts = accepted_extensions();
        assert!(exts.contains(&"mp4"));
        assert!(exts.contains(&"mp3"));
        assert_eq!(exts.len(), VIDEO_EXTS.len() + AUDIO_EXTS.len());
    }

    // ── fps parsing ─────────────────────────────────────────────────────────
    #[test]
    fn parses_rational_fps() {
        assert!((parse_fps(Some("30/1")) - 30.0).abs() < 0.001);
        assert!((parse_fps(Some("30000/1001")) - 29.97).abs() < 0.01);
        assert!((parse_fps(Some("25/1")) - 25.0).abs() < 0.001);
        assert_eq!(parse_fps(Some("0/0")), 0.0); // div by zero guard
        assert_eq!(parse_fps(None), 0.0);
    }

    // ── ffprobe JSON parsing ─────────────────────────────────────────────────
    #[test]
    fn parses_video_with_audio() {
        let json = r#"{
          "streams": [
            { "codec_type": "video", "codec_name": "h264", "width": 1920, "height": 1080, "r_frame_rate": "30000/1001" },
            { "codec_type": "audio", "codec_name": "aac", "channels": 2, "sample_rate": "48000" }
          ],
          "format": { "duration": "123.456", "format_name": "mov,mp4,m4a,3gp,3g2,mj2" }
        }"#;
        let m = parse_ffprobe_json(json).unwrap();
        assert_eq!(m.kind, MediaKind::Video);
        assert_eq!(m.width, 1920);
        assert_eq!(m.height, 1080);
        assert!((m.fps - 29.97).abs() < 0.01);
        assert_eq!(m.duration_ms, 123_456);
        assert_eq!(m.video_codec.as_deref(), Some("h264"));
        assert_eq!(m.audio_codec.as_deref(), Some("aac"));
        assert_eq!(m.audio_channels, Some(2));
        assert_eq!(m.audio_sample_rate, Some(48000));
    }

    #[test]
    fn parses_audio_only() {
        let json = r#"{
          "streams": [
            { "codec_type": "audio", "codec_name": "mp3", "channels": 1, "sample_rate": "44100" }
          ],
          "format": { "duration": "60.0", "format_name": "mp3" }
        }"#;
        let m = parse_ffprobe_json(json).unwrap();
        assert_eq!(m.kind, MediaKind::AudioOnly);
        assert_eq!(m.width, 0);
        assert_eq!(m.height, 0);
        assert_eq!(m.duration_ms, 60_000);
        assert_eq!(m.audio_codec.as_deref(), Some("mp3"));
    }

    #[test]
    fn rejects_no_streams() {
        let json = r#"{ "streams": [], "format": {} }"#;
        assert!(parse_ffprobe_json(json).is_err());
    }

    #[test]
    fn falls_back_to_stream_duration() {
        let json = r#"{
          "streams": [
            { "codec_type": "video", "codec_name": "h264", "width": 640, "height": 480, "r_frame_rate": "25/1", "duration": "10.5" }
          ],
          "format": { "format_name": "avi" }
        }"#;
        let m = parse_ffprobe_json(json).unwrap();
        assert_eq!(m.duration_ms, 10_500);
    }

    // ── content hashing ───────────────────────────────────────────────────────
    #[test]
    fn content_hash_is_stable_and_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        std::fs::File::create(&a)
            .unwrap()
            .write_all(b"hello world content")
            .unwrap();
        std::fs::File::create(&b)
            .unwrap()
            .write_all(b"different content!!")
            .unwrap();

        let ha1 = content_hash(&a).unwrap();
        let ha2 = content_hash(&a).unwrap();
        let hb = content_hash(&b).unwrap();

        assert_eq!(ha1, ha2, "same file hashes identically");
        assert_ne!(ha1, hb, "different files hash differently");
        assert_eq!(ha1.len(), 64, "sha-256 hex is 64 chars");
    }

    #[test]
    fn content_hash_handles_large_files() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.bin");
        // 200 KB > 2× chunk size, exercises the head+tail path
        let data = vec![7u8; 200 * 1024];
        std::fs::File::create(&big)
            .unwrap()
            .write_all(&data)
            .unwrap();
        let h = content_hash(&big).unwrap();
        assert_eq!(h.len(), 64);
    }

    // ── relink ──────────────────────────────────────────────────────────────
    #[test]
    fn relink_finds_moved_file_by_hash() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("sermon.mp4");
        std::fs::File::create(&original)
            .unwrap()
            .write_all(b"video bytes here")
            .unwrap();
        let hash = content_hash(&original).unwrap();

        // Move it (rename) to simulate the user relocating it
        let moved = dir.path().join("sermon-final.mp4");
        std::fs::rename(&original, &moved).unwrap();

        let found =
            find_relink_candidate(&hash, &[dir.path().to_path_buf()], Some("sermon.mp4")).unwrap();
        assert_eq!(found, Some(moved));
    }

    // ── thumbnail arg construction ────────────────────────────────────────────
    #[test]
    fn thumbnail_args_seek_scale_and_output() {
        let args = thumbnail_args("/media/clip.mp4", 2500, "/out/thumb.jpg");
        // Input-side seek in seconds.
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        assert_eq!(args[ss + 1], "2.500");
        // Single frame, 120px tall (even width).
        let vf = args.iter().position(|a| a == "-vf").unwrap();
        assert_eq!(args[vf + 1], "scale=-2:120");
        assert!(args.windows(2).any(|w| w[0] == "-frames:v" && w[1] == "1"));
        // Output is the final arg, overwrite enabled.
        assert_eq!(args.last().unwrap(), "/out/thumb.jpg");
        assert!(args.iter().any(|a| a == "-y"));
    }

    #[test]
    fn thumbnail_args_clamp_negative_time_to_zero() {
        let args = thumbnail_args("/m.mp4", -1000, "/o.jpg");
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        assert_eq!(args[ss + 1], "0.000");
    }

    #[test]
    fn relink_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let found = find_relink_candidate("deadbeef", &[dir.path().to_path_buf()], None).unwrap();
        assert_eq!(found, None);
    }
}
