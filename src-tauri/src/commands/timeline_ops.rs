//! NLE timeline Tauri commands. The pure operations live in
//! `services::timeline_ops`; these wrappers just shuttle data across IPC and
//! generate new entity ids at the command layer (same pattern as
//! `commands::operations`).

use std::path::Path;

use crate::error::AppResult;
use crate::model::{MediaItem, Project, TimelineItemKind, TrackKind, Transform};
use crate::services::timeline_ops::Gap;
use crate::services::{timeline_ops, video};

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

// ── media pool ─────────────────────────────────────────────────────────────────

/// Import a media file into the project's pool: probe it (ffprobe) + hash it
/// for path-stable identity, build a `MediaItem`, then append it.
#[tauri::command]
pub fn op_import_media(project: Project, path: String) -> AppResult<Project> {
    let p = Path::new(&path);
    let meta = video::probe(p)?;
    let hash = video::content_hash(p)?;
    let original_filename = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Untitled")
        .to_string();

    let media = MediaItem {
        id: new_id(),
        path: path.clone(),
        content_hash: hash,
        kind: meta.kind,
        duration_ms: meta.duration_ms,
        width: meta.width,
        height: meta.height,
        fps: meta.fps,
        has_audio: meta.audio_codec.is_some(),
        audio_wav_path: None,
        original_filename,
        added_at: now_ms(),
    };
    timeline_ops::add_media(&project, media)
}

#[tauri::command]
pub fn op_remove_media(project: Project, media_id: String) -> AppResult<Project> {
    timeline_ops::remove_media(&project, &media_id)
}

/// Repoint a pool entry at a file that moved or was renamed: re-probe and
/// re-hash `new_path`, keep the media **id** so every clip stays attached,
/// clamp any clip the new (possibly shorter) duration no longer fits, and
/// carry the legacy primary-video scalars when this was the primary.
///
/// The renderer calls `project_relink` first (auto-search by content hash)
/// and falls back to a file dialog; either way the chosen path lands here.
#[tauri::command]
pub fn op_relink_media(project: Project, media_id: String, new_path: String) -> AppResult<Project> {
    timeline_ops::relink_media(&project, &media_id, &new_path)
}

// ── tracks ─────────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn op_add_track(project: Project, kind: TrackKind, name: String) -> AppResult<Project> {
    timeline_ops::add_track(&project, new_id(), kind, name)
}

#[tauri::command]
pub fn op_remove_track(project: Project, track_id: String) -> AppResult<Project> {
    timeline_ops::remove_track(&project, &track_id)
}

#[tauri::command]
pub fn op_reorder_track(project: Project, track_id: String, new_index: i32) -> AppResult<Project> {
    timeline_ops::reorder_track(&project, &track_id, new_index)
}

#[tauri::command]
pub fn op_set_track_flags(
    project: Project,
    track_id: String,
    enabled: Option<bool>,
    locked: Option<bool>,
    muted: Option<bool>,
    solo: Option<bool>,
) -> AppResult<Project> {
    timeline_ops::set_track_flags(&project, &track_id, enabled, locked, muted, solo)
}

// ── timeline items ─────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn op_add_timeline_item(
    project: Project,
    track_id: String,
    source_media_id: Option<String>,
    in_ms: i64,
    out_ms: i64,
    timeline_start_ms: i64,
    kind: TimelineItemKind,
) -> AppResult<Project> {
    timeline_ops::add_timeline_item(
        &project,
        new_id(),
        &track_id,
        source_media_id,
        in_ms,
        out_ms,
        timeline_start_ms,
        kind,
    )
}

#[tauri::command]
pub fn op_split_timeline_item(
    project: Project,
    item_id: String,
    at_timeline_ms: i64,
) -> AppResult<Project> {
    timeline_ops::split_timeline_item(&project, &item_id, at_timeline_ms, new_id())
}

#[tauri::command]
pub fn op_trim_timeline_item(
    project: Project,
    item_id: String,
    new_in_ms: Option<i64>,
    new_out_ms: Option<i64>,
    new_timeline_start_ms: Option<i64>,
) -> AppResult<Project> {
    timeline_ops::trim_timeline_item(
        &project,
        &item_id,
        new_in_ms,
        new_out_ms,
        new_timeline_start_ms,
    )
}

