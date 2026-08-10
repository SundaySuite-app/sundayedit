//! Curated clip effects — the ONE place the GPU preview and the ffmpeg export
//! agree on what an effect *is* (programme stage E6,
//! docs/OSS-INTEGRATION-PROGRAM.md; ADR-013).
//!
//! `TimelineItem.effects` has always been an open bag (`kind` + opaque JSON
//! `params`). Opening it to a 233-entry third-party catalogue would break the
//! architecture invariant that **ffmpeg `filter_complex` is the export truth**:
//! the preview would show things the export cannot render. So the bag stays
//! open in the *model* and is narrowed here to a **curated registry** — the
//! effects that BOTH sides can produce:
//!
//! | id           | params                     | ffmpeg fragment      |
//! | ------------ | -------------------------- | -------------------- |
//! | `brightness` | `amount` −1…1 (default 0)  | `eq=brightness=<a>`  |
//! | `contrast`   | `amount` 0…3 (default 1)   | `eq=contrast=<a>`    |
//! | `saturation` | `amount` 0…3 (default 1)   | `eq=saturation=<a>`  |
//! | `grayscale`  | —                          | `hue=s=0`            |
//!
//! The ranges are `vf_eq`'s own: `brightness` is ADDITIVE on the luma LUT
//! (`v = contrast*(v-0.5) + 0.5 + brightness`, libavfilter/vf_eq.c) and lives
//! in −1…1; `contrast` and `saturation` are multiplicative around neutral 1.0
//! (`vf_eq` accepts −1000…1000, but anything past 3 is a novelty, so the UI
//! range stops there and every value is CLAMPED here as well as in the op).
//!
//! Three rules keep the seam honest:
//!
//! 1. **Unknown kinds emit nothing.** A hand-edited project file carrying
//!    `kind: "bloom"` renders exactly as it does today (no filter), it does not
//!    abort the export with an invented filter name — the same failure mode
//!    `compose::xfade_transition_name` guards on the transition side.
//! 2. **Neutral emits nothing.** An enabled effect parked at its default value
//!    produces the identical filtergraph to no effect at all, so "I turned it
//!    on and left it alone" cannot change a single output byte.
//! 3. **Out-of-range clamps, never rejects** — the house rule from
//!    `timeline_ops` (a drag past a neighbour stops at the gap).
//!
//! The TypeScript mirror is `src/features/timeline/effects/registry.ts` (Pixi
//! uniforms for the preview, the same ids/ranges/fragments), and the two are
//! pinned against each other by `src-tauri/tests/effects_registry_parity.rs`.

use crate::model::Effect;

/// One tunable parameter of a curated effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamDef {
    /// Key inside `Effect.params`.
    pub name: &'static str,
    pub min: f64,
    pub max: f64,
    /// The value at which the effect is a no-op (and emits no filter).
    pub default: f64,
}

/// One curated effect: an id, its parameters, and — via `filter_fragment` —
/// the ffmpeg filter it lowers to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectDef {
    pub id: &'static str,
    pub params: &'static [ParamDef],
}

/// Every effect the UI may offer and the export can render. Anything not in
/// this list is not selectable (frontend) and emits no filter (here).
pub const CURATED: &[EffectDef] = &[
    EffectDef {
        id: "brightness",
        params: &[ParamDef {
            name: "amount",
            min: -1.0,
            max: 1.0,
            default: 0.0,
        }],
    },
    EffectDef {
        id: "contrast",
        params: &[ParamDef {
            name: "amount",
            min: 0.0,
            max: 3.0,
            default: 1.0,
        }],
    },
    EffectDef {
        id: "saturation",
        params: &[ParamDef {
            name: "amount",
            min: 0.0,
            max: 3.0,
            default: 1.0,
        }],
    },
    EffectDef {
        id: "grayscale",
        params: &[],
    },
];

/// The curated definition for `kind`, or `None` for anything we don't render.
pub fn definition(kind: &str) -> Option<&'static EffectDef> {
    CURATED.iter().find(|d| d.id == kind)
}

