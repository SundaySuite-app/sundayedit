//! ASR Tauri commands — Phase 2.
//!
//! Lists models, reports which are downloaded, and runs transcription.
//! Transcription converts the resulting Transcript → Caption[] via the
//! pure captionizer, so the renderer gets editor-ready captions with
//! confidence already populated.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::Emitter;

use crate::error::AppResult;
use crate::model::{Caption, Project};
use crate::services::asr::captionize::{CaptionizeOptions, captionize};
use crate::services::asr::cloud::{self, CloudCostEstimate, CloudProvider, CloudProviderInfo};
use crate::services::asr::download::download_model;
use crate::services::asr::local::LocalWhisperProvider;
use crate::services::asr::model::{WhisperModel, WhisperModelInfo, catalog};
use crate::services::asr::{AsrOptions, AsrProvider, TranscribeProgress};
use crate::services::secrets::{self, SecretProvider};

/// Which keychain entry + env var holds a cloud provider's key.
fn secret_provider_for(p: CloudProvider) -> SecretProvider {
    match p {
        CloudProvider::OpenaiWhisper => SecretProvider::OpenAi,
        CloudProvider::AssemblyAi => SecretProvider::AssemblyAi,
        CloudProvider::Deepgram => SecretProvider::Deepgram,
    }
}

fn env_var_for(p: CloudProvider) -> &'static str {
    match p {
        CloudProvider::OpenaiWhisper => "OPENAI_API_KEY",
        CloudProvider::AssemblyAi => "ASSEMBLYAI_API_KEY",
        CloudProvider::Deepgram => "DEEPGRAM_API_KEY",
    }
}

/// Shared cancel flag for the in-flight model download (one at a time —
/// the picker only downloads one model). Managed by the Tauri runtime.
#[derive(Default)]
pub struct DownloadControl {
    cancel: Arc<AtomicBool>,
}

/// Shared cancel flag for the in-flight local transcription (one at a time —
/// the transcribe panel runs a single job). Polled by whisper's abort callback
/// between encoder steps. Managed by the Tauri runtime.
#[derive(Default)]
pub struct TranscribeControl {
    cancel: Arc<AtomicBool>,
}

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

/// The model catalog for the picker.
#[tauri::command]
pub fn asr_list_models() -> Vec<WhisperModelInfo> {
    catalog()
}

/// The cloud-provider catalog for the picker (names, price/min, privacy URL).
#[tauri::command]
pub fn cloud_providers() -> Vec<CloudProviderInfo> {
    cloud::catalog()
}

/// Pre-submit cost preview for a cloud provider over `duration_ms` of audio.
#[tauri::command]
pub fn cloud_cost_estimate(provider: CloudProvider, duration_ms: i64) -> CloudCostEstimate {
    cloud::estimate_cost(provider, duration_ms)
}

/// Transcribe the project's audio via a cloud provider (BYOK — key from the
/// keychain). Uploads the extracted 16 kHz WAV when present (small, under the
/// API size limit), else the source media. Returns editor-ready captions.
#[tauri::command]
pub async fn cloud_transcribe(
    project: Project,
    provider: CloudProvider,
    api_key: Option<String>,
    language: Option<String>,
) -> AppResult<Vec<Caption>> {
    // Prefer the small extracted WAV when it's actually on disk; the choice
    // rule itself lives in (tested) `cloud::select_upload_source`.
    let wav_exists = project
        .audio_wav_path
        .as_deref()
        .is_some_and(|p| Path::new(p).is_file());
    let path = cloud::select_upload_source(
        project.audio_wav_path.as_deref(),
        project.video_path.as_str(),
        wav_exists,
    )
    .to_string();

    let key = secrets::resolve(
        api_key,
        secret_provider_for(provider),
        env_var_for(provider),
    );
    let lang = language.unwrap_or_else(|| project.language.clone());

    let transcript = cloud::cloud_transcribe(provider, Path::new(&path), &key, &lang).await?;
    Ok(captionize(
        &transcript,
        CaptionizeOptions::default(),
        now_ms(),
        |_idx| new_id(),
    ))
}

/// Which models are already downloaded into `models_dir`.
#[tauri::command]
pub fn asr_downloaded_models(models_dir: String) -> Vec<WhisperModel> {
    let dir = Path::new(&models_dir);
    WhisperModel::all()
        .into_iter()
        .filter(|m| m.is_downloaded(dir))
        .collect()
}

/// Download `model` into `models_dir`, streaming `model-download-progress`
/// events to the window. Resolves when the model is on disk (or no-ops if it
/// already was). Cancellable via `asr_cancel_download`.
#[tauri::command]
pub async fn asr_download_model(
    window: tauri::Window,
    control: tauri::State<'_, DownloadControl>,
    models_dir: String,
    model: WhisperModel,
) -> AppResult<()> {
    let cancel = control.cancel.clone();
    cancel.store(false, Ordering::Relaxed);
    download_model(model, Path::new(&models_dir), &cancel, |p| {
        let _ = window.emit("model-download-progress", &p);
    })
    .await?;
    Ok(())
}

/// Cancel the in-flight model download, if any.
#[tauri::command]
pub fn asr_cancel_download(control: tauri::State<'_, DownloadControl>) {
    control.cancel.store(true, Ordering::Relaxed);
}

/// Transcribe an audio WAV with the local model and return editor-ready
/// captions. Streams `transcribe-progress` events to the window (including
/// `running` fractions from whisper's own progress callback while inference
/// grinds). Async + spawn_blocking: a sync command runs on the MAIN thread,
/// which froze the entire UI for the whole transcription. Cancellable via
/// `asr_cancel_transcribe`.
///
/// Without the `whisper` feature the provider returns a clear error that
/// the renderer surfaces ("rebuild with --features whisper or use cloud").
#[tauri::command]
pub async fn asr_transcribe_local(
    window: tauri::Window,
    control: tauri::State<'_, TranscribeControl>,
    audio_path: String,
    models_dir: String,
    model: WhisperModel,
    options: AsrOptions,
) -> AppResult<Vec<Caption>> {
    let cancel = control.cancel.clone();
    cancel.store(false, Ordering::Relaxed);

    let transcript = tokio::task::spawn_blocking(move || {
        let provider = LocalWhisperProvider::new(model, Path::new(&models_dir));
        let on_progress: Arc<dyn Fn(TranscribeProgress) + Send + Sync> =
            Arc::new(move |p: TranscribeProgress| {
                let _ = window.emit("transcribe-progress", &p);
            });
        provider.transcribe(Path::new(&audio_path), &options, on_progress, cancel)
    })
    .await
    .map_err(|e| crate::error::AppError::Internal(format!("transcribe task join: {e}")))??;

    let mut counter = 0usize;
    let captions = captionize(
        &transcript,
        CaptionizeOptions::default(),
        now_ms(),
        |_idx| {
            counter += 1;
            new_id()
        },
    );
    Ok(captions)
}

/// Cancel the in-flight local transcription, if any. The abort callback picks
/// the flag up between encoder steps, so the run stops within a step or two.
#[tauri::command]
pub fn asr_cancel_transcribe(control: tauri::State<'_, TranscribeControl>) {
    control.cancel.store(true, Ordering::Relaxed);
}