#[tauri::command]
pub fn op_move_timeline_item(
    project: Project,
    item_id: String,
    new_track_id: String,
    new_timeline_start_ms: i64,
) -> AppResult<Project> {
    timeline_ops::move_timeline_item(&project, &item_id, &new_track_id, new_timeline_start_ms)
}

#[tauri::command]
pub fn op_ripple_delete_item(project: Project, item_id: String) -> AppResult<Project> {
    timeline_ops::ripple_delete_item(&project, &item_id)
}

// ── gap engine ─────────────────────────────────────────────────────────────────

/// Query the empty stretches on one track. A gap is flagged `protected` when
/// the clip after it is locked — ripples stop there. Read-only: no Project
/// comes back, so the caller does not touch undo history.
#[tauri::command]
pub fn timeline_detect_gaps(project: Project, track_id: String) -> AppResult<Vec<Gap>> {
    timeline_ops::detect_gaps(&project, &track_id)
}

#[tauri::command]
pub fn op_insert_gap(
    project: Project,
    track_id: String,
    at_ms: i64,
    duration_ms: i64,
) -> AppResult<Project> {
    timeline_ops::insert_gap_with_ripple(&project, &track_id, at_ms, duration_ms)
}

#[tauri::command]
pub fn op_remove_gap(project: Project, track_id: String, at_ms: i64) -> AppResult<Project> {
    timeline_ops::remove_gap_with_ripple(&project, &track_id, at_ms)
}

#[tauri::command]
pub fn op_pack_track(project: Project, track_id: String) -> AppResult<Project> {
    timeline_ops::pack_track(&project, &track_id)
}

// ── transitions / transform ─────────────────────────────────────────────────────

#[tauri::command]
pub fn op_set_transition(
    project: Project,
    item_id: String,
    kind: String,
    duration_ms: i64,
) -> AppResult<Project> {
    timeline_ops::set_transition(&project, &item_id, kind, duration_ms)
}

#[tauri::command]
pub fn op_clear_transition(project: Project, item_id: String) -> AppResult<Project> {
    timeline_ops::clear_transition(&project, &item_id)
}

#[tauri::command]
pub fn op_set_transform(
    project: Project,
    item_id: String,
    transform: Transform,
) -> AppResult<Project> {
    timeline_ops::set_transform(&project, &item_id, transform)
}

/// Add or update ONE curated colour effect on a clip. A non-curated `kind` is
/// rejected — the registry is the preview↔export parity contract (E6).
#[tauri::command]
pub fn op_set_effect(
    project: Project,
    item_id: String,
    kind: String,
    params: serde_json::Value,
    enabled: bool,
) -> AppResult<Project> {
    timeline_ops::set_effect(&project, &item_id, &kind, &params, enabled)
}

/// Set any subset of a clip's audio parameters (gain / fade-in / fade-out).
/// Omitted fields are left unchanged; everything is clamped server-side.
#[tauri::command]
pub fn op_set_item_audio(
    project: Project,
    item_id: String,
    gain_db: Option<f32>,
    fade_in_ms: Option<i64>,
    fade_out_ms: Option<i64>,
) -> AppResult<Project> {
    timeline_ops::set_item_audio(&project, &item_id, gain_db, fade_in_ms, fade_out_ms)
}

/// Set a track's fader, in dB. Clamped to [-60, +12] server-side.
#[tauri::command]
pub fn op_set_track_volume(
    project: Project,
    track_id: String,
    volume_db: f32,
) -> AppResult<Project> {
    timeline_ops::set_track_volume(&project, &track_id, volume_db)
}

/// Remove a clip's effect of the given kind (no-op when absent).
#[tauri::command]
pub fn op_remove_effect(project: Project, item_id: String, kind: String) -> AppResult<Project> {
    timeline_ops::remove_effect(&project, &item_id, &kind)
}

#[tauri::command]
pub fn op_add_text_item(
    project: Project,
    track_id: String,
    timeline_start_ms: i64,
    duration_ms: i64,
    text: String,
) -> AppResult<Project> {
    timeline_ops::add_text_item(
        &project,
        new_id(),
        &track_id,
        timeline_start_ms,
        duration_ms,
        text,
    )
}
