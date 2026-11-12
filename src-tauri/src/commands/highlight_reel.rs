//! Sermon Highlight Reel Studio Tauri commands.
//!
//! The operator-facing flow:
//!   - `reel_storyboard` — propose a clip storyboard. Async. Uses the AI clip
//!     planner when an Anthropic key is resolvable; otherwise FALLS BACK to the
//!     pure caption-gap heuristic so the feature works fully offline / keyless.
//!     Either way it returns a reviewable `ReelStoryboard` — nothing renders.
//!   - `reel_build_plan` — pure. Fan out the (operator-reviewed) clips × the
//!     chosen platforms into a confirmable `RenderPlan`. No IO.
//!   - `reel_render_all` — async. Walk the render plan and burn each clip ×
//!     platform to a vertical MP4, streaming `reel-render-progress` events and
//!     honouring a shared cancel flag — mirroring `asr_download_model`.
//!   - `reel_cancel_render` — flip the cancel flag.
//!
//! Keyless discipline: `reel_storyboard` NEVER errors for lack of a key. It
//! sets `used_ai=false` and returns the heuristic plan so the UI can show
//! "AI ikke tilgjengelig — forslag fra pauser i talen" and let the operator
//! curate manually.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tauri::Emitter;
use ts_rs::TS;

use crate::error::AppResult;
use crate::model::{Project, Style};
use crate::services::burnin;
use crate::services::highlight_reel::{
    self, HeuristicParams, ReelRenderProgress, RenderPlan, build_render_plan, heuristic_plan,
};
use crate::services::llm::clips::{
    self, ClipPlan, build_clips_system_prompt, build_clips_user_prompt, estimate_output_tokens,
};
use crate::services::llm::{self, ClaudeModel, LlmConfig};
use crate::services::secrets::{self, SecretProvider};

/// Shared cancel flag for the in-flight batch render (one batch at a time).
/// Managed by the Tauri runtime, mirroring `asr::DownloadControl`.
#[derive(Default)]
pub struct ReelRenderControl {
    cancel: Arc<AtomicBool>,
}

/// The reviewable storyboard handed back to the operator. `used_ai` tells the
/// UI whether these came from Claude or the keyless heuristic, so it can label
/// the source honestly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/ReelStoryboard.ts")]
pub struct ReelStoryboard {
    pub plan: ClipPlan,
    /// True when Claude produced the clips; false when the heuristic did.
    pub used_ai: bool,
    /// Set when the AI path was attempted but failed (key present, call errored)
    /// — the UI can surface it while still showing the heuristic fallback.
    pub ai_error: Option<String>,
}

/// Outcome of the batch render — surfaced after "Render all" finishes (or is
/// cancelled), so a partial success is visible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/ReelRenderResult.ts")]
pub struct ReelRenderResult {
    /// Output paths that rendered successfully.
    pub rendered: Vec<String>,
    /// (item id, error message) for items that failed — one bad clip does not
    /// abort the whole batch.
    pub failed: Vec<(String, String)>,
    /// True if the operator cancelled partway.
    pub cancelled: bool,
}

/// Propose a storyboard of social clips for this sermon. Tries the AI planner
/// when a key is available; otherwise (or on AI failure) returns the keyless
/// heuristic. Never blocks the flow for lack of a key.
#[tauri::command]
pub async fn reel_storyboard(
    project: Project,
    model: ClaudeModel,
    api_key: Option<String>,
) -> AppResult<ReelStoryboard> {
    let has_captions = project.captions.iter().any(|c| !c.words.is_empty());
    if !has_captions {
        return Ok(ReelStoryboard {
            plan: ClipPlan {
                talk_summary: String::new(),
                clips: vec![],
            },
            used_ai: false,
            ai_error: None,
        });
    }

    // Resolve a key WITHOUT prompting; empty means "no key" → heuristic.
    let key = secrets::resolve(api_key, SecretProvider::Anthropic, "ANTHROPIC_API_KEY");
    if key.trim().is_empty() {
        return Ok(ReelStoryboard {
            plan: heuristic_plan(&project, HeuristicParams::default()),
            used_ai: false,
            ai_error: None,
        });
    }

    // AI path. On ANY failure (no `llm` feature, network, parse) we degrade to
    // the heuristic rather than surfacing a hard error — the operator still
    // gets a working storyboard.
    let system = build_clips_system_prompt(&project.language);
    let user = build_clips_user_prompt(&project);
    let max_tokens = estimate_output_tokens(&project).clamp(256, 4096) as u32;
    let config = LlmConfig {
        model,
        api_key: key,
    };

    match llm::complete(&config, &system, &user, max_tokens).await {
        Ok(response) => match clips::parse_clips_response(&project, &response) {
            Ok(plan) if !plan.clips.is_empty() => Ok(ReelStoryboard {
                plan,
                used_ai: true,
                ai_error: None,
            }),
            // Parsed but empty, or parse failed → fall back, keep the reason.
            Ok(_) => Ok(ReelStoryboard {
                plan: heuristic_plan(&project, HeuristicParams::default()),
                used_ai: false,
                ai_error: Some("AI fant ingen klipp — bruker forslag fra pauser i talen.".into()),
            }),
            Err(e) => Ok(ReelStoryboard {
                plan: heuristic_plan(&project, HeuristicParams::default()),
                used_ai: false,
                ai_error: Some(e.to_string()),
            }),
        },
        Err(e) => Ok(ReelStoryboard {
            plan: heuristic_plan(&project, HeuristicParams::default()),
            used_ai: false,
            ai_error: Some(e.to_string()),
        }),
    }
}

