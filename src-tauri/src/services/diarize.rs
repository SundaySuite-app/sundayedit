//! Speaker diarization — Phase 4.2.
//!
//! "Who said what." Diarization is genuinely harder than transcription, so
//! the product treats it as best-effort: we surface the result and tell the
//! user to verify before export.
//!
//! As with Whisper and the LLM features, the heavy/native part is gated and
//! everything that matters for correctness is pure and tested offline:
//!   - `parse_diarization_json` — normalize the sidecar's turns.
//!   - `assign_speakers` — overlap-match turns to captions, build the speaker
//!     roster with auto names + colours.
//!   - `merge_speakers` / `rename_speaker` / `set_speaker_color` — the
//!     roster-editing the UI needs (split is just re-running detection or
//!     manual re-assign).
//!
//! The diarization *engine* itself needs audio + a model (pyannote has no
//! mature pure-Rust port yet), so `run_diarization` shells out to a
//! `sundayedit-diarize` sidecar behind the optional `diarize` feature. The
//! default build stubs it with an actionable error.

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::model::{Project, Speaker};

/// One diarization turn from the engine. `speaker` is an opaque label like
/// "SPEAKER_00"; we map it to a friendly roster entry in `assign_speakers`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerTurn {
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: String,
}

/// Distinct, accessible colours assigned to detected speakers in order.
const SPEAKER_COLORS: &[&str] = &[
    "#4FD1C5", "#F6AD55", "#9F7AEA", "#68D391", "#FC8181", "#63B3ED", "#F687B3", "#B5C44D",
];

fn sec_to_ms(sec: f64) -> i64 {
    (sec * 1000.0).round() as i64
}

/// Parse the sidecar's JSON: an array of `{ "start": <sec>, "end": <sec>,
/// "speaker": <label> }`. Pure + tolerant of stray prose/fences.
pub fn parse_diarization_json(json: &str) -> AppResult<Vec<SpeakerTurn>> {
    let slice = {
        let start = json.find('[');
        let end = json.rfind(']');
        match (start, end) {
            (Some(s), Some(e)) if e > s => &json[s..=e],
            _ => {
                return Err(AppError::Validation(
                    "diarization output had no JSON array".into(),
                ));
            }
        }
    };
    let raw: serde_json::Value = serde_json::from_str(slice)
        .map_err(|e| AppError::Validation(format!("diarization output was not valid JSON: {e}")))?;
    let arr = raw
        .as_array()
        .ok_or_else(|| AppError::Validation("diarization output not an array".into()))?;

    let mut turns = Vec::new();
    for t in arr {
        let start = t.get("start").and_then(|v| v.as_f64());
        let end = t.get("end").and_then(|v| v.as_f64());
        let speaker = t.get("speaker").and_then(|v| v.as_str());
        if let (Some(start), Some(end), Some(speaker)) = (start, end, speaker)
            && end > start
            && !speaker.trim().is_empty()
        {
            turns.push(SpeakerTurn {
                start_ms: sec_to_ms(start),
                end_ms: sec_to_ms(end),
                speaker: speaker.trim().to_string(),
            });
        }
    }
    Ok(turns)
}

/// Temporal overlap of two `[start, end]` ranges in ms (0 if disjoint).
fn overlap_ms(a_start: i64, a_end: i64, b_start: i64, b_end: i64) -> i64 {
    (a_end.min(b_end) - a_start.max(b_start)).max(0)
}

