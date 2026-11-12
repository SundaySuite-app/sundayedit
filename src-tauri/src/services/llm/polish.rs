//! AI punctuation & capitalization polish — Phase 4.1.
//!
//! Whisper's punctuation is decent but imperfect, especially for Nordic
//! languages. An LLM polish pass cleans it up. The promise to the user —
//! and the thing this module enforces in code — is narrow:
//!
//!   **Polish may change punctuation and capitalization. It may NEVER
//!   change word content.**
//!
//! That guarantee is what makes the feature safe to run automatically. The
//! enforcement is a pure function, [`substance_preserved`], applied to
//! every caption: if the model's output is anything other than the same
//! sequence of words with different casing/punctuation, the polish for
//! that caption is REJECTED and the original is kept untouched. We never
//! trust the model not to drift; we verify.
//!
//! Word timings come from audio and are sacrosanct — polish maps the
//! corrected surface text back onto the existing words positionally, so
//! start/end/confidence are preserved exactly.
//!
//! Pure pipeline (all tested offline):
//!   build_polish_items → build_polish_user_prompt / build_polish_system_prompt
//!     → [network: llm::complete] → parse_polish_response → apply_polish

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{AppError, AppResult};
use crate::model::Project;
use crate::services::llm::rough_token_count;

/// One caption sent to / returned from the model. `id` round-trips so we
/// can match the polished text back to its caption regardless of order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolishItem {
    pub id: String,
    pub text: String,
}

/// One applied punctuation/casing change — surfaced so the user can review,
/// compare against the original ("Show original"), or undo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/PolishChange.ts")]
pub struct PolishChange {
    pub caption_id: String,
    pub word_index: usize,
    pub from: String,
    pub to: String,
}

/// Result of applying polish: the updated project, the per-word changes,
/// and the ids of any captions whose polish was rejected by the substance
/// guard (the model tried to alter word content — we kept the original).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../src/lib/bindings/PolishResult.ts")]
pub struct PolishResult {
    pub project: Project,
    pub changes: Vec<PolishChange>,
    pub rejected: Vec<String>,
}

// ── Building the request ──────────────────────────────────────────────────────

/// One item per non-empty caption, text derived from its words.
pub fn build_polish_items(project: &Project) -> Vec<PolishItem> {
    project
        .captions
        .iter()
        .filter(|c| !c.words.is_empty())
        .map(|c| PolishItem {
            id: c.id.clone(),
            text: c.text(),
        })
        .collect()
}

fn language_rules(language: &str) -> &'static str {
    match language {
        "no" | "nb" | "nn" => {
            "The language is Norwegian. Capitalize the first word of each sentence and proper \
             nouns. \"jeg\" is lower-case except at the start of a sentence. Follow Norwegian \
             punctuation conventions."
        }
        "en" => "The language is English. Use standard sentence casing and punctuation.",
        "sv" => "The language is Swedish. Use standard Swedish casing and punctuation.",
        "da" => "The language is Danish. Use standard Danish casing and punctuation.",
        "de" => "The language is German. Capitalize nouns as German grammar requires.",
        _ => {
            "Use the standard sentence-casing and punctuation conventions of the caption's language."
        }
    }
}

pub fn build_polish_system_prompt(language: &str) -> String {
    format!(
        "You are a meticulous subtitle copy-editor. Your only job is to fix punctuation and \
         capitalization.\n\n\
         Hard rules you must never break:\n\
         - NEVER add, remove, reorder, or substitute words. The sequence of words in each caption \
         must stay byte-for-byte identical apart from letter-case.\n\
         - You may only change capitalization and add, remove, or adjust punctuation marks \
         (. , ! ? ; : — \" ' …).\n\
         - Preserve the exact word count of every caption.\n\
         - Do not merge or split captions.\n\
         - {rules}\n\n\
         Output format: return ONLY a JSON array, with no prose and no code fences. Each element \
         is an object {{\"id\": <the caption's id unchanged>, \"text\": <the polished text>}}. \
         Return every caption you were given, with the same ids in the same order.",
        rules = language_rules(language)
    )
}

pub fn build_polish_user_prompt(items: &[PolishItem]) -> String {
    let json = serde_json::to_string_pretty(items).unwrap_or_else(|_| "[]".to_string());
    format!("Polish the punctuation and capitalization of these captions:\n\n{json}")
}