/// Fan out the operator-reviewed clips × chosen platforms into a confirmable
/// render plan. Pure — no IO, no render. `preset_ids` empty → vertical social
/// defaults.
#[tauri::command]
pub fn reel_build_plan(plan: ClipPlan, preset_ids: Vec<String>, output_dir: String) -> RenderPlan {
    build_render_plan(&plan.clips, &preset_ids, &output_dir)
}

/// Render every item in `plan`: each clip burned in at its platform preset to a
/// vertical MP4. Streams `reel-render-progress` events, honours the cancel
/// flag, and continues past a failed item (reported in the result). Mirrors the
/// `asr_download_model` atomic+progress+cancel pattern.
#[tauri::command]
pub async fn reel_render_all(
    window: tauri::Window,
    control: tauri::State<'_, ReelRenderControl>,
    project: Project,
    plan: RenderPlan,
) -> AppResult<ReelRenderResult> {
    let cancel = control.cancel.clone();
    cancel.store(false, Ordering::Relaxed);

    // ffmpeg burn-in is synchronous and blocks for minutes per clip. Run the
    // whole batch on a blocking thread so it never starves the Tokio executor
    // (mirrors asr_transcribe_local); progress is emitted from inside via
    // window.emit, and cancel is polled between clips.
    tokio::task::spawn_blocking(move || {
        let total = plan.total;
        let mut completed: u32 = 0;
        let mut failed_count: u32 = 0;
        let mut rendered: Vec<String> = Vec::new();
        let mut failed: Vec<(String, String)> = Vec::new();
        let mut cancelled = false;

        let emit = |window: &tauri::Window, p: &ReelRenderProgress| {
            let _ = window.emit("reel-render-progress", p);
        };

        // Initial 0% tick.
        emit(
            &window,
            &ReelRenderProgress {
                completed: 0,
                total,
                fraction: highlight_reel::render_fraction(0, total),
                current_item_id: plan.items.first().map(|i| i.id.clone()),
                failed: 0,
            },
        );

        let title_style = Style::title_overlay();

        for item in &plan.items {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }

            // Announce which item is starting.
            emit(
                &window,
                &ReelRenderProgress {
                    completed,
                    total,
                    fraction: highlight_reel::render_fraction(completed, total),
                    current_item_id: Some(item.id.clone()),
                    failed: failed_count,
                },
            );

            let options = item.preset.to_clip_burnin_options(&item.clip);
            let out = Path::new(&item.output_path);
            // Ensure the output directory exists (best-effort; render reports the
            // real error if it can't write).
            if let Some(parent) = out.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            match burnin::render_clip(&project, &item.clip, out, &options, &title_style) {
                Ok(()) => rendered.push(item.output_path.clone()),
                Err(e) => {
                    failed_count += 1;
                    failed.push((item.id.clone(), e.to_string()));
                }
            }

            completed += 1;
            emit(
                &window,
                &ReelRenderProgress {
                    completed,
                    total,
                    fraction: highlight_reel::render_fraction(completed, total),
                    current_item_id: Some(item.id.clone()),
                    failed: failed_count,
                },
            );
        }

        // Final tick: no current item.
        emit(
            &window,
            &ReelRenderProgress {
                completed,
                total,
                fraction: highlight_reel::render_fraction(completed, total),
                current_item_id: None,
                failed: failed_count,
            },
        );

        ReelRenderResult {
            rendered,
            failed,
            cancelled,
        }
    })
    .await
    .map_err(|e| crate::error::AppError::Internal(format!("reel render task join: {e}")))
}

/// Cancel the in-flight batch render, if any. The loop stops after the current
/// item finishes (we never kill a half-written MP4 mid-encode).
#[tauri::command]
pub fn reel_cancel_render(control: tauri::State<'_, ReelRenderControl>) {
    control.cancel.store(true, Ordering::Relaxed);
}
