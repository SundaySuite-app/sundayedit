//! Confidence calibration runner — Phase 2.3.
//!
//! Turns hand-labelled words into a precision/recall table so the confidence
//! tier boundaries can be chosen from data instead of guessed.
//!
//!   cargo run --example calibrate -- path/to/labeled.json
//!
//! Input JSON:
//!   { "words": [ { "confidence": 95.0, "correct": true }, ... ] }
//!
//! `confidence` is the 0..100 score SundayEdit assigned; `correct` is whether
//! the word matched ground truth on manual review. See docs/CALIBRATION.md
//! for the full procedure (and docs/calibration-sample.json for a demo).

use std::process::ExitCode;

use serde::Deserialize;
use sundayedit_lib::services::asr::calibration::{
    LabeledWord, best_f1, cohens_kappa, default_thresholds, precision_interval, recall_interval,
    sweep,
};

#[derive(Deserialize)]
struct Input {
    words: Vec<LabeledWord>,
    /// Optional second labeller's correct/incorrect calls, word-aligned with
    /// `words`, for inter-labeller agreement (Cohen's kappa). Present in the
    /// real multi-labeller set (`docs/calibration-real.json`); absent in the
    /// modelled set. See docs/CALIBRATION.md.
    #[serde(default)]
    labeller_b: Vec<bool>,
}

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: cargo run --example calibrate -- <labeled.json>");
        return ExitCode::FAILURE;
    };

    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("could not read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let input: Input = match serde_json::from_str(&raw) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("could not parse {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let total = input.words.len();
    let wrong = input.words.iter().filter(|w| !w.correct).count();
    println!(
        "{total} labelled words, {wrong} wrong ({:.1}% error rate)\n",
        pct(wrong, total)
    );

    let table = sweep(&input.words, &default_thresholds());
    println!("threshold  precision   recall      f1   flagged");
    println!("--------------------------------------------------");
    for m in &table {
        println!(
            "{:>8.0}   {:>8.3}  {:>8.3}  {:>6.3}  {:>7}",
            m.threshold, m.precision, m.recall, m.f1, m.flagged,
        );
    }

    if let Some(best) = best_f1(&table) {
        println!(
            "\nBest F1 at threshold {:.0}: precision {:.3}, recall {:.3}.",
            best.threshold, best.precision, best.recall,
        );
        println!(
            "→ Use ~{:.0} as the 'uncertain' cutoff and refit the stretch() \
             anchors in confidence.rs so it maps to the tier boundary you want \
             (see docs/CALIBRATION.md).",
            best.threshold,
        );
    }

    // 95 % Wilson intervals at the shipped tier-2 floor (70) — the headline the
    // app surfaces is a point estimate, but this is the honest range behind it.
    let r = recall_interval(&input.words, 70.0);
    let p = precision_interval(&input.words, 70.0);
    println!(
        "\nAt the tier-2 floor (conf < 70), 95% CI:\n  \
         recall    {:.3}  [{:.3}, {:.3}]\n  precision {:.3}  [{:.3}, {:.3}]",
        r.point, r.lower, r.upper, p.point, p.lower, p.upper,
    );

    // Inter-labeller agreement, when a second labeller's calls are present.
    if !input.labeller_b.is_empty() {
        let labeller_a: Vec<bool> = input.words.iter().map(|w| w.correct).collect();
        match cohens_kappa(&labeller_a, &input.labeller_b) {
            Some(k) => println!(
                "\nInter-labeller agreement (Cohen's kappa): {k:.3}  \
                 (>0.8 almost perfect, 0.6–0.8 substantial).",
            ),
            None => eprintln!(
                "\nlabeller_b present but not word-aligned with words ({} vs {}); \
                 skipping kappa.",
                input.labeller_b.len(),
                input.words.len(),
            ),
        }
    }

    ExitCode::SUCCESS
}

fn pct(n: usize, total: usize) -> f32 {
    if total == 0 {
        0.0
    } else {
        100.0 * n as f32 / total as f32
    }
}
