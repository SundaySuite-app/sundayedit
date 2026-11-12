//! AI smart-suggestion Tauri commands — Phase 4.3.
//!
//! Three commands mirroring the propose→approve flow:
//!   - `suggest_estimate` — pure cost/scope preview, no network.
//!   - `suggest_captions` — async. Returns a review queue of suggestions.
//!     Applies NOTHING; the user decides per item.
//!   - `apply_suggestion` — pure. Applies one accepted suggestion.

use crate::commands::polish::PolishEstimate;
use crate::error::AppResult;
use crate::model::Project;
use crate::services::llm::suggest::{
    self, Strictness, Suggestion, build_suggest_system_prompt, build_suggest_user_prompt,
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

#[tauri::command]
pub fn suggest_estimate(
    project: Project,
    model: ClaudeModel,
    strictness: Strictness,
) -> AppResult<PolishEstimate> {
    let system = build_suggest_system_prompt(&project.language, strictness);
    let user = build_suggest_user_prompt(&project);
    let input = rough_token_count(&system) + rough_token_count(&user);
    let output = estimate_output_tokens(&project);

    let caption_count = project
        .captions
        .iter()
        .filter(|c| !c.words.is_empty())
        .count();
    Ok(PolishEstimate {
        caption_count,
        estimated_input_tokens: input,
        estimated_output_tokens: output,
        estimated_cost_usd: estimate_cost_usd(model, input, output),
        model_id: model.id().to_string(),
    })
}

/// Run a Smart Suggest pass. Returns the review queue — nothing is applied.
#[tauri::command]
pub async fn suggest_captions(
    project: Project,
    model: ClaudeModel,
    strictness: Strictness,
    api_key: Option<String>,
) -> AppResult<Vec<Suggestion>> {
    let has_captions = project.captions.iter().any(|c| !c.words.is_empty());
    if !has_captions {
        return Ok(vec![]);
    }

    let system = build_suggest_system_prompt(&project.language, strictness);
    let user = build_suggest_user_prompt(&project);
    let max_tokens = estimate_output_tokens(&project).clamp(256, 8192) as u32;

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
    suggest::parse_suggestions_response(&response)
}

/// Apply one suggestion the user accepted.
#[tauri::command]
pub fn apply_suggestion(project: Project, suggestion: Suggestion) -> AppResult<Project> {
    suggest::apply_suggestion(&project, &suggestion, now_ms())
}
