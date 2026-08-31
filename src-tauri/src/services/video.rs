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

/// The frame-rate window this app is willing to probe, canvas and render at.
///
/// `MIN_FPS` is where a "frame rate" stops describing motion; `MAX_FPS` is
/// comfortably above every real capture device (240 fps slow-motion phones)
/// and far below the four-digit tick bases VFR containers report.
pub const MIN_FPS: f32 = 1.0;
pub const MAX_FPS: f32 = 240.0;
/// What we canvas at when nothing plausible could be read at all.
pub const DEFAULT_FPS: f32 = 30.0;

/// Is `fps` a rate a real camera or screen recorder could have produced?
pub fn plausible_fps(fps: f32) -> bool {
    fps.is_finite() && (MIN_FPS..=MAX_FPS).contains(&fps)
}

/// Force any frame rate into `MIN_FPS..=MAX_FPS`, falling back to
/// `DEFAULT_FPS` for anything non-finite or non-positive.
///
/// This is the last line of defence, mirrored in TypeScript by
/// `composeEngine.ts::saneFps` and pinned by `fps_sanity_parity.rs`: an
/// unclamped rate flows straight into the export as `-r 1000`, which produces
/// a file no player will scrub and an encode that takes ~33× as long as the
/// footage deserves.
pub fn sane_fps(fps: f32) -> f32 {
    if !fps.is_finite() || fps <= 0.0 {
        DEFAULT_FPS
    } else {
        fps.clamp(MIN_FPS, MAX_FPS)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/VideoMetadata.ts")]
pub struct VideoMetadata {
    #[ts(type = "number")]
    pub duration_ms: i64,
    /// DISPLAY width — the coded width, with width/height swapped when the
    /// container asks for a quarter turn. See `parse_rotation`.
    pub width: i32,
    /// DISPLAY height. See `width`.
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
        Some(s) => {
            let coded_w = s.get("width").and_then(|w| w.as_i64()).unwrap_or(0) as i32;
            let coded_h = s.get("height").and_then(|h| h.as_i64()).unwrap_or(0) as i32;
            // Quarter turn → the frame everyone downstream sees is transposed.
            let (w, h) = if parse_rotation(s) % 180 == 90 {
                (coded_h, coded_w)
            } else {
                (coded_w, coded_h)
            };
            (
                w,
                h,
                choose_fps(
                    s.get("r_frame_rate").and_then(|r| r.as_str()),
                    s.get("avg_frame_rate").and_then(|r| r.as_str()),
                ),
                s.get("codec_name")
                    .and_then(|c| c.as_str())
                    .map(String::from),
            )
        }
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

/// The display rotation the container asks for, normalised to 0/90/180/270.
///
/// MEASURED, not assumed (ffmpeg 6.0 and 8.1, `-metadata:s:v rotate=90` and
/// `-display_rotation`): ffmpeg AUTO-ROTATES on decode, and it does so inside
/// `-filter_complex` too — a stream ffprobe reports as `320x120` arrives in
/// the graph as `120x320`. So the honest number to report is the DISPLAY size,
/// and the filtergraph must NOT rotate again (`transform_filters` only ever
/// applies the user's own `Transform.rotation_deg`).
///
/// Two spellings, because both are in the wild:
///   - `side_data_list[].rotation` — the display matrix, what every ffprobe
///     since 4.x emits and the only spelling ffprobe 8 still emits.
///   - `tags.rotate` — the legacy mov/mp4 tag, still written by ffprobe 6 and
///     by plenty of phone/camera firmware. Its sign is the INVERSE of the
///     display matrix's (`rotate=90` writes a matrix reading `rotation: 90`
///     and a tag reading `270`), which is irrelevant here: the only question
///     we ask of the answer is "is this a quarter turn?", and `90` and `270`
///     answer it identically.
fn parse_rotation(stream: &serde_json::Value) -> i32 {
    let from_side_data = stream
        .get("side_data_list")
        .and_then(|l| l.as_array())
        .and_then(|list| list.iter().find_map(|sd| sd.get("rotation")))
        .and_then(|r| r.as_f64());

    let from_tag = || {
        stream
            .get("tags")
            .and_then(|t| t.get("rotate"))
            .and_then(|r| {
                r.as_f64()
                    .or_else(|| r.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
            })
    };

    let deg = from_side_data.or_else(from_tag).unwrap_or(0.0);
    if !deg.is_finite() {
        return 0;
    }
    // Snap to the nearest quarter turn, then wrap into 0..360 (`%` in Rust
    // keeps the sign of the dividend, and `-90` is the common spelling).
    let quarters = (deg / 90.0).round() as i64;
    (((quarters % 4) + 4) % 4) as i32 * 90
}

/// Which of ffprobe's two frame-rate fields to believe.
///
/// `r_frame_rate` is the NOMINAL rate — for a VFR screen recording it is the
/// container's tick base (`1000/1`, `600/1`), not a rate anything was shot at.
/// Left unchecked it flowed all the way into the export as `-r 1000`.
/// `avg_frame_rate` is the measured average over the file, which is exactly
/// the right answer for VFR sources. So: believe `r_frame_rate` while it is
/// plausible, otherwise `avg_frame_rate`, otherwise clamp whatever we got.
fn choose_fps(r_frame_rate: Option<&str>, avg_frame_rate: Option<&str>) -> f32 {
    let r = parse_fps(r_frame_rate);
    if plausible_fps(r) {
        return r;
    }
    let avg = parse_fps(avg_frame_rate);
    if plausible_fps(avg) {
        return avg;
    }
    // Neither field is renderable. Clamp the larger of the two into the
    // window (a `1000/1` tick base becomes `MAX_FPS`, a `1/5` timelapse
    // `MIN_FPS`); `sane_fps` turns "nothing at all" into `DEFAULT_FPS`.
    sane_fps(r.max(avg))
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

// ── Availability (relink detection) ─────────────────────────────────────────────

/// Whether one pooled `MediaItem`'s file is still where the project says it is.
///
/// Returned by the `check_media_paths` command, which the renderer runs on
/// project open so a moved/renamed source shows the relink affordance instead
/// of a silent "Video utilgjengelig" preview and a failing export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/MediaAvailability.ts")]
pub struct MediaAvailability {
    pub media_id: String,
    pub path: String,
    pub exists: bool,
}

/// Pure core of `check_media_paths`: map each `(media_id, path)` through an
/// existence predicate, preserving pool order.
///
/// The filesystem call is injected so the mapping is unit-testable without
/// touching disk — the command wrapper supplies `Path::exists`. Deliberately
/// does NOT hash: this runs on every project open and must stay a stat per
/// file, not a read of every byte (that is `find_relink_candidate`'s job,
/// and only once we already know something is missing).
pub fn media_availability<F>(items: &[(String, String)], exists: F) -> Vec<MediaAvailability>
where
    F: Fn(&str) -> bool,
{
    items
        .iter()
        .map(|(media_id, path)| MediaAvailability {
            media_id: media_id.clone(),
            path: path.clone(),
            exists: exists(path),
        })
        .collect()
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

// ── Filmstrip tiles ─────────────────────────────────────────────────────────────
//
// A filmstrip tile is ONE jpeg holding `cols` frames side by side, covering a
// fixed slice of the timeline. The slices are addressed on the absolute grid
// in `services::tiles`, so scrolling and zooming reuse already-rendered tiles
// instead of re-rendering a viewport-shaped strip every time.

/// Build the ffmpeg argument vector for one filmstrip tile: `cols` frames
/// evenly spaced across `[start_ms, end_ms)`, scaled to `height` px tall
/// (even width), tiled horizontally into a single image.
///
/// Clamps rather than rejects — a negative start snaps to 0, a non-positive
/// range becomes 1 ms, `cols` lands in `1..=64` and `height` in `8..=512`.
/// Pure — no IO — so it's unit-testable without ffmpeg installed.
pub fn filmstrip_tile_args(
    media_path: &str,
    start_ms: i64,
    end_ms: i64,
    cols: u32,
    height: u32,
    out_path: &str,
) -> Vec<String> {
    let start = start_ms.max(0);
    let dur_ms = (end_ms - start).max(1);
    let cols = cols.clamp(1, 64);
    // Even height keeps the scaler happy for every codec we accept.
    let height = height.clamp(8, 512) & !1;

    // `cols` frames across `dur_ms` → an exact rational frame rate, so the
    // sampled instants are deterministic (and identical for the same tile on
    // any machine): cols / (dur_ms / 1000) = cols*1000 / dur_ms.
    let vf = format!(
        "fps={}/{},scale=-2:{},tile={}x1",
        cols as i64 * 1000,
        dur_ms,
        height,
        cols
    );

    vec![
        // Input-side seek + duration limit: ffmpeg only decodes the slice.
        "-ss".into(),
        format!("{:.3}", start as f64 / 1000.0),
        "-t".into(),
        format!("{:.3}", dur_ms as f64 / 1000.0),
        "-i".into(),
        media_path.into(),
        "-an".into(),
        "-vf".into(),
        vf,
        "-frames:v".into(),
        "1".into(),
        "-y".into(),
        out_path.into(),
    ]
}

/// Render one filmstrip tile — `cols` frames from `[start_ms, end_ms)` of
/// `media_path` into a single horizontal-strip JPEG at `out_path`. Returns
/// `out_path` on success. Spawns the bundled ffmpeg.
pub fn extract_filmstrip_tile(
    media_path: &str,
    start_ms: i64,
    end_ms: i64,
    cols: u32,
    out_path: &str,
) -> AppResult<String> {
    // Same guard as extract_thumbnail — ffmpeg won't create the cache dir.
    if let Some(parent) = std::path::Path::new(out_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let args = filmstrip_tile_args(
        media_path,
        start_ms,
        end_ms,
        cols,
        crate::services::tiles::TILE_HEIGHT_PX,
        out_path,
    );
    let status = Command::new(ffmpeg_path())
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| AppError::Internal(format!("failed to launch ffmpeg for filmstrip: {e}")))?;
    if !status.success() {
        return Err(AppError::Internal(format!(
            "ffmpeg filmstrip tile extraction failed for '{media_path}'"
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

    // ── fps sanity ──────────────────────────────────────────────────────────
    #[test]
    fn an_implausible_nominal_rate_falls_back_to_the_measured_average() {
        // The VFR screen-recording shape: 1 ms tick base, ~30 fps in practice.
        assert!((choose_fps(Some("1000/1"), Some("30000/1001")) - 29.97).abs() < 0.01);
        // A plausible nominal rate is believed even when avg disagrees (a
        // trimmed clip's avg is routinely a hair off; `r` is the real rate).
        assert!((choose_fps(Some("25/1"), Some("24/1")) - 25.0).abs() < 0.001);
        // Nothing usable anywhere → the window's default, not 0 and not 1000.
        assert_eq!(choose_fps(Some("0/0"), Some("0/0")), DEFAULT_FPS);
        assert_eq!(choose_fps(None, None), DEFAULT_FPS);
        // Both absurd → clamped, never passed through.
        assert_eq!(choose_fps(Some("1000/1"), Some("600/1")), MAX_FPS);
        // A real slow timelapse: below the window, clamped up rather than
        // dropped to the default.
        assert_eq!(choose_fps(Some("1/5"), Some("1/5")), MIN_FPS);
    }

    #[test]
    fn sane_fps_covers_every_way_a_rate_can_be_useless() {
        assert_eq!(sane_fps(0.0), DEFAULT_FPS);
        assert_eq!(sane_fps(-30.0), DEFAULT_FPS);
        assert_eq!(sane_fps(f32::NAN), DEFAULT_FPS);
        assert_eq!(sane_fps(f32::INFINITY), DEFAULT_FPS);
        assert_eq!(sane_fps(1000.0), MAX_FPS);
        assert_eq!(sane_fps(0.25), MIN_FPS);
        assert!(
            (sane_fps(29.97) - 29.97).abs() < 1e-6,
            "a real rate is left alone"
        );
    }

    // ── rotation ────────────────────────────────────────────────────────────
    fn stream_with(extra: &str) -> serde_json::Value {
        serde_json::from_str(&format!(
            r#"{{ "codec_type": "video", "width": 1920, "height": 1080,
                  "r_frame_rate": "30/1" {extra} }}"#
        ))
        .unwrap()
    }

    #[test]
    fn reads_rotation_from_the_display_matrix() {
        let s = stream_with(
            r#", "side_data_list": [{ "side_data_type": "Display Matrix", "rotation": -90 }]"#,
        );
        assert_eq!(parse_rotation(&s), 270);
        let s = stream_with(r#", "side_data_list": [{ "rotation": 90 }]"#);
        assert_eq!(parse_rotation(&s), 90);
        let s = stream_with(r#", "side_data_list": [{ "rotation": 180 }]"#);
        assert_eq!(parse_rotation(&s), 180);
    }

    #[test]
    fn falls_back_to_the_legacy_rotate_tag() {
        // ffprobe 6 writes both; ffprobe 8 writes only the matrix; camera
        // firmware and older muxers write only the tag.
        let s = stream_with(r#", "tags": { "rotate": "270" }"#);
        assert_eq!(parse_rotation(&s), 270);
        // Some tools emit it as a number rather than a string.
        let s = stream_with(r#", "tags": { "rotate": 90 }"#);
        assert_eq!(parse_rotation(&s), 90);
        // The display matrix WINS when both are present — it is the field
        // every modern ffprobe agrees on, and the two disagree in sign.
        let s =
            stream_with(r#", "tags": { "rotate": "270" }, "side_data_list": [{ "rotation": 90 }]"#);
        assert_eq!(parse_rotation(&s), 90);
    }

    #[test]
    fn a_missing_or_junk_rotation_is_no_rotation() {
        assert_eq!(parse_rotation(&stream_with("")), 0);
        assert_eq!(
            parse_rotation(&stream_with(r#", "tags": { "rotate": "" }"#)),
            0
        );
        assert_eq!(parse_rotation(&stream_with(r#", "side_data_list": []"#)), 0);
        // Non-quarter turns snap to the nearest quarter (a 5° tilt is not a
        // transpose, and ffmpeg's autorotate would not transpose for it).
        assert_eq!(
            parse_rotation(&stream_with(r#", "tags": { "rotate": "5" }"#)),
            0
        );
        assert_eq!(
            parse_rotation(&stream_with(r#", "tags": { "rotate": "-450" }"#)),
            270
        );
    }

    #[test]
    fn a_quarter_turn_swaps_the_reported_frame() {
        let json = |extra: &str| {
            format!(
                r#"{{ "streams": [ {{ "codec_type": "video", "codec_name": "h264",
                       "width": 1920, "height": 1080, "r_frame_rate": "30/1" {extra} }} ],
                     "format": {{ "duration": "10.0" }} }}"#
            )
        };
        let upright = parse_ffprobe_json(&json("")).unwrap();
        assert_eq!((upright.width, upright.height), (1920, 1080));

        // The portrait-iPhone shape: stored landscape, displayed portrait.
        let portrait =
            parse_ffprobe_json(&json(r#", "side_data_list": [{ "rotation": -90 }]"#)).unwrap();
        assert_eq!(
            (portrait.width, portrait.height),
            (1080, 1920),
            "a quarter turn must be reported as the DISPLAY frame — ffmpeg \
             auto-rotates on decode, so this is the size the graph will see"
        );

        // A half turn is not a transpose.
        let upside_down =
            parse_ffprobe_json(&json(r#", "side_data_list": [{ "rotation": 180 }]"#)).unwrap();
        assert_eq!((upside_down.width, upside_down.height), (1920, 1080));
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

    // ── Filmstrip tiles ────────────────────────────────────────────────────
    fn vf_of(args: &[String]) -> String {
        let i = args.iter().position(|a| a == "-vf").unwrap();
        args[i + 1].clone()
    }

    #[test]
    fn filmstrip_args_seek_duration_and_single_output() {
        let args = filmstrip_tile_args("/media/clip.mp4", 4_000, 8_000, 8, 72, "/out/t.jpg");
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        assert_eq!(args[ss + 1], "4.000");
        let t = args.iter().position(|a| a == "-t").unwrap();
        assert_eq!(args[t + 1], "4.000");
        // Input options must precede -i so ffmpeg only decodes the slice.
        let i = args.iter().position(|a| a == "-i").unwrap();
        assert!(ss < i && t < i);
        assert!(args.windows(2).any(|w| w[0] == "-frames:v" && w[1] == "1"));
        assert!(args.iter().any(|a| a == "-an"));
        assert!(args.iter().any(|a| a == "-y"));
        assert_eq!(args.last().unwrap(), "/out/t.jpg");
    }

    #[test]
    fn filmstrip_args_build_exact_rational_fps_and_tile() {
        // 8 frames across 4000 ms → 8000/4000 fps, tiled 8 wide, 1 tall.
        let args = filmstrip_tile_args("/m.mp4", 0, 4_000, 8, 72, "/o.jpg");
        assert_eq!(vf_of(&args), "fps=8000/4000,scale=-2:72,tile=8x1");
    }

    #[test]
    fn filmstrip_args_use_the_tile_grid_span() {
        use crate::services::tiles::{tile_range_ms, TILE_COLS_DEFAULT, TILE_HEIGHT_PX};
        let (s, e) = tile_range_ms(4, 3); // 4 s tiles at tier 4
        let args = filmstrip_tile_args("/m.mp4", s, e, TILE_COLS_DEFAULT, TILE_HEIGHT_PX, "/o.jpg");
        assert_eq!(vf_of(&args), "fps=8000/4000,scale=-2:72,tile=8x1");
    }

    #[test]
    fn filmstrip_args_clamp_negative_start_and_empty_range() {
        let args = filmstrip_tile_args("/m.mp4", -500, -500, 8, 72, "/o.jpg");
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        assert_eq!(args[ss + 1], "0.000");
        let t = args.iter().position(|a| a == "-t").unwrap();
        assert_eq!(args[t + 1], "0.001");
    }

    #[test]
    fn filmstrip_args_clamp_cols_and_height() {
        let low = filmstrip_tile_args("/m.mp4", 0, 1_000, 0, 1, "/o.jpg");
        assert_eq!(vf_of(&low), "fps=1000/1000,scale=-2:8,tile=1x1");
        let high = filmstrip_tile_args("/m.mp4", 0, 1_000, 9_999, 9_999, "/o.jpg");
        assert_eq!(vf_of(&high), "fps=64000/1000,scale=-2:512,tile=64x1");
    }

    #[test]
    fn filmstrip_args_force_even_height() {
        let args = filmstrip_tile_args("/m.mp4", 0, 1_000, 4, 73, "/o.jpg");
        assert_eq!(vf_of(&args), "fps=4000/1000,scale=-2:72,tile=4x1");
    }

    /// Real ffmpeg: render one tile from a synthetic source and prove the
    /// geometry — ONE image, `cols` frames wide, `TILE_HEIGHT_PX` tall.
    /// Synthesises its own input (`testsrc`), so no sample asset is needed.
    #[test]
    #[ignore = "needs ffmpeg/ffprobe on PATH"]
    fn filmstrip_tile_renders_a_single_strip_of_cols_frames() {
        use crate::services::tiles::{tile_range_ms, TILE_HEIGHT_PX};
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.mp4");
        let status = Command::new("ffmpeg")
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x180:rate=25:duration=10",
                "-pix_fmt",
                "yuv420p",
                "-y",
            ])
            .arg(&src)
            .status()
            .expect("spawn ffmpeg");
        assert!(status.success(), "could not synthesise the test source");

        // Tier 4 = 4 s tiles; take tile 1 → [4000..8000).
        let (start, end) = tile_range_ms(4, 1);
        let out = dir.path().join("tile.jpg");
        let cols = 8;
        extract_filmstrip_tile(
            &src.to_string_lossy(),
            start,
            end,
            cols,
            &out.to_string_lossy(),
        )
        .expect("filmstrip tile renders");
        assert!(out.exists(), "tile file written");

        let probe = Command::new("ffprobe")
            .args(["-v", "error", "-print_format", "json", "-show_streams"])
            .arg(&out)
            .output()
            .expect("spawn ffprobe");
        let meta = parse_ffprobe_json(&String::from_utf8_lossy(&probe.stdout))
            .expect("ffprobe json parses");
        assert_eq!(meta.height, TILE_HEIGHT_PX as i32, "one row, tile-tall");
        // 320x180 scaled to 72 tall → 128 wide (even), times 8 columns.
        assert_eq!(meta.width, 128 * cols as i32, "cols frames side by side");
    }

    /// The cache dir is created on demand, exactly like `extract_thumbnail`.
    #[test]
    #[ignore = "needs ffmpeg on PATH"]
    fn filmstrip_tile_creates_the_cache_directory() {
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.mp4");
        let status = Command::new("ffmpeg")
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x90:rate=25:duration=2",
                "-pix_fmt",
                "yuv420p",
                "-y",
            ])
            .arg(&src)
            .status()
            .expect("spawn ffmpeg");
        assert!(status.success());

        let out = dir.path().join("nested/filmstrip/tile.jpg");
        assert!(!out.parent().unwrap().exists());
        extract_filmstrip_tile(&src.to_string_lossy(), 0, 1_000, 4, &out.to_string_lossy())
            .expect("renders into a freshly created dir");
        assert!(out.exists());
    }

    #[test]
    fn relink_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let found = find_relink_candidate("deadbeef", &[dir.path().to_path_buf()], None).unwrap();
        assert_eq!(found, None);
    }

    // ── media_availability (pure) ──────────────────────────────────────────
    fn pair(id: &str, path: &str) -> (String, String) {
        (id.to_string(), path.to_string())
    }

    #[test]
    fn media_availability_reports_per_item() {
        let items = vec![
            pair("m1", "/here/a.mp4"),
            pair("m2", "/gone/b.mp4"),
            pair("m3", "/here/c.mov"),
        ];
        let got = media_availability(&items, |p| p.starts_with("/here/"));
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].media_id, "m1");
        assert_eq!(got[0].path, "/here/a.mp4");
        assert!(got[0].exists);
        assert!(!got[1].exists);
        assert!(got[2].exists);
    }

    #[test]
    fn media_availability_preserves_pool_order() {
        let items = vec![pair("z", "/z"), pair("a", "/a"), pair("m", "/m")];
        let got = media_availability(&items, |_| true);
        let ids: Vec<&str> = got.iter().map(|a| a.media_id.as_str()).collect();
        assert_eq!(ids, vec!["z", "a", "m"]);
    }

    #[test]
    fn media_availability_empty_pool_is_empty() {
        assert!(media_availability(&[], |_| true).is_empty());
    }

    #[test]
    fn media_availability_reports_duplicate_paths_independently() {
        // Two pool entries can legitimately point at the same file (imported
        // twice). Both rows must be present so the UI can relink each id.
        let items = vec![pair("m1", "/shared.mp4"), pair("m2", "/shared.mp4")];
        let got = media_availability(&items, |_| false);
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|a| !a.exists));
    }

    #[test]
    fn media_availability_matches_real_fs_through_the_command_predicate() {
        // The IO half the command wrapper supplies, pinned once against a
        // real temp dir so the injected predicate isn't a fiction.
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("present.mp4");
        std::fs::write(&present, b"x").unwrap();
        let missing = dir.path().join("missing.mp4");

        let items = vec![
            pair("m1", &present.to_string_lossy()),
            pair("m2", &missing.to_string_lossy()),
        ];
        let got = media_availability(&items, |p| Path::new(p).exists());
        assert!(got[0].exists);
        assert!(!got[1].exists);
    }
}
