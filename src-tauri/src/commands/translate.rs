//! AI translation Tauri commands — Phase 7.1.
//!
//!   - `translate_supported_languages` — the curated target-language list.
//!   - `translate_estimate` — pure cost/scope preview, no network.
//!   - `translate_captions` — async. Returns a translated caption track +
//!     length warnings WITHOUT mutating the project; the caller decides
//!     whether to replace the track.

use crate::commands::polish::PolishEstimate;
use crate::error::AppResult;
use crate::model::Project;
use crate::services::llm::translate::{
    self, TranslationLanguage, TranslationResult, build_translate_system_prompt,
    build_translate_user_prompt, estimate_output_tokens, supported_languages,
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
pub fn translate_supported_languages() -> AppResult<Vec<TranslationLanguage>> {
    Ok(supported_languages())
}

#[tauri::command]
pub fn translate_estimate(
    project: Project,
    target_language: String,
    model: ClaudeModel,
) -> AppResult<PolishEstimate> {
    let system = build_translate_system_prompt(&target_language, &project.glossary);
    let user = build_translate_user_prompt(&project);
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

/// Translate the caption track. Returns the translated captions + warnings;
/// does not mutate the project.
#[tauri::command]
pub async fn translate_captions(
    project: Project,
    target_language: String,
    model: ClaudeModel,
    api_key: Option<String>,
) -> AppResult<TranslationResult> {
    let has_captions = project.captions.iter().any(|c| !c.words.is_empty());
    if !has_captions {
        return Ok(TranslationResult {
            target_language,
            captions: project.captions,
            warnings: vec![],
        });
    }

    let system = build_translate_system_prompt(&target_language, &project.glossary);
    let user = build_translate_user_prompt(&project);
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
    let parsed = translate::parse_translation_response(&response)?;
    Ok(translate::translate_to_captions(
        &project,
        &parsed,
        &target_language,
        now_ms(),
    ))
}
