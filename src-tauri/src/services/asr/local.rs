//! Local Whisper provider — Phase 2.1.
//!
//! Real transcription runs on-device via `whisper-rs` (whisper.cpp
//! bindings). Building whisper.cpp needs a C/C++ toolchain + cmake and
//! pulls in a multi-GB model at runtime, so it sits behind the optional
//! `whisper` cargo feature:
//!
//!   cargo build --features whisper      # real local transcription
//!   cargo build                         # everything else; ASR stubbed
//!
//! The default build (and the test suite) compile WITHOUT building
//! whisper.cpp. The stub returns a clear, actionable error so the UI can
//! tell the user "this build doesn't include local transcription —
//! rebuild with --features whisper, or use a cloud provider."
//!
//! GPU acceleration (Metal on macOS, CUDA on Windows) is selected by
//! whisper-rs's build features when the `whisper` feature is on.

use std::path::{Path, PathBuf};

use crate::error::AppResult;
use crate::services::asr::model::WhisperModel;
use crate::services::asr::{AsrOptions, AsrProvider, TranscribeProgress, Transcript};

pub struct LocalWhisperProvider {
    pub model: WhisperModel,
    pub model_path: PathBuf,
}

impl LocalWhisperProvider {
    pub fn new(model: WhisperModel, models_dir: &Path) -> Self {
        Self {
            model,
            model_path: model.path_in(models_dir),
        }
    }
}

// ── Real implementation (feature = "whisper") ────────────────────────────────
// HARDWARE-UNVERIFIED: building whisper.cpp + running a real model on a real WAV
// needs `--features whisper`, a downloaded model, and a device (P2c — see
// docs/NEEDS-RICHARD.md). The pieces it leans on — `AsrOptions::initial_prompt`,
// `word_confidence_from_token_logprobs`, `read_wav_samples` — are unit-tested;
// only the whisper-rs invocation itself is unverified.
#[cfg(feature = "whisper")]
impl AsrProvider for LocalWhisperProvider {
    fn name(&self) -> String {
        format!("Local · {}", self.model.filename())
    }

