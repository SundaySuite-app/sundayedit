//! SundayEdit — AI-assisted video captioning for desktop.
//!
//! Entry point. Wires up logging, registers all Tauri command handlers,
//! and runs the Tauri runtime. The actual work happens in `services::*`.

pub mod account;
pub mod commands;
pub mod error;
pub mod model;
pub mod services;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::asr::DownloadControl::default())
        .manage(commands::asr::TranscribeControl::default())
        .manage(commands::highlight_reel::ReelRenderControl::default())
        .manage(commands::compose::ComposeRenderControl::default());

    // Auto-update + relaunch + deep-link import are desktop-only.
    #[cfg(desktop)]
    {
        builder = builder
            .plugin(tauri_plugin_updater::Builder::new().build())
            .plugin(tauri_plugin_process::init())
            .plugin(tauri_plugin_deep_link::init());
    }

    builder
        // Sunday-link (Phase 8): forward inbound `sundayedit://import?…` URLs
        // to the renderer, which validates them via `deeplink_parse_import`
        // and drives the import. The URL scheme itself is registered by the
        // bundler from `tauri.conf.json` → plugins.deep-link.
        .setup(|app| {
            #[cfg(desktop)]
            {
                use tauri::Emitter;
                use tauri_plugin_deep_link::DeepLinkExt;
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        let _ = handle.emit("deep-link://import", url.to_string());
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Sunday Account (SSO) — shared cross-app session
            commands::account::sunday_account_status,
            commands::account::sunday_sign_out,
            // Caption operations (Phase 3.1)
            commands::operations::op_split_caption,
            commands::operations::op_merge_captions,
            commands::operations::op_shift_all_captions,
            commands::operations::op_move_caption,
            commands::operations::op_resize_caption,
            commands::operations::op_edit_word,
            commands::operations::op_lock_word,
            commands::operations::op_accept_alternate,
            commands::operations::op_retime_word,
            commands::operations::op_apply_glossary,
            // NLE timeline / clip-track operations (multi-track)
            commands::timeline_ops::op_import_media,
            commands::timeline_ops::op_remove_media,
            commands::timeline_ops::op_relink_media,
            commands::timeline_ops::op_add_track,
            commands::timeline_ops::op_remove_track,
            commands::timeline_ops::op_reorder_track,
            commands::timeline_ops::op_set_track_flags,
            commands::timeline_ops::op_add_timeline_item,
            commands::timeline_ops::op_split_timeline_item,
            commands::timeline_ops::op_duplicate_timeline_item,
            commands::timeline_ops::op_trim_timeline_item,
            commands::timeline_ops::op_move_timeline_item,
            commands::timeline_ops::op_ripple_delete_item,
            // Gap engine (E3) — detect/insert/remove/pack with protected gaps
            commands::timeline_ops::timeline_detect_gaps,
            commands::timeline_ops::op_insert_gap,
            commands::timeline_ops::op_remove_gap,
            commands::timeline_ops::op_pack_track,
            commands::timeline_ops::op_set_transition,
            commands::timeline_ops::op_clear_transition,
            commands::timeline_ops::op_set_transform,
            // Curated clip effects (E6) — the preview↔ffmpeg parity registry
            commands::timeline_ops::op_set_effect,
            commands::timeline_ops::op_set_item_audio,
            commands::timeline_ops::op_set_track_volume,
            commands::timeline_ops::op_remove_effect,
            commands::timeline_ops::op_add_text_item,
            // AI glossary suggestions (Phase 3.4 mode 3)
            commands::glossary::glossary_suggest_estimate,
            commands::glossary::glossary_suggest,
            // Glossary terms from a reference document (Phase 3.4 mode 4)
            commands::glossary::glossary_extract_document,
            commands::glossary::glossary_from_document_estimate,
            commands::glossary::glossary_from_document,
            // Export (Phase 6.1)
            commands::export::export_srt,
            commands::export::export_vtt,
            commands::export::export_ass,
            commands::export::export_txt,
            commands::export::export_json,
            commands::export::save_export,
            // Project lifecycle + video import (Phase 1)
            commands::project::video_probe,
            commands::project::extract_thumbnail,
            commands::project::extract_filmstrip_tile,
            commands::project::project_create_from_video,
            commands::project::project_save,
            commands::project::project_open,
            commands::project::waveform_compute,
            commands::project::extract_audio,
            commands::project::project_relink,
            commands::project::check_media_paths,
            commands::project::accepted_media_extensions,
            // ASR / transcription (Phase 2)
            commands::asr::asr_list_models,
            commands::asr::cloud_providers,
            commands::asr::cloud_cost_estimate,
            commands::asr::cloud_transcribe,
            commands::asr::asr_downloaded_models,
            commands::asr::asr_download_model,
            commands::asr::asr_cancel_download,
            commands::asr::asr_transcribe_local,
            commands::asr::asr_cancel_transcribe,
            // API key storage (Phase 2.2)
            commands::secrets::secret_set,
            commands::secrets::secret_delete,
            commands::secrets::secret_status,
            // Styling (Phase 5)
            commands::style::style_list_presets,
            // Burn-in + platform export (Phase 6.2 / 6.3)
            commands::render::export_list_presets,
            commands::render::export_validate,
            commands::render::burnin_render,
            commands::render::burnin_render_preset,
            // Find/replace + filler cleanup (Phase 7)
            commands::cleanup::find_in_project,
            commands::cleanup::replace_in_project,
            commands::cleanup::bulk_delete_captions,
            commands::cleanup::bulk_set_speaker,
            commands::cleanup::detect_fillers,
            commands::cleanup::detect_silences,
            commands::cleanup::apply_ripple_cuts,
            // Caption readability re-flow (Phase 7)
            commands::reflow::reflow_analyze,
            commands::reflow::reflow_repair,
            // AI punctuation polish (Phase 4.1)
            commands::polish::polish_estimate,
            commands::polish::polish_captions,
            // AI smart suggestions (Phase 4.3)
            commands::suggest::suggest_estimate,
            commands::suggest::suggest_captions,
            commands::suggest::apply_suggestion,
            // AI translation (Phase 7.1)
            commands::translate::translate_supported_languages,
            commands::translate::translate_estimate,
            commands::translate::translate_captions,
            // Speaker diarization (Phase 4.2)
            commands::diarize::diarize_run,
            commands::diarize::speaker_merge,
            commands::diarize::speaker_rename,
            commands::diarize::speaker_set_color,
            // AI social clips (SundayEdit)
            commands::clips::clips_estimate,
            commands::clips::clips_generate,
            commands::clips::clips_apply_plan,
            commands::render::clip_burnin_render,
            // Sermon Highlight Reel Studio (batch clip rendering)
            commands::highlight_reel::reel_storyboard,
            commands::highlight_reel::reel_build_plan,
            commands::highlight_reel::reel_render_all,
            commands::highlight_reel::reel_cancel_render,
            // Multi-track compose render engine (filter_complex pipeline)
            commands::compose::compose_render,
            commands::compose::compose_preview_proxy,
            commands::compose::compose_cancel,
            commands::compose::compose_default_encoder,
            // Sunday-link deep-link import (Phase 8)
            commands::deeplink::deeplink_parse_import,
            commands::deeplink::deeplink_captions_callback_url,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