/// Output is roughly the size of the input plus JSON scaffolding. Used to
/// pick `max_tokens` and to preview cost. Generous so a polish is never
/// truncated.
pub fn estimate_output_tokens(items: &[PolishItem]) -> usize {
    let body: usize = items
        .iter()
        .map(|i| rough_token_count(&i.text) + rough_token_count(&i.id) + 8)
        .sum();
    ((body as f64) * 1.3).ceil() as usize + 64
}

// ── Parsing the response ──────────────────────────────────────────────────────

/// Pull the outermost JSON array out of a model response, tolerating code
/// fences or stray prose around it (first `[` … last `]`).
fn extract_json_array(s: &str) -> Option<&str> {
    let start = s.find('[')?;
    let end = s.rfind(']')?;
    if end > start {
        Some(&s[start..=end])
    } else {
        None
    }
}

pub fn parse_polish_response(response: &str) -> AppResult<Vec<PolishItem>> {
    let slice = extract_json_array(response)
        .ok_or_else(|| AppError::Validation("polish response had no JSON array".into()))?;
    let items: Vec<PolishItem> = serde_json::from_str(slice)
        .map_err(|e| AppError::Validation(format!("polish response was not valid JSON: {e}")))?;
    Ok(items)
}

// ── The substance-preservation guard (the heart) ──────────────────────────────