    fn transcribe(
        &self,
        audio_path: &Path,
        opts: &AsrOptions,
        progress: std::sync::Arc<dyn Fn(TranscribeProgress) + Send + Sync>,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> AppResult<Transcript> {
        use crate::error::AppError;
        use crate::services::asr::confidence::word_confidence_from_token_logprobs;
        use crate::services::asr::{Segment, TranscribedWord};
        use crate::services::waveform::read_wav_samples;
        use std::sync::atomic::Ordering;
        use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

        progress(TranscribeProgress::Preparing);

        if !self.model_path.is_file() {
            return Err(AppError::Internal(format!(
                "Whisper model not found at {}. Download it first.",
                self.model_path.display()
            )));
        }

        let ctx =
            WhisperContext::new_with_params(&self.model_path, WhisperContextParameters::default())
                .map_err(|e| AppError::Internal(format!("failed to load Whisper model: {e}")))?;

        let (samples, _sr) = read_wav_samples(audio_path)?;

        let mut params = FullParams::new(SamplingStrategy::BeamSearch {
            beam_size: opts.beam_size,
            patience: 1.0,
        });
        params.set_token_timestamps(true);
        params.set_max_len(1); // word-level segments
        params.set_split_on_word(true);
        if opts.language != "auto" {
            params.set_language(Some(&opts.language));
        }
        if let Some(prompt) = opts.initial_prompt() {
            params.set_initial_prompt(&prompt);
        }
        // Stream inference progress (whisper reports whole percent between
        // encoder steps) — this is the long phase, and without it the UI sat on
        // "preparing" with no feedback for the entire run.
        let progress_cb = progress.clone();
        params.set_progress_callback_safe(move |pct: i32| {
            progress_cb(TranscribeProgress::Running {
                fraction: (pct.clamp(0, 100) as f32) / 100.0,
            });
        });
        // UPSTREAM BUG (whisper-rs ≤0.16): set_abort_callback_safe's C
        // trampoline is instantiated with the caller's closure type F, but
        // user_data points at a `Box<dyn FnMut() -> bool>` — for any other F
        // the cast is UB and the garbage return aborts EVERY run ("failed to
        // encode", code -6). Passing an already-boxed dyn closure makes F ==
        // Box<dyn FnMut() -> bool>, so the trampoline's cast is exact. Don't
        // "simplify" this back to a bare closure.
        let abort_cancel = cancel.clone();
        let abort_cb: Box<dyn FnMut() -> bool> =
            Box::new(move || abort_cancel.load(Ordering::Relaxed));
        params.set_abort_callback_safe(abort_cb);

        let mut state = ctx
            .create_state()
            .map_err(|e| AppError::Internal(format!("whisper state: {e}")))?;
        if let Err(e) = state.full(params, &samples) {
            // An abort surfaces as a generic inference error — report the
            // user's cancel as a recognisable "cancelled" instead.
            if cancel.load(Ordering::Relaxed) {
                return Err(AppError::Internal("transcription cancelled".into()));
            }
            return Err(AppError::Internal(format!("whisper transcribe: {e}")));
        }

        let n_segments = state.full_n_segments();

        let mut words: Vec<TranscribedWord> = Vec::new();
        for i in 0..n_segments {
            let seg = state
                .get_segment(i)
                .ok_or_else(|| AppError::Internal(format!("whisper segment {i} out of bounds")))?;
            let text = seg
                .to_str_lossy()
                .map_err(|e| AppError::Internal(format!("segment text: {e}")))?
                .into_owned();
            let t0 = seg.start_timestamp(); // centiseconds
            let t1 = seg.end_timestamp();

            // Average token logprobs across the segment's tokens.
            let n_tokens = seg.n_tokens();
            let mut logprobs = Vec::new();
            for j in 0..n_tokens {
                if let Some(token) = seg.get_token(j) {
                    // whisper-rs exposes token probability (0..1); convert to logprob
                    logprobs.push(token.token_probability().max(1e-6).ln());
                }
            }
            let confidence = word_confidence_from_token_logprobs(&logprobs);

            let text = text.trim().to_string();
            if text.is_empty() {
                continue;
            }
            words.push(TranscribedWord {
                text,
                start_ms: t0 * 10, // cs → ms
                end_ms: t1 * 10,
                confidence,
            });

            let frac = (i + 1) as f32 / n_segments as f32;
            progress(TranscribeProgress::Segment {
                fraction: frac,
                segment: Segment {
                    start_ms: t0 * 10,
                    end_ms: t1 * 10,
                    words: words.last().cloned().into_iter().collect(),
                },
            });
        }

        progress(TranscribeProgress::Done);

        let language = if opts.language == "auto" {
            "auto".to_string()
        } else {
            opts.language.clone()
        };
        Ok(Transcript {
            language,
            backend: self.name(),
            segments: vec![Segment {
                start_ms: words.first().map(|w| w.start_ms).unwrap_or(0),
                end_ms: words.last().map(|w| w.end_ms).unwrap_or(0),
                words,
            }],
        })
    }
}

// ── Stub implementation (default build, no whisper.cpp) ───────────────────────
#[cfg(not(feature = "whisper"))]
impl AsrProvider for LocalWhisperProvider {
    fn name(&self) -> String {
        format!("Local · {} (unavailable)", self.model.filename())
    }