/// Is this effect kind one the UI may offer and the export can render?
pub fn is_curated(kind: &str) -> bool {
    definition(kind).is_some()
}

/// Read one parameter out of an effect's opaque JSON bag, clamped to its
/// declared range. A missing / non-numeric / non-finite value falls back to
/// the neutral default rather than poisoning the filtergraph with `NaN`.
pub fn param(effect: &Effect, def: &ParamDef) -> f64 {
    let raw = effect
        .params
        .get(def.name)
        .and_then(|v| v.as_f64())
        .filter(|v| v.is_finite())
        .unwrap_or(def.default);
    raw.clamp(def.min, def.max)
}

/// Format a parameter for an ffmpeg filter argument: fixed notation, at most
/// four decimals, no trailing zeros, and never the string `-0` (ffmpeg parses
/// it, but it reads like a bug in an argv dump). Locale-independent by
/// construction — Rust's float formatting always uses `.`.
fn fmt(v: f64) -> String {
    let mut s = format!("{v:.4}");
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if s == "-0" {
        s = "0".to_string();
    }
    s
}

/// The ffmpeg filter fragment for ONE effect, or `None` when it contributes
/// nothing: disabled, an unknown kind, or every parameter still neutral.
///
/// This is the export half of the parity contract — the TypeScript registry's
/// `ffmpegFragment` must return the identical string for the identical input.
pub fn filter_fragment(effect: &Effect) -> Option<String> {
    if !effect.enabled {
        return None;
    }
    let def = definition(&effect.kind)?;
    match def.id {
        "brightness" => {
            let a = param(effect, &def.params[0]);
            (a != def.params[0].default).then(|| format!("eq=brightness={}", fmt(a)))
        }
        "contrast" => {
            let a = param(effect, &def.params[0]);
            (a != def.params[0].default).then(|| format!("eq=contrast={}", fmt(a)))
        }
        "saturation" => {
            let a = param(effect, &def.params[0]);
            (a != def.params[0].default).then(|| format!("eq=saturation={}", fmt(a)))
        }
        // No parameters: enabling it IS the change.
        "grayscale" => Some("hue=s=0".to_string()),
        // Unreachable while `CURATED` and this match agree — the test
        // `every_curated_effect_emits_a_fragment` is what keeps them agreeing.
        _ => None,
    }
}