/// The alphanumeric content of a token, lower-cased and stripped of all
/// punctuation. This is what must be invariant under polish: "World." and
/// "world" share the core "world"; "don't" and "dont" share "dont".
/// Unicode-aware, so Norwegian "Får"/"får" → "får".
fn word_core(token: &str) -> String {
    token
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// THE guard. True iff `polished` is the same sequence of words as
/// `original`, differing only in capitalization and punctuation. A
/// different word count, a substituted word, a reorder, or a dropped word
/// all return false → the caller rejects the polish and keeps the original.
pub fn substance_preserved(original_tokens: &[&str], polished_tokens: &[&str]) -> bool {
    if original_tokens.len() != polished_tokens.len() {
        return false;
    }
    original_tokens
        .iter()
        .zip(polished_tokens.iter())
        .all(|(o, p)| word_core(o) == word_core(p))
}

// ── Applying polish ────────────────────────────────────────────────────────────

/// Apply parsed polish items back onto the project. For each caption whose
/// polish passes the substance guard, replace each word's surface text
/// with the polished token (timings/confidence preserved), mark genuinely
/// changed words `polished`, and record a [`PolishChange`]. Captions whose
/// polish fails the guard are left untouched and their ids returned in
/// `rejected`.
pub fn apply_polish(project: &Project, polished: &[PolishItem], now_ms: i64) -> PolishResult {
    use std::collections::HashMap;

    let by_id: HashMap<&str, &str> = polished
        .iter()
        .map(|p| (p.id.as_str(), p.text.as_str()))
        .collect();

    let mut next = project.clone();
    let mut changes = Vec::new();
    let mut rejected = Vec::new();
    let mut any_change = false;

    for cap in next.captions.iter_mut() {
        let Some(polished_text) = by_id.get(cap.id.as_str()) else {
            continue; // model didn't return this caption — leave it as-is
        };

        let original_tokens: Vec<&str> = cap.words.iter().map(|w| w.text.as_str()).collect();
        let polished_tokens: Vec<&str> = polished_text.split_whitespace().collect();

        if !substance_preserved(&original_tokens, &polished_tokens) {
            rejected.push(cap.id.clone());
            continue;
        }

        let mut caption_changed = false;
        for (i, word) in cap.words.iter_mut().enumerate() {
            let new_text = polished_tokens[i];
            if new_text != word.text {
                changes.push(PolishChange {
                    caption_id: cap.id.clone(),
                    word_index: i,
                    from: word.text.clone(),
                    to: new_text.to_string(),
                });
                word.text = new_text.to_string();
                word.polished = true;
                caption_changed = true;
            }
        }
        if caption_changed {
            cap.last_edited_at = now_ms;
            any_change = true;
        }
    }

    if any_change {
        next.updated_at = now_ms;
    }

    PolishResult {
        project: next,
        changes,
        rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Caption, Style, Word};

    fn caption(id: &str, words: Vec<Word>) -> Caption {
        let end = words.last().map(|w| w.end_ms).unwrap_or(1000);
        Caption {
            id: id.into(),
            start_ms: words.first().map(|w| w.start_ms).unwrap_or(0),
            end_ms: end,
            words,
            speaker_id: None,
            style_id: None,
            notes: None,
            ai_generated: true,
            last_edited_at: 0,
            track_id: None,
        }
    }

    fn project_with(captions: Vec<Caption>) -> Project {
        Project {
            id: "p".into(),
            name: "t".into(),
            video_path: "/x".into(),
            video_content_hash: "h".into(),
            video_duration_ms: 10_000,
            video_width: 0,
            video_height: 0,
            video_fps: 0.0,
            audio_wav_path: None,
            language: "no".into(),
            default_style: Style::broadcast_news(),
            context_description: None,
            captions,
            speakers: vec![],
            glossary: vec![],
            clips: vec![],
            talk_summary: None,
            export_config: crate::model::ExportConfig::default(),
            project_meta: crate::model::ProjectMeta::default(),
            created_at: 0,
            updated_at: 0,
            media: vec![],
            tracks: vec![],
            timeline_items: vec![],
        }
    }

    // ── word_core ──────────────────────────────────────────────────────────
    #[test]
    fn word_core_strips_case_and_punctuation() {
        assert_eq!(word_core("World."), "world");
        assert_eq!(word_core("\"Hello,\""), "hello");
        assert_eq!(word_core("don't"), "dont");
        assert_eq!(word_core("U.S.A."), "usa");
        assert_eq!(word_core("Får"), "får"); // Norwegian, unicode-lowered
        assert_eq!(word_core("—"), ""); // pure punctuation
    }

    // ── substance_preserved: the guard ──────────────────────────────────────
    #[test]
    fn guard_allows_punctuation_and_casing() {
        assert!(substance_preserved(
            &["i", "think", "we", "should", "go"],
            &["I", "think", "we", "should", "go."],
        ));
        assert!(substance_preserved(
            &["hello", "world"],
            &["Hello,", "world!"]
        ));
        assert!(substance_preserved(&["dont"], &["don't"]));
    }

    #[test]
    fn guard_rejects_word_substitution() {
        // "their" → "there" is a content change, not punctuation.
        assert!(!substance_preserved(
            &["their", "house"],
            &["there", "house"]
        ));
    }

    #[test]
    fn guard_rejects_added_word() {
        assert!(!substance_preserved(
            &["hello", "world"],
            &["hello", "big", "world"]
        ));
    }

    #[test]
    fn guard_rejects_dropped_word() {
        assert!(!substance_preserved(
            &["um", "hello", "world"],
            &["hello", "world"]
        ));
    }

    #[test]
    fn guard_rejects_reorder() {
        assert!(!substance_preserved(
            &["hello", "world"],
            &["world", "hello"]
        ));
    }

    #[test]
    fn guard_rejects_merge_or_split() {
        // merge: "can not" → "cannot" (2 → 1)
        assert!(!substance_preserved(&["can", "not"], &["cannot"]));
        // split: "cannot" → "can not" (1 → 2)
        assert!(!substance_preserved(&["cannot"], &["can", "not"]));
    }

    // ── apply_polish ─────────────────────────────────────────────────────────
    #[test]
    fn applies_punctuation_and_marks_changed_words() {
        let p = project_with(vec![caption(
            "c1",
            vec![
                Word::new("i", 0, 200, 90.0),
                Word::new("think", 200, 500, 88.0),
                Word::new("so", 500, 800, 80.0),
            ],
        )]);
        let polished = vec![PolishItem {
            id: "c1".into(),
            text: "I think so.".into(),
        }];
        let res = apply_polish(&p, &polished, 1234);

        let words = &res.project.captions[0].words;
        assert_eq!(words[0].text, "I");
        assert_eq!(words[1].text, "think");
        assert_eq!(words[2].text, "so.");
        // changed words marked polished; unchanged one is not
        assert!(words[0].polished);
        assert!(!words[1].polished);
        assert!(words[2].polished);
        // timings untouched
        assert_eq!(words[2].start_ms, 500);
        assert_eq!(words[2].end_ms, 800);
        // two changes recorded, none rejected
        assert_eq!(res.changes.len(), 2);
        assert!(res.rejected.is_empty());
        assert_eq!(res.changes[0].from, "i");
        assert_eq!(res.changes[0].to, "I");
        assert_eq!(res.project.captions[0].last_edited_at, 1234);
        assert_eq!(res.project.updated_at, 1234);
    }

    #[test]
    fn rejects_substantive_polish_and_keeps_original() {
        let p = project_with(vec![caption(
            "c1",
            vec![
                Word::new("their", 0, 300, 70.0),
                Word::new("house", 300, 600, 85.0),
            ],
        )]);
        // Model tried to "correct" their → there: a word change. Reject.
        let polished = vec![PolishItem {
            id: "c1".into(),
            text: "There house.".into(),
        }];
        let res = apply_polish(&p, &polished, 1234);

        assert_eq!(
            res.project.captions[0].words[0].text, "their",
            "original preserved"
        );
        assert!(res.changes.is_empty());
        assert_eq!(res.rejected, vec!["c1".to_string()]);
        // nothing changed → updated_at untouched
        assert_eq!(res.project.updated_at, 0);
    }

    #[test]
    fn mixed_project_accepts_safe_rejects_unsafe() {
        let p = project_with(vec![
            caption("c1", vec![Word::new("hello", 0, 300, 90.0)]),
            caption("c2", vec![Word::new("world", 1000, 1300, 90.0)]),
        ]);
        let polished = vec![
            PolishItem {
                id: "c1".into(),
                text: "Hello!".into(),
            }, // safe
            PolishItem {
                id: "c2".into(),
                text: "Goodbye.".into(),
            }, // substituted → reject
        ];
        let res = apply_polish(&p, &polished, 50);
        assert_eq!(res.project.captions[0].words[0].text, "Hello!");
        assert_eq!(res.project.captions[1].words[0].text, "world");
        assert_eq!(res.changes.len(), 1);
        assert_eq!(res.rejected, vec!["c2".to_string()]);
    }

    #[test]
    fn caption_missing_from_response_is_left_alone() {
        let p = project_with(vec![caption("c1", vec![Word::new("hello", 0, 300, 90.0)])]);
        let res = apply_polish(&p, &[], 50);
        assert_eq!(res.project.captions[0].words[0].text, "hello");
        assert!(res.changes.is_empty());
        assert!(res.rejected.is_empty());
    }

    #[test]
    fn identical_polish_is_a_noop() {
        let p = project_with(vec![caption("c1", vec![Word::new("Hello.", 0, 300, 90.0)])]);
        let polished = vec![PolishItem {
            id: "c1".into(),
            text: "Hello.".into(),
        }];
        let res = apply_polish(&p, &polished, 50);
        assert!(!res.project.captions[0].words[0].polished);
        assert!(res.changes.is_empty());
        assert_eq!(res.project.updated_at, 0);
    }

    // ── prompts + parsing ────────────────────────────────────────────────────
    #[test]
    fn system_prompt_is_language_aware() {
        assert!(build_polish_system_prompt("no").contains("Norwegian"));
        assert!(build_polish_system_prompt("en").contains("English"));
        // Hard rule must always be present.
        assert!(build_polish_system_prompt("xx").contains("NEVER add, remove, reorder"));
    }

    #[test]
    fn user_prompt_embeds_items_as_json() {
        let items = vec![PolishItem {
            id: "c1".into(),
            text: "hello world".into(),
        }];
        let prompt = build_polish_user_prompt(&items);
        assert!(prompt.contains("\"id\""));
        assert!(prompt.contains("c1"));
        assert!(prompt.contains("hello world"));
    }

    #[test]
    fn build_items_skips_empty_captions() {
        let p = project_with(vec![
            caption("c1", vec![Word::new("hi", 0, 100, 90.0)]),
            caption("c2", vec![]),
        ]);
        let items = build_polish_items(&p);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "c1");
        assert_eq!(items[0].text, "hi");
    }

    #[test]
    fn parses_bare_json_array() {
        let r = r#"[{"id":"c1","text":"Hello."},{"id":"c2","text":"World!"}]"#;
        let items = parse_polish_response(r).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].text, "World!");
    }

    #[test]
    fn parses_array_wrapped_in_fences_and_prose() {
        let r = "Sure! Here you go:\n```json\n[{\"id\":\"c1\",\"text\":\"Hi.\"}]\n```\nHope that helps.";
        let items = parse_polish_response(r).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "Hi.");
    }

    #[test]
    fn parse_rejects_non_json() {
        assert!(parse_polish_response("I cannot help with that.").is_err());
    }

    #[test]
    fn end_to_end_parse_then_apply() {
        let p = project_with(vec![caption(
            "c1",
            vec![
                Word::new("hei", 0, 300, 60.0),
                Word::new("verden", 300, 600, 65.0),
            ],
        )]);
        // Model response with fences, fixes casing + adds punctuation.
        let response = "```json\n[{\"id\":\"c1\",\"text\":\"Hei, verden.\"}]\n```";
        let items = parse_polish_response(response).unwrap();
        let res = apply_polish(&p, &items, 99);
        assert_eq!(res.project.captions[0].words[0].text, "Hei,");
        assert_eq!(res.project.captions[0].words[1].text, "verden.");
        assert_eq!(res.changes.len(), 2);
        assert!(res.rejected.is_empty());
    }
}
