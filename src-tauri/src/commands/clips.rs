//! AI social-clip Tauri commands — SundayEdit.
//!
//! Mirrors the suggest flow:
//!   - `clips_estimate` — pure cost/scope preview, no network.
//!   - `clips_generate` — async. Returns a reviewable ClipPlan; applies nothing.
//!   - `clips_apply_plan` — pure. Persists the reviewed plan onto the project.

use crate::commands::polish::PolishEstimate;
use crate::error::AppResult;
use crate::model::Project;
use crate::services::llm::clips::{
    self, ClipPlan, build_clips_system_prompt, build_clips_user_prompt, estimate_output_tokens,
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
pub fn clips_estimate(project: Project, model: ClaudeModel) -> AppResult<PolishEstimate> {
    let system = build_clips_system_prompt(&project.language);
    let user = build_clips_user_prompt(&project);
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

/// Generate a clip plan from the transcript. Returns the reviewable plan —
/// nothing is persisted until `clips_apply_plan`.
#[tauri::command]
pub async fn clips_generate(
    project: Project,
    model: ClaudeModel,
    api_key: Option<String>,
) -> AppResult<ClipPlan> {
    let has_captions = project.captions.iter().any(|c| !c.words.is_empty());
    if !has_captions {
        return Ok(ClipPlan {
            talk_summary: String::new(),
            clips: vec![],
        });
    }

    let system = build_clips_system_prompt(&project.language);
    let user = build_clips_user_prompt(&project);
    let max_tokens = estimate_output_tokens(&project).clamp(256, 4096) as u32;

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
    clips::parse_clips_response(&project, &response)
}

/// Persist a reviewed plan (clips + talk summary) onto the project.
#[tauri::command]
pub fn clips_apply_plan(project: Project, plan: ClipPlan) -> AppResult<Project> {
    Ok(clips::apply_plan(&project, &plan, now_ms()))
}