/// Append the filter fragments for a clip's effects to its per-item chain, in
/// the order the user stacked them. Called by `compose::build_filter_complex`.
pub fn effect_filters(effects: &[Effect], chain: &mut Vec<String>) {
    for e in effects {
        if let Some(f) = filter_fragment(e) {
            chain.push(f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn effect(kind: &str, params: serde_json::Value) -> Effect {
        Effect {
            id: format!("fx-{kind}"),
            kind: kind.into(),
            params,
            enabled: true,
        }
    }

    #[test]
    fn brightness_emits_eq_brightness() {
        let f = filter_fragment(&effect("brightness", json!({ "amount": 0.25 })));
        assert_eq!(f.as_deref(), Some("eq=brightness=0.25"));
    }

    #[test]
    fn negative_brightness_keeps_its_sign() {
        let f = filter_fragment(&effect("brightness", json!({ "amount": -0.4 })));
        assert_eq!(f.as_deref(), Some("eq=brightness=-0.4"));
    }

    #[test]
    fn contrast_emits_eq_contrast() {
        let f = filter_fragment(&effect("contrast", json!({ "amount": 1.5 })));
        assert_eq!(f.as_deref(), Some("eq=contrast=1.5"));
    }

    #[test]
    fn saturation_emits_eq_saturation() {
        let f = filter_fragment(&effect("saturation", json!({ "amount": 0.5 })));
        assert_eq!(f.as_deref(), Some("eq=saturation=0.5"));
    }

    #[test]
    fn grayscale_emits_hue_s_zero() {
        let f = filter_fragment(&effect("grayscale", json!({})));
        assert_eq!(f.as_deref(), Some("hue=s=0"));
    }

    #[test]
    fn disabled_effect_emits_nothing() {
        let mut e = effect("brightness", json!({ "amount": 0.5 }));
        e.enabled = false;
        assert_eq!(filter_fragment(&e), None);
    }

    #[test]
    fn unknown_kind_emits_nothing() {
        // The whole point of rule 1: a project file from a future (or hand-
        // edited) build must render, not abort with an invented filter name.
        assert_eq!(filter_fragment(&effect("bloom", json!({ "x": 1 }))), None);
        assert!(!is_curated("bloom"));
    }

    #[test]
    fn neutral_values_emit_nothing() {
        assert_eq!(
            filter_fragment(&effect("brightness", json!({ "amount": 0.0 }))),
            None
        );
        assert_eq!(
            filter_fragment(&effect("contrast", json!({ "amount": 1.0 }))),
            None
        );
        assert_eq!(
            filter_fragment(&effect("saturation", json!({ "amount": 1.0 }))),
            None
        );
    }

    #[test]
    fn missing_or_garbage_params_fall_back_to_neutral() {
        assert_eq!(filter_fragment(&effect("brightness", json!({}))), None);
        assert_eq!(
            filter_fragment(&effect("contrast", json!({ "amount": "loud" }))),
            None
        );
        // NaN/Infinity cannot survive JSON, but a huge finite value can —
        // clamping is what stops it reaching ffmpeg.
        assert_eq!(
            filter_fragment(&effect("contrast", json!({ "amount": 1e308 }))).as_deref(),
            Some("eq=contrast=3")
        );
    }

    #[test]
    fn out_of_range_clamps_to_the_declared_bounds() {
        assert_eq!(
            filter_fragment(&effect("brightness", json!({ "amount": 9.0 }))).as_deref(),
            Some("eq=brightness=1")
        );
        assert_eq!(
            filter_fragment(&effect("brightness", json!({ "amount": -9.0 }))).as_deref(),
            Some("eq=brightness=-1")
        );
        assert_eq!(
            filter_fragment(&effect("saturation", json!({ "amount": -2.0 }))).as_deref(),
            Some("eq=saturation=0")
        );
    }

    #[test]
    fn values_are_formatted_without_float_noise() {
        // 0.1_f32 widened to f64 is 0.10000000149011612 — a naive `{}` would
        // put all of that in the argv.
        let e = effect("brightness", json!({ "amount": 0.1_f32 }));
        assert_eq!(filter_fragment(&e).as_deref(), Some("eq=brightness=0.1"));
    }

    #[test]
    fn effect_filters_appends_in_order_and_skips_the_silent_ones() {
        let effects = vec![
            effect("brightness", serde_json::json!({ "amount": 0.2 })),
            effect("bloom", serde_json::json!({})),
            effect("saturation", serde_json::json!({ "amount": 1.0 })),
            effect("grayscale", serde_json::json!({})),
        ];
        let mut chain = vec!["trim=start=0:end=1".to_string()];
        effect_filters(&effects, &mut chain);
        assert_eq!(
            chain,
            vec!["trim=start=0:end=1", "eq=brightness=0.2", "hue=s=0"]
        );
    }

    #[test]
    fn every_curated_effect_emits_a_fragment_when_pushed_off_neutral() {
        // Guards the `match` above against a registry entry that was added but
        // never lowered — the exact shape of a seam bug (both halves fine, the
        // seam silent).
        for def in CURATED {
            let params = match def.params.first() {
                Some(p) => json!({ p.name: p.max }),
                None => json!({}),
            };
            let f = filter_fragment(&effect(def.id, params));
            assert!(
                f.is_some(),
                "curated effect `{}` lowers to no ffmpeg filter",
                def.id
            );
        }
    }

    #[test]
    fn curated_ids_are_unique() {
        let mut ids: Vec<&str> = CURATED.iter().map(|d| d.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate id in CURATED");
    }

    #[test]
    fn every_param_default_sits_inside_its_range() {
        for def in CURATED {
            for p in def.params {
                assert!(p.min <= p.default && p.default <= p.max, "{}", def.id);
            }
        }
    }
}