/// Assign a speaker to every caption by picking the turn label with the
/// most overlap, and build the project's speaker roster (auto names "Taler
/// 1…N" + distinct colours), ordered by first appearance in time. Captions
/// with no overlapping turn are left unassigned. Pure.
pub fn assign_speakers(project: &Project, turns: &[SpeakerTurn], now_ms: i64) -> Project {
    use std::collections::HashMap;

    // Roster: labels in order of first appearance (turns sorted by start).
    let mut sorted = turns.to_vec();
    sorted.sort_by_key(|t| (t.start_ms, t.end_ms));
    let mut label_to_id: HashMap<String, String> = HashMap::new();
    let mut speakers: Vec<Speaker> = Vec::new();
    for t in &sorted {
        if !label_to_id.contains_key(&t.speaker) {
            let idx = speakers.len();
            let id = format!("spk:{idx}");
            label_to_id.insert(t.speaker.clone(), id.clone());
            speakers.push(Speaker {
                id,
                display_name: format!("Taler {}", idx + 1),
                color_hex: Some(SPEAKER_COLORS[idx % SPEAKER_COLORS.len()].to_string()),
            });
        }
    }

    let mut next = project.clone();
    for cap in next.captions.iter_mut() {
        // Best overlap across all turns.
        let mut best_label: Option<&str> = None;
        let mut best_overlap = 0i64;
        for t in turns {
            let ov = overlap_ms(cap.start_ms, cap.end_ms, t.start_ms, t.end_ms);
            if ov > best_overlap {
                best_overlap = ov;
                best_label = Some(&t.speaker);
            }
        }
        let new_id = best_label.and_then(|l| label_to_id.get(l)).cloned();
        if new_id != cap.speaker_id {
            cap.speaker_id = new_id;
            cap.last_edited_at = now_ms;
        }
    }

    next.speakers = speakers;
    next.updated_at = now_ms;
    next
}

/// Merge `remove_id` into `keep_id`: re-attribute that speaker's captions,
/// then drop it from the roster. For when diarization split one person.
pub fn merge_speakers(
    project: &Project,
    keep_id: &str,
    remove_id: &str,
    now_ms: i64,
) -> AppResult<Project> {
    if keep_id == remove_id {
        return Err(AppError::Validation(
            "cannot merge a speaker into itself".into(),
        ));
    }
    if !project.speakers.iter().any(|s| s.id == keep_id) {
        return Err(AppError::NotFound {
            entity: "speaker",
            id: keep_id.to_string(),
        });
    }
    if !project.speakers.iter().any(|s| s.id == remove_id) {
        return Err(AppError::NotFound {
            entity: "speaker",
            id: remove_id.to_string(),
        });
    }

    let mut next = project.clone();
    for cap in next.captions.iter_mut() {
        if cap.speaker_id.as_deref() == Some(remove_id) {
            cap.speaker_id = Some(keep_id.to_string());
            cap.last_edited_at = now_ms;
        }
    }
    next.speakers.retain(|s| s.id != remove_id);
    next.updated_at = now_ms;
    Ok(next)
}

pub fn rename_speaker(
    project: &Project,
    speaker_id: &str,
    name: &str,
    now_ms: i64,
) -> AppResult<Project> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Validation("speaker name cannot be empty".into()));
    }
    let mut next = project.clone();
    let s = next
        .speakers
        .iter_mut()
        .find(|s| s.id == speaker_id)
        .ok_or_else(|| AppError::NotFound {
            entity: "speaker",
            id: speaker_id.to_string(),
        })?;
    s.display_name = name.to_string();
    next.updated_at = now_ms;
    Ok(next)
}

pub fn set_speaker_color(
    project: &Project,
    speaker_id: &str,
    color_hex: &str,
    now_ms: i64,
) -> AppResult<Project> {
    let mut next = project.clone();
    let s = next
        .speakers
        .iter_mut()
        .find(|s| s.id == speaker_id)
        .ok_or_else(|| AppError::NotFound {
            entity: "speaker",
            id: speaker_id.to_string(),
        })?;
    s.color_hex = Some(color_hex.to_string());
    next.updated_at = now_ms;
    Ok(next)
}

