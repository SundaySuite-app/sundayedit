//! Seam guard: the curated effect registry has TWO implementations.
//!
//! `src-tauri/src/services/effects.rs` decides what the EXPORT renders;
//! `src/features/timeline/effects/registry.ts` decides what the INSPECTOR
//! offers and what the Pixi preview draws. They are deliberately separate
//! (one is the ffmpeg filtergraph, the other a WebGL colour matrix) — which
//! makes them the exact shape of a seam bug: both halves individually correct,
//! silently disagreeing about ids, ranges, defaults or emitted syntax, with
//! green tests on both sides.
//!
//! This is the sibling of `compose_xfade_vocabulary.rs`, which pins the
//! transition picker to ffmpeg's `xfade` enum for the same reason. It reads
//! the TypeScript literal out of the ACTUAL frontend source, so an effect
//! added to one side and forgotten on the other fails here.
//!
//! Not covered here (deliberately): the Pixi colour matrices. Those are an
//! approximation of `vf_eq`'s YUV model in RGB, documented as such in
//! registry.ts and ADR-013, and asserted numerically by registry.test.ts.
//! What must be exact — ids, ranges, defaults, neutrality, and the emitted
//! ffmpeg fragment — is exact here.

use std::path::Path;

use regex::Regex;
use serde_json::json;

use sundayedit_lib::model::Effect;
use sundayedit_lib::services::effects::{self, CURATED};

#[derive(Debug, PartialEq)]
struct TsParam {
    name: String,
    min: f64,
    max: f64,
    default: f64,
}

#[derive(Debug)]
struct TsEffect {
    id: String,
    params: Vec<TsParam>,
}

