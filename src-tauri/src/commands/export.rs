//! Export Tauri commands. The renderer passes the current project
//! state in — Rust generates the formatted string.

use crate::error::{AppError, AppResult};
use crate::model::Project;
use crate::services::export::{
    build_docx, project_karaoke, write_ass_with, write_json, write_srt, write_txt, write_vtt,
    JsonOptions, SrtOptions, TxtOptions, VttOptions,
};
use crate::services::karaoke::KaraokeOptions;

/// Karaoke options for an ASS write: an explicit per-call override when the UI
/// passes one (live style preview), otherwise the project's persisted setting —
/// which is what `burnin`/`compose` read, so the sidecar and the burned frame
/// agree by construction.
fn karaoke_for(project: &Project, override_opts: Option<KaraokeOptions>) -> KaraokeOptions {
    override_opts.unwrap_or_else(|| project_karaoke(project))
}

#[tauri::command]
pub fn export_srt(
    project: Project,
    include_speakers: bool,
    strip_empty: bool,
) -> AppResult<String> {
    Ok(write_srt(
        &project,
        SrtOptions {
            include_speakers,
            strip_empty,
        },
    ))
}

#[tauri::command]
pub fn export_vtt(
    project: Project,
    include_speakers: bool,
    strip_empty: bool,
) -> AppResult<String> {
    Ok(write_vtt(
        &project,
        VttOptions {
            include_speakers,
            strip_empty,
        },
    ))
}

/// `karaoke` is optional — omit it (the existing renderer call shape) to use
/// the project's persisted `export_config.karaoke`.
#[tauri::command]
pub fn export_ass(project: Project, karaoke: Option<KaraokeOptions>) -> AppResult<String> {
    let k = karaoke_for(&project, karaoke);
    Ok(write_ass_with(&project, &k))
}

#[tauri::command]
pub fn export_json(project: Project, strip_empty: bool) -> AppResult<String> {
    Ok(write_json(&project, JsonOptions { strip_empty }))
}

/// Regenerate `format` server-side and write it to `path` (chosen by the OS
/// save dialog). One command for every format so the renderer never has to
/// handle file bytes — and DOCX (binary) is covered too.
#[tauri::command]
pub fn save_export(
    project: Project,
    path: String,
    format: String,
    include_speakers: bool,
    strip_empty: bool,
    karaoke: Option<KaraokeOptions>,
) -> AppResult<()> {
    let bytes: Vec<u8> = match format.as_str() {
        "srt" => write_srt(
            &project,
            SrtOptions {
                include_speakers,
                strip_empty,
            },
        )
        .into_bytes(),
        "vtt" => write_vtt(
            &project,
            VttOptions {
                include_speakers,
                strip_empty,
            },
        )
        .into_bytes(),
        "ass" => write_ass_with(&project, &karaoke_for(&project, karaoke)).into_bytes(),
        "txt" => write_txt(
            &project,
            TxtOptions {
                include_speakers,
                strip_empty,
            },
        )
        .into_bytes(),
        "json" => write_json(&project, JsonOptions { strip_empty }).into_bytes(),
        "docx" => build_docx(
            &project,
            TxtOptions {
                include_speakers,
                strip_empty,
            },
        )?,
        other => {
            return Err(AppError::Validation(format!(
                "unknown export format: {other}"
            )))
        }
    };
    std::fs::write(&path, bytes)?;
    Ok(())
}

#[tauri::command]
pub fn export_txt(
    project: Project,
    include_speakers: bool,
    strip_empty: bool,
) -> AppResult<String> {
    Ok(write_txt(
        &project,
        TxtOptions {
            include_speakers,
            strip_empty,
        },
    ))
}
