//! AI punctuation polish Tauri commands — Phase 4.1.
//!
//! Two commands:
//!   - `polish_estimate` — pure, no network. Lets the UI show how many
//!     captions and roughly how much the run will cost BEFORE the user
//!     spends anything.
//!   - `polish_captions` — the real run. Builds the prompt, calls Claude
//!     (feature-gated; stubbed in the default build), parses the response,
//!     and applies it through the substance guard so word content can
//!     never drift. Returns the new project + the change list + any
//!     captions whose polish was rejected.
//!
//! The API key comes from the caller (OS keychain, wired in Settings) or
//! the `ANTHROPIC_API_KEY` env var — never from project files.

use serde::Serialize;
use ts_rs::TS;

use crate::error::AppResult;
use crate::model::Project;
use crate::services::llm::polish::{
    self, PolishResult, build_polish_items, build_polish_system_prompt, build_polish_user_prompt,
    estimate_output_tokens,
};
use crate::services::llm::{self, ClaudeModel, LlmConfig, estimate_cost_usd, rough_token_count};

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/PolishEstimate.ts")]
pub struct PolishEstimate {
    pub caption_count: usize,
    pub estimated_input_tokens: usize,
    pub estimated_output_tokens: usize,
    pub estimated_cost_usd: f64,
    pub model_id: String,
}

/// Pure cost/scope preview — safe to call on every keystroke, no network.
#[tauri::command]
pub fn polish_estimate(project: Project, model: ClaudeModel) -> AppResult<PolishEstimate> {
    let items = build_polish_items(&project);
    let system = build_polish_system_prompt(&project.language);
    let user = build_polish_user_prompt(&items);

    let input = rough_token_count(&system) + rough_token_count(&user);
    let output = estimate_output_tokens(&items);

    Ok(PolishEstimate {
        caption_count: items.len(),
        estimated_input_tokens: input,
        estimated_output_tokens: output,
        estimated_cost_usd: estimate_cost_usd(model, input, output),
        model_id: model.id().to_string(),
    })
}

/// Run the polish. `api_key` falls back to ANTHROPIC_API_KEY when omitted.
#[tauri::command]
pub async fn polish_captions(
    project: Project,
    model: ClaudeModel,
    api_key: Option<String>,
) -> AppResult<PolishResult> {
    let items = build_polish_items(&project);
    if items.is_empty() {
        return Ok(PolishResult {
            project,
            changes: vec![],
            rejected: vec![],
        });
    }

    let system = build_polish_system_prompt(&project.language);
    let user = build_polish_user_prompt(&items);
    let max_tokens = estimate_output_tokens(&items).min(8192) as u32;

    let key = crate::services::secrets::resolve(
        api_key,
        crate::services::secrets::SecretProvider::Anthropic,
        "ANTHROPIC_API_KEY",
    );
    let config = LlmConfig {
        model,
        api_key: key,
    };

    let response = llm::complete(&config, &system, &user, max_tokens).await?;
    let parsed = polish::parse_polish_response(&response)?;
    Ok(polish::apply_polish(&project, &parsed, now_ms()))
}
