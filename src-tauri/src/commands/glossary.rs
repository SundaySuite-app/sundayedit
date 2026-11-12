//! AI glossary-suggestion Tauri commands — Phase 3.4 (modes 3 + 4).
//!
//! Propose-and-approve, like Smart Suggest:
//!   - `glossary_suggest_estimate` — pure cost/scope preview, no network.
//!   - `glossary_suggest` — async. Returns candidate terms from the transcript
//!     (mode 3); adds nothing. The renderer merges the accepted ones.
//!   - `glossary_extract_document` — read a reference file (.txt/.docx) to text.
//!   - `glossary_from_document_estimate` / `glossary_from_document` — mode 4:
//!     propose terms from that document, *before* transcription.
//!
//! (The post-pass `op_apply_glossary` that corrects captions from an existing
//! glossary lives in `commands::operations`.)

use crate::commands::polish::PolishEstimate;
use crate::error::AppResult;
use crate::model::Project;
use crate::services::document::{self, ExtractedDocument};
use crate::services::llm::glossary_suggest::{
    self, SuggestedTerm, build_doc_glossary_system_prompt, build_doc_glossary_user_prompt,
    build_glossary_system_prompt, build_glossary_user_prompt, estimate_doc_output_tokens,
    estimate_output_tokens,
};
use crate::services::llm::{self, ClaudeModel, LlmConfig, estimate_cost_usd, rough_token_count};

#[tauri::command]
pub fn glossary_suggest_estimate(
    project: Project,
    model: ClaudeModel,
) -> AppResult<PolishEstimate> {
    let system = build_glossary_system_prompt(&project.language);
    let user = build_glossary_user_prompt(&project);
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

/// Scan the transcript for likely-misrecognized terms. Returns candidates for
/// review; nothing is added to the project here.
#[tauri::command]
pub async fn glossary_suggest(
    project: Project,
    model: ClaudeModel,
    api_key: Option<String>,
) -> AppResult<Vec<SuggestedTerm>> {
    let has_captions = project.captions.iter().any(|c| !c.words.is_empty());
    if !has_captions {
        return Ok(vec![]);
    }

    let system = build_glossary_system_prompt(&project.language);
    let user = build_glossary_user_prompt(&project);
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
    glossary_suggest::parse_glossary_suggestions(&response)
}

// ── Mode 4: glossary terms from a reference document ─────────────────────────

/// Read a reference document (.txt/.md/.docx) into plain text, truncated to the
/// prompt budget. Pure file I/O — no network, nothing added to the project.
#[tauri::command]
pub fn glossary_extract_document(path: String) -> AppResult<ExtractedDocument> {
    document::extract_document(&path)
}

/// Pure cost/scope preview for a document pass — no network.
#[tauri::command]
pub fn glossary_from_document_estimate(
    project: Project,
    model: ClaudeModel,
    document_text: String,
) -> AppResult<PolishEstimate> {
    let system = build_doc_glossary_system_prompt(&project.language);
    let user = build_doc_glossary_user_prompt(&project, &document_text);
    let input = rough_token_count(&system) + rough_token_count(&user);
    let output = estimate_doc_output_tokens(&document_text);

    Ok(PolishEstimate {
        // Document mode isn't transcript-based, so there are no captions to count.
        caption_count: 0,
        estimated_input_tokens: input,
        estimated_output_tokens: output,
        estimated_cost_usd: estimate_cost_usd(model, input, output),
        model_id: model.id().to_string(),
    })
}

/// Propose glossary terms from a reference document. Returns candidates for
/// review; nothing is added to the project here.
#[tauri::command]
pub async fn glossary_from_document(
    project: Project,
    model: ClaudeModel,
    document_text: String,
    api_key: Option<String>,
) -> AppResult<Vec<SuggestedTerm>> {
    if document_text.trim().is_empty() {
        return Ok(vec![]);
    }

    let system = build_doc_glossary_system_prompt(&project.language);
    let user = build_doc_glossary_user_prompt(&project, &document_text);
    let max_tokens = estimate_doc_output_tokens(&document_text).clamp(256, 4096) as u32;

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
    glossary_suggest::parse_glossary_suggestions(&response)
}
