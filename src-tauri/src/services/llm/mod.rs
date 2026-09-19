//! Claude API client — Phase 4 foundation.
//!
//! AI assistance (punctuation polish, smart suggestions, translation) all
//! talk to Anthropic's Messages API. This module is the one place that
//! knows how to build a request and read a response.
//!
//! Discipline, mirroring the cloud-ASR module:
//!   - Everything that matters for correctness is PURE and tested offline:
//!     model metadata, cost estimation, request-body construction, and
//!     response parsing.
//!   - The HTTP call itself sits behind the optional `llm` cargo feature
//!     (`reqwest`). The default build — and the whole test suite — compile
//!     and run without it; the stub returns a clear, actionable error so
//!     the UI can say "this build has no AI features / set an API key."
//!
//! API keys never live in plaintext config: the command layer reads them
//! from the OS keychain or the `ANTHROPIC_API_KEY` env var and passes them
//! in here.

pub mod clips;
pub mod glossary_suggest;
pub mod polish;
pub mod suggest;
pub mod translate;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, AppResult};
use crate::model::Project;

/// One caption serialised for an LLM request: its id and its joined text.
/// Shared by the passes that only need `{caption_id, text}` (translate,
/// suggest). The clip-detection pass uses a wider struct of its own because
/// it also feeds the model the real timings.
#[derive(Serialize)]
pub struct CaptionTextInput<'a> {
    pub caption_id: &'a str,
    pub text: String,
}

/// Build the `{caption_id, text}` request items, skipping empty captions.
pub fn caption_text_inputs(project: &Project) -> Vec<CaptionTextInput<'_>> {
    project
        .captions
        .iter()
        .filter(|c| !c.words.is_empty())
        .map(|c| CaptionTextInput {
            caption_id: &c.id,
            text: c.text(),
        })
        .collect()
}

pub const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The Claude models SundayEdit offers for AI features. Haiku is the default
/// for high-volume passes (polish over a whole project); Sonnet/Opus are
/// available when the user wants maximum quality on hard content.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export, export_to = "../../src/lib/bindings/ClaudeModel.ts")]
pub enum ClaudeModel {
    // Polish is a high-volume, low-creativity task — Haiku is plenty and keeps
    // a 60-min project well under a cent, so it is the default.
    #[default]
    Haiku45,
    Sonnet46,
    Opus47,
}

impl ClaudeModel {
    /// The exact API model identifier.
    pub fn id(&self) -> &'static str {
        match self {
            ClaudeModel::Haiku45 => "claude-haiku-4-5-20251001",
            ClaudeModel::Sonnet46 => "claude-sonnet-4-6",
            ClaudeModel::Opus47 => "claude-opus-4-7",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            ClaudeModel::Haiku45 => "Claude Haiku 4.5",
            ClaudeModel::Sonnet46 => "Claude Sonnet 4.6",
            ClaudeModel::Opus47 => "Claude Opus 4.7",
        }
    }

    /// (input, output) USD price per million tokens. Estimates for the cost
    /// preview only — the real invoice comes from Anthropic. Update when
    /// pricing changes.
    pub fn price_per_mtok(&self) -> (f64, f64) {
        match self {
            ClaudeModel::Haiku45 => (1.0, 5.0),
            ClaudeModel::Sonnet46 => (3.0, 15.0),
            ClaudeModel::Opus47 => (15.0, 75.0),
        }
    }
}

/// Very rough token estimate (~4 chars/token) for the pre-run cost preview.
/// Deliberately approximate; never used for billing, only to set
/// expectations before a paid call.
pub fn rough_token_count(text: &str) -> usize {
    (text.chars().count() as f64 / 4.0).ceil().max(1.0) as usize
}

/// Estimated USD cost of a call. Pure.
pub fn estimate_cost_usd(model: ClaudeModel, input_tokens: usize, output_tokens: usize) -> f64 {
    let (in_price, out_price) = model.price_per_mtok();
    (input_tokens as f64 / 1_000_000.0) * in_price
        + (output_tokens as f64 / 1_000_000.0) * out_price
}

/// What `complete` needs: which model, and the key to authenticate with.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub model: ClaudeModel,
    pub api_key: String,
}

/// Build the Anthropic Messages request body. Pure → JSON value, so it can
/// be asserted on without a network. A single user turn with a system
/// prompt is all our features need.
pub fn build_messages_body(
    model: ClaudeModel,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> serde_json::Value {
    serde_json::json!({
        "model": model.id(),
        "max_tokens": max_tokens,
        "system": system,
        "messages": [
            { "role": "user", "content": user }
        ]
    })
}

/// Extract the assistant's text from an Anthropic Messages response. Pure.
/// Concatenates every `text` content block; surfaces API errors as
/// `AppError::Internal` with the provider's message.
pub fn parse_text_response(json: &str) -> AppResult<String> {
    let v: serde_json::Value = serde_json::from_str(json)?;

    // Error envelope: { "type": "error", "error": { "type", "message" } }
    if v.get("type").and_then(|t| t.as_str()) == Some("error") {
        let msg = v
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown Anthropic API error");
        return Err(AppError::Internal(format!("Claude API error: {msg}")));
    }

    let content = v
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| AppError::Validation("Claude response has no content array".into()))?;

    let mut text = String::new();
    for block in content {
        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                text.push_str(t);
            }
        }
    }

    if text.trim().is_empty() {
        return Err(AppError::Validation(
            "Claude response contained no text content".into(),
        ));
    }
    Ok(text)
}