    fn transcribe(
        &self,
        _audio_path: &Path,
        _opts: &AsrOptions,
        _progress: std::sync::Arc<dyn Fn(TranscribeProgress) + Send + Sync>,
        _cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> AppResult<Transcript> {
        Err(crate::error::AppError::Internal(
            "This build of SundayEdit does not include local transcription. \
             Rebuild with `--features whisper`, or configure a cloud provider in Settings."
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name_includes_model() {
        let dir = std::path::Path::new("/tmp/models");
        let p = LocalWhisperProvider::new(WhisperModel::LargeV3Turbo, dir);
        assert!(p.name().contains("large-v3-turbo"));
    }

    #[test]
    fn model_path_resolves_under_dir() {
        let dir = std::path::Path::new("/data/models");
        let p = LocalWhisperProvider::new(WhisperModel::Base, dir);
        assert_eq!(p.model_path, dir.join("ggml-base.bin"));
    }

    /// Live end-to-end inference — needs a real model + 16 kHz mono WAV, so
    /// it's `#[ignore]`d (run explicitly). Drives the EXACT production path:
    /// model load → Metal inference → word-level segments + confidences.
    ///
    /// ```sh
    /// SUNDAYEDIT_TEST_MODELS_DIR="$HOME/Library/Application Support/app.sundayedit/models" \
    /// SUNDAYEDIT_TEST_INPUT=/tmp/wtest.wav \
    /// SUNDAYEDIT_TEST_MODEL=large-v3-turbo \
    /// cargo test --features whisper live_transcribe -- --ignored --nocapture
    /// ```
    #[cfg(feature = "whisper")]
    #[test]
    #[ignore = "needs a downloaded model + 16 kHz mono WAV on the machine"]
    fn live_transcribe_runs_inference_and_reports_progress() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let models_dir = std::path::PathBuf::from(
            std::env::var("SUNDAYEDIT_TEST_MODELS_DIR").expect("set SUNDAYEDIT_TEST_MODELS_DIR"),
        );
        let input = std::env::var("SUNDAYEDIT_TEST_INPUT").expect("set SUNDAYEDIT_TEST_INPUT");
        let model = match std::env::var("SUNDAYEDIT_TEST_MODEL").as_deref() {
            Ok("large-v3-turbo") => WhisperModel::LargeV3Turbo,
            Ok("medium") => WhisperModel::Medium,
            _ => WhisperModel::Base,
        };

        let p = LocalWhisperProvider::new(model, &models_dir);
        let saw_running = Arc::new(AtomicBool::new(false));
        let saw_in_cb = saw_running.clone();
        let transcript = p
            .transcribe(
                Path::new(&input),
                &AsrOptions::default(),
                Arc::new(move |prog: TranscribeProgress| {
                    if let TranscribeProgress::Running { fraction } = prog {
                        println!("running fraction = {fraction}");
                        saw_in_cb.store(true, Ordering::Relaxed);
                    }
                }),
                Arc::new(AtomicBool::new(false)),
            )
            .expect("live transcription succeeds");
        let words: Vec<_> = transcript
            .segments
            .iter()
            .flat_map(|s| s.words.iter().map(|w| w.text.as_str()))
            .collect();
        println!("words: {words:?}");
        assert!(!words.is_empty(), "expected at least one word");
        assert!(
            saw_running.load(Ordering::Relaxed),
            "whisper progress callback never fired"
        );
    }

    /// Live cancel — a pre-raised flag must abort inference and surface the
    /// "cancelled" error the renderer suppresses, not a generic failure.
    #[cfg(feature = "whisper")]
    #[test]
    #[ignore = "needs a downloaded model + 16 kHz mono WAV on the machine"]
    fn live_transcribe_cancel_aborts_with_cancelled() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        let models_dir = std::path::PathBuf::from(
            std::env::var("SUNDAYEDIT_TEST_MODELS_DIR").expect("set SUNDAYEDIT_TEST_MODELS_DIR"),
        );
        let input = std::env::var("SUNDAYEDIT_TEST_INPUT").expect("set SUNDAYEDIT_TEST_INPUT");
        let model = match std::env::var("SUNDAYEDIT_TEST_MODEL").as_deref() {
            Ok("large-v3-turbo") => WhisperModel::LargeV3Turbo,
            Ok("medium") => WhisperModel::Medium,
            _ => WhisperModel::Base,
        };

        let p = LocalWhisperProvider::new(model, &models_dir);
        let err = p
            .transcribe(
                Path::new(&input),
                &AsrOptions::default(),
                Arc::new(|_p: TranscribeProgress| {}),
                Arc::new(AtomicBool::new(true)),
            )
            .expect_err("pre-cancelled transcription must error");
        assert!(
            err.to_string().contains("cancelled"),
            "expected cancelled, got: {err}"
        );
    }

    // Without the `whisper` feature, transcribe returns a clear error
    // rather than panicking — the UI relies on this to guide the user.
    #[cfg(not(feature = "whisper"))]
    #[test]
    fn stub_returns_actionable_error() {
        let dir = std::path::Path::new("/tmp/models");
        let p = LocalWhisperProvider::new(WhisperModel::Base, dir);
        let err = p
            .transcribe(
                Path::new("/tmp/x.wav"),
                &AsrOptions::default(),
                std::sync::Arc::new(|_p: TranscribeProgress| {}),
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .unwrap_err();
        assert_eq!(err.code(), "internal");
        assert!(err.to_string().contains("--features whisper"));
    }
}