// ── The engine (feature = "diarize") ──────────────────────────────────────────
//
// Shells out to a `sundayedit-diarize <audio.wav>` sidecar that prints the
// turn JSON parsed above. We keep the heavy model out of the Rust binary.
#[cfg(feature = "diarize")]
pub fn run_diarization(audio_path: &std::path::Path) -> AppResult<Vec<SpeakerTurn>> {
    use std::process::Command;
    let output = Command::new("sundayedit-diarize")
        .arg(audio_path)
        .output()
        .map_err(|e| {
            AppError::Internal(format!("could not launch sundayedit-diarize sidecar: {e}"))
        })?;
    if !output.status.success() {
        return Err(AppError::Internal(format!(
            "diarization sidecar failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    parse_diarization_json(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(feature = "diarize"))]
pub fn run_diarization(_audio_path: &std::path::Path) -> AppResult<Vec<SpeakerTurn>> {
    Err(AppError::Internal(
        "This build of SundayEdit does not include speaker diarization. Rebuild with \
         `--features diarize` and install the sundayedit-diarize sidecar."
            .to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Caption, Style, Word};

    fn caption(id: &str, start: i64, end: i64) -> Caption {
        Caption {
            id: id.into(),
            start_ms: start,
            end_ms: end,
            words: vec![Word::new("x", start, end, 90.0)],
            speaker_id: None,
            style_id: None,
            notes: None,
            ai_generated: true,
            last_edited_at: 0,
            track_id: None,
        }
    }

    fn project_with(captions: Vec<Caption>, speakers: Vec<Speaker>) -> Project {
        Project {
            id: "p".into(),
            name: "t".into(),
            video_path: "/x".into(),
            video_content_hash: "h".into(),
            video_duration_ms: 60_000,
            video_width: 0,
            video_height: 0,
            video_fps: 0.0,
            audio_wav_path: None,
            language: "no".into(),
            default_style: Style::broadcast_news(),
            context_description: None,
            captions,
            speakers,
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

    fn turn(start_ms: i64, end_ms: i64, speaker: &str) -> SpeakerTurn {
        SpeakerTurn {
            start_ms,
            end_ms,
            speaker: speaker.into(),
        }
    }

    // ── parse ────────────────────────────────────────────────────────────────
    #[test]
    fn parses_turns_seconds_to_ms() {
        let json = r#"[{"start":0.0,"end":1.5,"speaker":"SPEAKER_00"},{"start":1.5,"end":3.0,"speaker":"SPEAKER_01"}]"#;
        let turns = parse_diarization_json(json).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].end_ms, 1500);
        assert_eq!(turns[1].speaker, "SPEAKER_01");
    }

    #[test]
    fn parse_tolerates_fences_and_drops_degenerate_turns() {
        let json = "```json\n[{\"start\":0,\"end\":0,\"speaker\":\"A\"},{\"start\":0.0,\"end\":2.0,\"speaker\":\"B\"}]\n```";
        let turns = parse_diarization_json(json).unwrap();
        assert_eq!(turns.len(), 1, "zero-length turn dropped");
        assert_eq!(turns[0].speaker, "B");
    }

    #[test]
    fn parse_rejects_non_array() {
        assert!(parse_diarization_json("{}").is_err());
    }

    // ── assign ───────────────────────────────────────────────────────────────
    #[test]
    fn assigns_by_max_overlap_and_builds_roster() {
        let p = project_with(
            vec![
                caption("c1", 0, 1000),
                caption("c2", 1000, 2000),
                caption("c3", 2000, 3000),
            ],
            vec![],
        );
        let turns = vec![turn(0, 1100, "SPEAKER_00"), turn(1100, 3000, "SPEAKER_01")];
        let out = assign_speakers(&p, &turns, 99);

        // roster: two speakers, ordered by first appearance, distinct colours
        assert_eq!(out.speakers.len(), 2);
        assert_eq!(out.speakers[0].display_name, "Taler 1");
        assert_eq!(out.speakers[1].display_name, "Taler 2");
        assert_ne!(out.speakers[0].color_hex, out.speakers[1].color_hex);

        // c1 fully in turn0 → spk:0; c2 mostly turn1 (900 vs 100) → spk:1; c3 → spk:1
        assert_eq!(out.captions[0].speaker_id.as_deref(), Some("spk:0"));
        assert_eq!(out.captions[1].speaker_id.as_deref(), Some("spk:1"));
        assert_eq!(out.captions[2].speaker_id.as_deref(), Some("spk:1"));
    }

    #[test]
    fn caption_with_no_overlap_is_unassigned() {
        let p = project_with(vec![caption("c1", 5000, 6000)], vec![]);
        let turns = vec![turn(0, 1000, "SPEAKER_00")];
        let out = assign_speakers(&p, &turns, 1);
        assert_eq!(out.captions[0].speaker_id, None);
    }

    #[test]
    fn roster_order_follows_time_not_label() {
        // SPEAKER_01 speaks first in time → becomes Taler 1.
        let p = project_with(vec![caption("c1", 0, 1000)], vec![]);
        let turns = vec![turn(2000, 3000, "SPEAKER_00"), turn(0, 1000, "SPEAKER_01")];
        let out = assign_speakers(&p, &turns, 1);
        assert_eq!(out.speakers[0].id, "spk:0");
        // first-by-time is SPEAKER_01, mapped to spk:0
        assert_eq!(out.captions[0].speaker_id.as_deref(), Some("spk:0"));
    }

    // ── merge / rename / colour ────────────────────────────────────────────────
    #[test]
    fn merge_reattributes_and_drops_speaker() {
        let mut p = project_with(
            vec![caption("c1", 0, 1000), caption("c2", 1000, 2000)],
            vec![
                Speaker {
                    id: "spk:0".into(),
                    display_name: "Taler 1".into(),
                    color_hex: None,
                },
                Speaker {
                    id: "spk:1".into(),
                    display_name: "Taler 2".into(),
                    color_hex: None,
                },
            ],
        );
        p.captions[0].speaker_id = Some("spk:0".into());
        p.captions[1].speaker_id = Some("spk:1".into());

        let out = merge_speakers(&p, "spk:0", "spk:1", 5).unwrap();
        assert_eq!(out.speakers.len(), 1);
        assert_eq!(
            out.captions[1].speaker_id.as_deref(),
            Some("spk:0"),
            "c2 reattributed"
        );
    }

    #[test]
    fn merge_validates_ids() {
        let p = project_with(
            vec![],
            vec![Speaker {
                id: "spk:0".into(),
                display_name: "T".into(),
                color_hex: None,
            }],
        );
        assert!(
            merge_speakers(&p, "spk:0", "spk:0", 1).is_err(),
            "self-merge"
        );
        assert!(
            merge_speakers(&p, "spk:0", "ghost", 1).is_err(),
            "missing remove"
        );
        assert!(
            merge_speakers(&p, "ghost", "spk:0", 1).is_err(),
            "missing keep"
        );
    }

    #[test]
    fn rename_and_color_update_roster() {
        let p = project_with(
            vec![],
            vec![Speaker {
                id: "spk:0".into(),
                display_name: "Taler 1".into(),
                color_hex: None,
            }],
        );
        let renamed = rename_speaker(&p, "spk:0", "Pastor Lars", 5).unwrap();
        assert_eq!(renamed.speakers[0].display_name, "Pastor Lars");
        assert!(rename_speaker(&p, "spk:0", "  ", 5).is_err());

        let colored = set_speaker_color(&p, "spk:0", "#FF0000", 5).unwrap();
        assert_eq!(colored.speakers[0].color_hex.as_deref(), Some("#FF0000"));
        assert!(set_speaker_color(&p, "ghost", "#FF0000", 5).is_err());
    }

    #[test]
    fn overlap_is_zero_when_disjoint() {
        assert_eq!(overlap_ms(0, 100, 200, 300), 0);
        assert_eq!(overlap_ms(0, 200, 100, 300), 100);
    }
}