// ── The network call (feature = "llm") ────────────────────────────────────────
#[cfg(feature = "llm")]
pub async fn complete(
    config: &LlmConfig,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> AppResult<String> {
    let body = build_messages_body(config.model, system, user, max_tokens);
    let client = crate::services::http::client();
    let resp = client
        .post(ANTHROPIC_API_URL)
        .header("x-api-key", &config.api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("Claude request failed: {e}")))?;

    let text = resp
        .text()
        .await
        .map_err(|e| AppError::Internal(format!("reading Claude response: {e}")))?;
    parse_text_response(&text)
}

// ── Stub (default build, no `llm` feature) ────────────────────────────────────
#[cfg(not(feature = "llm"))]
pub async fn complete(
    _config: &LlmConfig,
    _system: &str,
    _user: &str,
    _max_tokens: u32,
) -> AppResult<String> {
    Err(AppError::Internal(
        "This build of SundayEdit does not include AI features. \
         Rebuild with `--features llm` to enable Claude-powered polish."
            .to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_are_exact() {
        assert_eq!(ClaudeModel::Haiku45.id(), "claude-haiku-4-5-20251001");
        assert_eq!(ClaudeModel::Sonnet46.id(), "claude-sonnet-4-6");
        assert_eq!(ClaudeModel::Opus47.id(), "claude-opus-4-7");
    }

    #[test]
    fn default_model_is_haiku() {
        assert_eq!(ClaudeModel::default(), ClaudeModel::Haiku45);
    }

    #[test]
    fn token_count_is_roughly_chars_over_four() {
        assert_eq!(rough_token_count(""), 1); // floored to at least 1
        assert_eq!(rough_token_count("abcd"), 1);
        assert_eq!(rough_token_count("abcde"), 2);
    }

    #[test]
    fn cost_estimate_scales_with_model() {
        // Same token budget costs more on Opus than Haiku.
        let haiku = estimate_cost_usd(ClaudeModel::Haiku45, 1_000_000, 1_000_000);
        let opus = estimate_cost_usd(ClaudeModel::Opus47, 1_000_000, 1_000_000);
        assert!((haiku - 6.0).abs() < 1e-9, "got {haiku}"); // 1.0 + 5.0
        assert!((opus - 90.0).abs() < 1e-9, "got {opus}"); // 15.0 + 75.0
        assert!(opus > haiku);
    }

    #[test]
    fn request_body_has_model_system_and_user_turn() {
        let body = build_messages_body(ClaudeModel::Haiku45, "be terse", "hello", 256);
        assert_eq!(body["model"], "claude-haiku-4-5-20251001");
        assert_eq!(body["max_tokens"], 256);
        assert_eq!(body["system"], "be terse");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hello");
    }

    #[test]
    fn parses_text_blocks() {
        let json = r#"{
          "type": "message", "role": "assistant",
          "content": [
            { "type": "text", "text": "Hello, " },
            { "type": "text", "text": "world." }
          ],
          "stop_reason": "end_turn"
        }"#;
        assert_eq!(parse_text_response(json).unwrap(), "Hello, world.");
    }

    #[test]
    fn surfaces_api_error_envelope() {
        let json = r#"{ "type": "error", "error": { "type": "authentication_error", "message": "invalid x-api-key" } }"#;
        let err = parse_text_response(json).unwrap_err();
        assert_eq!(err.code(), "internal");
        assert!(err.to_string().contains("invalid x-api-key"));
    }

    #[test]
    fn rejects_response_without_content() {
        assert!(parse_text_response("{}").is_err());
    }

    #[test]
    fn rejects_empty_text() {
        let json = r#"{ "content": [ { "type": "text", "text": "   " } ] }"#;
        assert!(parse_text_response(json).is_err());
    }

    // Default build has no `llm` feature → complete() must fail loudly, not
    // panic, so the UI can guide the user.
    #[cfg(not(feature = "llm"))]
    #[tokio::test]
    async fn complete_stub_returns_actionable_error() {
        let cfg = LlmConfig {
            model: ClaudeModel::Haiku45,
            api_key: "x".into(),
        };
        let err = complete(&cfg, "sys", "user", 16).await.unwrap_err();
        assert_eq!(err.code(), "internal");
        assert!(err.to_string().contains("--features llm"));
    }
}
