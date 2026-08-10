//! Project lifecycle Tauri commands — Phase 1.
//!
//! probe a video, create a project from it, open/save `.sundayedit` files,
//! compute the waveform, and relink a moved video.

use std::path::{Path, PathBuf};

use crate::error::{AppError, AppResult};
use crate::model::{ExportConfig, Project, ProjectMeta, Style};
use crate::services::video::VideoMetadata;
use crate::services::waveform::WaveformData;
use crate::services::{project_file, video, waveform};

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Probe a media file's metadata (ffprobe).
#[tauri::command]
pub fn video_probe(path: String) -> AppResult<VideoMetadata> {
    video::probe(Path::new(&path))
}

/// Grab a single-frame thumbnail from `media_path` at `at_ms`, scaled to 120px
/// tall, written to `out_path` (JPEG). Returns the written path. Used by the
/// timeline/media-bin clip previews.
#[tauri::command]
pub fn extract_thumbnail(media_path: String, at_ms: i64, out_path: String) -> AppResult<String> {
    video::extract_thumbnail(&media_path, at_ms, &out_path)
}

/// Render one filmstrip tile — `cols` frames from `[start_ms, end_ms)` tiled
/// into a single JPEG at `out_path`. Ranges come from the fixed per-zoom-tier
/// grid in `services::tiles` (`tiles_covering` / `tile_key`), so the rendered
/// files stay addressable and reusable across scroll and zoom.
#[tauri::command]
pub fn extract_filmstrip_tile(
    media_path: String,
    start_ms: i64,
    end_ms: i64,
    cols: u32,
    out_path: String,
) -> AppResult<String> {
    video::extract_filmstrip_tile(&media_path, start_ms, end_ms, cols, &out_path)
}

/// Create a fresh in-memory Project from a video file. Captions are empty
/// until the user transcribes (Phase 2). The video is hashed for path
/// stability and a sensible default style is applied. The multi-track shape
/// (media pool entry + Video/Caption tracks + the video placed as one
/// full-length clip) is backfilled immediately, so a fresh import lands
/// visible in the media bin, the lanes and the preview.
#[tauri::command]
pub fn project_create_from_video(path: String) -> AppResult<Project> {
    let p = Path::new(&path);
    let meta = video::probe(p)?;
    let hash = video::content_hash(p)?;
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Untitled")
        .to_string();
    let now = now_ms();

    let mut project = Project {
        id: new_id(),
        name,
        video_path: path.clone(),
        video_content_hash: hash,
        video_duration_ms: meta.duration_ms,
        video_width: meta.width,
        video_height: meta.height,
        video_fps: meta.fps,
        audio_wav_path: None, // set after extraction
        language: "auto".into(),
        default_style: Style::broadcast_news(),
        context_description: None,
        captions: vec![],
        speakers: vec![],
        glossary: vec![],
        clips: vec![],
        talk_summary: None,
        export_config: ExportConfig::default(),
        project_meta: ProjectMeta::default(),
        created_at: now,
        updated_at: now,
        media: vec![],
        tracks: vec![],
        timeline_items: vec![],
    };
    // Shared with the v<=3 load backfill (`project_file::load`). Audio
    // presence comes from the probe — the WAV extraction hasn't run yet.
    project.backfill_default_timeline(meta.audio_codec.is_some());
    Ok(project)
}

#[tauri::command]
pub async fn project_save(project: Project, path: String) -> AppResult<()> {
    project_file::save(&project, Path::new(&path)).await
}

#[tauri::command]
pub async fn project_open(path: String) -> AppResult<Project> {
    let project = project_file::load(Path::new(&path)).await?;
    // Verify the source video still exists; if not, the renderer shows
    // the relink UI keyed off the `video_missing` error.
    if !Path::new(&project.video_path).exists() {
        return Err(AppError::VideoMissing(project.video_path.clone()));
    }
    Ok(project)
}

/// Extract audio + compute multi-zoom waveform peaks for a video.
/// Writes the WAV to `cache_dir` (typically the app's project cache).
#[tauri::command]
pub fn waveform_compute(video_path: String, cache_dir: String) -> AppResult<WaveformData> {
    let input = Path::new(&video_path);
    let wav = Path::new(&cache_dir).join(format!("{}.wav", video::content_hash(input)?,));
    if !wav.exists() {
        waveform::extract_audio_wav(input, &wav)?;
    }
    let (samples, sample_rate) = waveform::read_wav_samples(&wav)?;
    // 800 base buckets ≈ a typical editor width; ×4 per finer level, 5 levels.
    Ok(waveform::compute_levels(&samples, sample_rate, 800, 4, 5))
}

/// Extract the project's audio to a 16 kHz mono WAV and return its path.
///
/// Local Whisper (and diarization) need a real WAV — the source video isn't
/// enough. The WAV is written to `cache_dir/{hash}.wav`, the SAME scheme
/// `waveform_compute` uses, so the two share one cached extraction. No-ops if
/// the WAV is already on disk. The renderer stores the returned path on the
/// project (`audio_wav_path`) so later steps reuse it.
#[tauri::command]
pub fn extract_audio(video_path: String, cache_dir: String) -> AppResult<String> {
    let input = Path::new(&video_path);
    let wav = Path::new(&cache_dir).join(format!("{}.wav", video::content_hash(input)?));
    if !wav.exists() {
        waveform::extract_audio_wav(input, &wav)?;
    }
    Ok(wav.to_string_lossy().to_string())
}

/// Try to relink a project whose video moved. Searches the provided dirs.
#[tauri::command]
pub fn project_relink(
    target_hash: String,
    search_dirs: Vec<String>,
    original_filename: Option<String>,
) -> AppResult<Option<String>> {
    let dirs: Vec<PathBuf> = search_dirs.into_iter().map(PathBuf::from).collect();
    let found = video::find_relink_candidate(&target_hash, &dirs, original_filename.as_deref())?;
    Ok(found.map(|p| p.to_string_lossy().to_string()))
}

/// The accepted file extensions — used by the renderer to build the
/// native open-file dialog filter.
#[tauri::command]
pub fn accepted_media_extensions() -> Vec<String> {
    video::accepted_extensions()
        .into_iter()
        .map(String::from)
        .collect()
}