fn registry_source() -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/features/timeline/effects/registry.ts");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Parse the `CURATED_EFFECTS` array literal out of registry.ts. Tolerates
/// whitespace and prettier reflow, but expects the shape documented in that
/// file: a flat array of `{ id, labelKey, params: [{ name, min, max, step,
/// default }] }` object literals with plain numeric fields.
fn ts_registry() -> Vec<TsEffect> {
    let src = registry_source();
    let start = src
        .find("export const CURATED_EFFECTS")
        .expect("registry.ts exports CURATED_EFFECTS");
    // `= [`, not the first `[` — the type annotation `readonly EffectDef[]`
    // sits between the name and the literal.
    let open = src[start..].find("= [").expect("array literal") + start + 2;
    // Walk to the matching bracket so a nested `params: [...]` can't end it early.
    let mut depth = 0usize;
    let mut close = open;
    for (i, ch) in src[open..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = open + i;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(close > open, "unterminated CURATED_EFFECTS literal");
    let body = &src[open + 1..close];

    let id_re = Regex::new(r#"id:\s*"([a-z]+)""#).unwrap();
    let params_re = Regex::new(r"params:\s*\[([^\]]*)\]").unwrap();
    let param_re = Regex::new(
        r#"\{\s*name:\s*"([a-z]+)"\s*,\s*min:\s*(-?[0-9.]+)\s*,\s*max:\s*(-?[0-9.]+)\s*,\s*step:\s*(-?[0-9.]+)\s*,\s*default:\s*(-?[0-9.]+)\s*,?\s*\}"#,
    )
    .unwrap();

    let ids: Vec<(usize, String)> = id_re
        .captures_iter(body)
        .map(|c| (c.get(0).unwrap().start(), c[1].to_string()))
        .collect();
    assert!(
        ids.iter().any(|(_, id)| id == "brightness"),
        "registry.ts parse broke — no ids found"
    );

    let mut out = Vec::new();
    for (n, (at, id)) in ids.iter().enumerate() {
        let end = ids.get(n + 1).map(|(a, _)| *a).unwrap_or(body.len());
        let block = &body[*at..end];
        let params = params_re
            .captures(block)
            .map(|c| {
                param_re
                    .captures_iter(c.get(1).unwrap().as_str())
                    .map(|p| TsParam {
                        name: p[1].to_string(),
                        min: p[2].parse().unwrap(),
                        max: p[3].parse().unwrap(),
                        default: p[5].parse().unwrap(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(TsEffect {
            id: id.clone(),
            params,
        });
    }
    out
}

#[test]
fn the_two_registries_declare_the_same_effects_in_the_same_order() {
    let ts = ts_registry();
    let rust: Vec<&str> = CURATED.iter().map(|d| d.id).collect();
    let ts_ids: Vec<&str> = ts.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ts_ids, rust,
        "src/features/timeline/effects/registry.ts and \
         src-tauri/src/services/effects.rs disagree about which effects exist"
    );
}

#[test]
fn the_two_registries_declare_the_same_ranges_and_defaults() {
    for (ts, rust) in ts_registry().iter().zip(CURATED) {
        assert_eq!(
            ts.params.len(),
            rust.params.len(),
            "`{}` has a different parameter count on each side",
            ts.id
        );
        for (p, r) in ts.params.iter().zip(rust.params) {
            assert_eq!(p.name, r.name, "`{}` parameter name", ts.id);
            assert_eq!(p.min, r.min, "`{}`.{} min", ts.id, p.name);
            assert_eq!(p.max, r.max, "`{}`.{} max", ts.id, p.name);
            assert_eq!(p.default, r.default, "`{}`.{} default", ts.id, p.name);
        }
    }
}

#[test]
fn the_ts_mirror_emits_the_same_ffmpeg_fragment_as_the_export() {
    // Sample each effect across its declared range (plus deliberately illegal
    // values) and compare against the strings registry.ts is expected to
    // produce for the same inputs. registry.test.ts asserts the TypeScript
    // half against these very strings, so a change on either side that isn't
    // matched on the other lands as a failure in one of the two suites.
    let cases: &[(&str, serde_json::Value, Option<&str>)] = &[
        (
            "brightness",
            json!({ "amount": 0.25 }),
            Some("eq=brightness=0.25"),
        ),
        (
            "brightness",
            json!({ "amount": -0.4 }),
            Some("eq=brightness=-0.4"),
        ),
        ("brightness", json!({ "amount": 0.0 }), None),
        (
            "brightness",
            json!({ "amount": 9.0 }),
            Some("eq=brightness=1"),
        ),
        (
            "brightness",
            json!({ "amount": -9.0 }),
            Some("eq=brightness=-1"),
        ),
        (
            "contrast",
            json!({ "amount": 1.5 }),
            Some("eq=contrast=1.5"),
        ),
        ("contrast", json!({ "amount": 1.0 }), None),
        (
            "contrast",
            json!({ "amount": 1e308 }),
            Some("eq=contrast=3"),
        ),
        (
            "saturation",
            json!({ "amount": 0.5 }),
            Some("eq=saturation=0.5"),
        ),
        ("saturation", json!({ "amount": 1.0 }), None),
        (
            "saturation",
            json!({ "amount": -2.0 }),
            Some("eq=saturation=0"),
        ),
        ("grayscale", json!({}), Some("hue=s=0")),
        ("bloom", json!({ "radius": 4 }), None),
    ];
    for (kind, params, expected) in cases {
        let e = Effect {
            id: format!("fx-{kind}"),
            kind: (*kind).into(),
            params: params.clone(),
            enabled: true,
        };
        assert_eq!(
            effects::filter_fragment(&e).as_deref(),
            *expected,
            "`{kind}` with {params}"
        );
    }
}

#[test]
fn the_ts_mirror_uses_the_same_neutrality_rule() {
    // Both sides must agree that "enabled but at its default" contributes
    // NOTHING — otherwise the inspector shows an effect that the export
    // silently drops (or the reverse).
    for def in CURATED {
        let Some(p) = def.params.first() else {
            continue;
        };
        let e = Effect {
            id: format!("fx-{}", def.id),
            kind: def.id.into(),
            params: json!({ p.name: p.default }),
            enabled: true,
        };
        assert_eq!(
            effects::filter_fragment(&e),
            None,
            "`{}` at its default must emit no filter",
            def.id
        );
    }
    let src = registry_source();
    assert!(
        src.contains("if (a === p.default) return null;"),
        "registry.ts must keep the same neutral-emits-nothing rule"
    );
}

#[test]
fn the_ts_mirror_keeps_the_locale_independent_number_format() {
    // `toLocaleString` in a Norwegian locale renders 0.25 as "0,25", which
    // ffmpeg parses as a DIFFERENT (comma-separated) option list. Pin the
    // formatter the mirror uses.
    let src = registry_source();
    assert!(
        src.contains("v.toFixed(4)"),
        "registry.ts must format ffmpeg numbers with toFixed, not toLocaleString"
    );
    assert!(
        !src.contains(".toLocaleString("),
        "registry.ts must never use toLocaleString for a filter argument"
    );
}

#[test]
fn the_inspector_only_offers_curated_effects() {
    // The UI half of "non-curated effects must not be selectable": the panel
    // renders the registry, it does not carry its own hard-coded list.
    let tsx =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/features/timeline/ClipInspector.tsx");
    let src =
        std::fs::read_to_string(&tsx).unwrap_or_else(|e| panic!("read {}: {e}", tsx.display()));
    assert!(
        src.contains("CURATED_EFFECTS"),
        "ClipInspector must render the curated registry"
    );
    for def in CURATED {
        assert!(
            !src.contains(&format!("\"{}\"", def.id)),
            "ClipInspector hard-codes the effect id `{}` instead of reading the \
             registry — that is how the two lists drift apart",
            def.id
        );
    }
}
