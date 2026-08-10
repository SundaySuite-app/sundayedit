//! Executable mirror parity — generate the fixture the TypeScript mirrors are
//! judged against (E8 hardening round).
//!
//! Three E1–E6 seams have TWO implementations of the same arithmetic, on
//! purpose (one in Rust because the export lives there, one in TypeScript
//! because the preview lives there):
//!
//! | seam      | Rust (truth)        | TypeScript (mirror)                         |
//! | --------- | ------------------- | ------------------------------------------- |
//! | karaoke   | `services::karaoke` | `src/features/timeline/karaoke.ts`          |
//! | tile grid | `services::tiles`   | `src/features/timeline/filmstrip.ts`        |
//! | effects   | `services::effects` | `src/features/timeline/effects/registry.ts` |
//!
//! Every existing guard on those seams is either a *unit test of one side* or
//! a *source-text assertion* about the other (`effects_registry_parity.rs`
//! greps registry.ts for the neutrality rule and the number formatter). Both
//! leave the classic seam hole open: two halves that each pass their own
//! tests and disagree on an input neither table happens to contain. Prose
//! ("keep these in lockstep") is not a test.
//!
//! So this test RUNS the Rust side over a deliberately nasty table — the
//! adversarial cases plus a fixed-seed random sweep — and freezes inputs *and*
//! outputs into `src/lib/__fixtures__/mirror-parity.json`.
//! `src/lib/mirrorParity.test.ts` then runs the TypeScript mirrors over the
//! identical inputs and asserts identical outputs. Both failure directions are
//! covered, whichever suite runs first:
//!
//!   * Rust behaviour changes → the committed fixture no longer matches → THIS
//!     test fails, telling you to regenerate.
//!   * TypeScript drifts from the frozen truth → the vitest side fails.
//!
//! Regenerate deliberately (and read the diff — it is the export changing):
//!
//! ```text
//! UPDATE_MIRROR_FIXTURE=1 cargo test --manifest-path src-tauri/Cargo.toml \
//!     --test mirror_fixture_parity
//! ```
//!
//! ── Why the records are pipe-encoded strings ────────────────────────────────
//! A golden file is read as a DIFF, and `serde_json::to_string_pretty` puts
//! every array element on its own line: the same table as nested objects came
//! to 1.7 MB, in which a one-word change is invisible. One record per line
//! keeps it ~40× smaller and makes a drifting field obvious. The encodings are
//! documented on each builder below and decoded by the vitest mirror.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::json;

use sundayedit_lib::model::{Caption, Effect, Word};
use sundayedit_lib::services::karaoke::{KaraokeWord, WordState};
use sundayedit_lib::services::{effects, karaoke, tiles};

// ── Fixture shape ───────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Fixture {
    /// Read by whoever is staring at an unexpected diff.
    note: &'static str,
    encoding: Encoding,
    karaoke: Vec<KaraokeCase>,
    tiles: TilesFixture,
    effects: Vec<EffectCase>,
}

#[derive(Serialize)]
struct Encoding {
    word_in: &'static str,
    word_out: &'static str,
    sample: &'static str,
    tile: &'static str,
}

#[derive(Serialize)]
struct KaraokeCase {
    name: String,
    caption_start_ms: i64,
    caption_end_ms: i64,
    /// Confidence threshold `uncertain_flags` was evaluated at.
    threshold: f32,
    /// Input words, `word_in`-encoded.
    words: Vec<String>,
    /// `karaoke_words` output, `word_out`-encoded.
    out: Vec<String>,
    /// `uncertain_flags`, one `0`/`1` per word (index-aligned with `out`).
    uncertain: String,
    /// `word_states_at` + `active_index_at`, `sample`-encoded.
    samples: Vec<String>,
}

#[derive(Serialize)]
struct TilesFixture {
    base_span_ms: i64,
    max_tier: u32,
    cols_default: u32,
    height_px: u32,
    cases: Vec<TileCase>,
}

#[derive(Serialize)]
struct TileCase {
    tier: u32,
    start_ms: i64,
    end_ms: i64,
    span_ms: i64,
    key: String,
    index_at_start: i64,
    range_at_index: (i64, i64),
    parent: Option<i64>,
    /// `tiles_covering`, `tile`-encoded.
    covering: Vec<String>,
}

#[derive(Serialize)]
struct EffectCase {
    kind: String,
    params: serde_json::Value,
    enabled: bool,
    /// `null` when the effect contributes nothing to the filtergraph.
    fragment: Option<String>,
}

// ── Deterministic randomness (the xorshift64* the karaoke unit tests use) ────

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> i64 {
        (self.next() % n.max(1)) as i64
    }
}

// ── Encoders ────────────────────────────────────────────────────────────────

/// `text|start_ms|end_ms|confidence|locked|edited` (flags as `0`/`1`).
fn enc_word_in(w: &Word) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}",
        w.text,
        w.start_ms,
        w.end_ms,
        w.confidence,
        u8::from(w.locked),
        u8::from(w.edited),
    )
}

/// `text|start_ms|end_ms|duration_cs|confidence`.
fn enc_word_out(k: &KaraokeWord) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        k.text, k.start_ms, k.end_ms, k.duration_cs, k.confidence,
    )
}

/// `t_ms|states|active` — `states` is one char per word (`p`/`a`/`d`),
/// `active` is `active_index_at` or `-` for none.
fn enc_sample(t_ms: i64, states: &[WordState], active: Option<usize>) -> String {
    let s: String = states
        .iter()
        .map(|st| match st {
            WordState::Pending => 'p',
            WordState::Active => 'a',
            WordState::Done => 'd',
        })
        .collect();
    let a = active.map(|i| i.to_string()).unwrap_or_else(|| "-".into());
    format!("{t_ms}|{s}|{a}")
}

/// `tier|index|start_ms|end_ms|key`.
fn enc_tile(t: &tiles::TileRef) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        t.tier, t.index, t.start_ms, t.end_ms, t.key
    )
}

// ── Table construction ──────────────────────────────────────────────────────

fn cap(start_ms: i64, end_ms: i64, words: Vec<Word>) -> Caption {
    Caption {
        id: "c".into(),
        start_ms,
        end_ms,
        words,
        speaker_id: None,
        style_id: None,
        notes: None,
        ai_generated: true,
        last_edited_at: 0,
        track_id: None,
    }
}

fn w(text: &str, start_ms: i64, end_ms: i64) -> Word {
    Word::new(text, start_ms, end_ms, 95.0)
}

/// The instants a caption's word states are sampled at: the caption's own
/// edges (the half-open boundaries are exactly where an off-by-one lives), one
/// instant either side, an even sweep across it, and every DERIVED cut point —
/// a word boundary is where the two implementations would disagree if one of
/// them wrote `<=` where the other wrote `<`.
fn sample_times(c: &Caption, words: &[KaraokeWord]) -> Vec<i64> {
    let end = c.end_ms.max(c.start_ms);
    let span = end - c.start_ms;
    let mut times = vec![c.start_ms - 1, c.start_ms, end - 1, end, end + 1];
    for k in 1..4 {
        times.push(c.start_ms + span * k / 4);
    }
    for kw in words {
        times.push(kw.start_ms - 1);
        times.push(kw.start_ms);
        times.push(kw.end_ms);
    }
    times.sort_unstable();
    times.dedup();
    times
}

fn karaoke_cases() -> Vec<(String, Caption, f32)> {
    let mut table: Vec<(String, Caption, f32)> = Vec::new();

    let mut trusted = vec![
        Word::new("sure", 0, 500, 95.0),
        Word::new("shaky", 500, 1_000, 40.0),
        Word::new("locked", 1_000, 1_500, 10.0),
        Word::new("edited", 1_500, 2_000, 5.0),
    ];
    trusted[2].locked = true;
    trusted[3].edited = true;

    let handcrafted: Vec<(&str, Caption, f32)> = vec![
        (
            "plain-three-words",
            cap(
                1_000,
                4_000,
                vec![
                    w("a", 1_000, 1_500),
                    w("b", 1_600, 2_500),
                    w("c", 2_600, 4_000),
                ],
            ),
            70.0,
        ),
        (
            "gap-absorbed-by-preceding-word",
            cap(
                1_000,
                4_000,
                vec![w("a", 1_000, 1_500), w("b", 3_000, 4_000)],
            ),
            70.0,
        ),
        (
            "sub-centisecond-boundaries",
            cap(
                0,
                1_000,
                (0..7).map(|i| w("x", i * 143, i * 143 + 143)).collect(),
            ),
            70.0,
        ),
        (
            "caption-start-off-the-centisecond-grid",
            cap(
                1_007,
                4_003,
                vec![w("a", 1_007, 2_001), w("b", 2_001, 4_003)],
            ),
            70.0,
        ),
        (
            "single-word",
            cap(500, 900, vec![w("only", 500, 900)]),
            70.0,
        ),
        (
            "zero-length-caption",
            cap(1_234, 1_234, vec![w("a", 1_234, 1_234)]),
            70.0,
        ),
        (
            "inverted-caption",
            cap(5_000, 1_000, vec![w("a", 5_000, 6_000)]),
            70.0,
        ),
        (
            "words-outside-the-caption",
            cap(
                1_000,
                2_000,
                vec![w("early", -5_000, 10), w("late", 9_000, 9_500)],
            ),
            70.0,
        ),
        (
            "zero-and-negative-word-spans",
            cap(
                0,
                3_000,
                vec![w("a", 500, 500), w("b", 2_000, 1_000), w("c", 2_500, 3_000)],
            ),
            70.0,
        ),
        (
            "out-of-order-words",
            cap(0, 2_000, vec![w("b", 1_500, 2_000), w("a", 200, 900)]),
            70.0,
        ),
        ("empty-word-list", cap(1_000, 2_500, vec![]), 70.0),
        (
            "negative-caption-start",
            cap(-500, 1_500, vec![w("a", -500, 0), w("b", 0, 1_500)]),
            70.0,
        ),
        (
            "nine-figure-timeline-position",
            cap(
                7_199_993,
                7_203_337,
                vec![w("a", 7_199_993, 7_201_111), w("b", 7_201_111, 7_203_337)],
            ),
            70.0,
        ),
        (
            "locked-and-edited-words-are-trusted",
            cap(0, 2_000, trusted),
            70.0,
        ),
        (
            "threshold-exactly-on-a-word",
            cap(0, 1_000, vec![Word::new("edge", 0, 1_000, 70.0)]),
            70.0,
        ),
        (
            "threshold-just-above-a-word",
            cap(0, 1_000, vec![Word::new("edge", 0, 1_000, 70.0)]),
            70.1,
        ),
    ];
    for (name, c, threshold) in handcrafted {
        table.push((name.to_string(), c, threshold));
    }

    // Fixed-seed sweep — deliberately sloppy timings (starts anywhere near the
    // caption, ends possibly before their own start): the shapes real ASR
    // produces on a bad passage. The seed is pinned like any other constant.
    let mut rng = Rng(0x5EED_1234_ABCD_0F01);
    for i in 0..48 {
        let start = rng.below(600_000);
        let span = rng.below(12_000);
        let n = rng.below(6) as usize + 1;
        let words: Vec<Word> = (0..n)
            .map(|k| {
                let ws = start + rng.below(span as u64 + 1) - 200;
                let we = ws + rng.below(900) - 300;
                let mut word = Word::new(format!("w{k}"), ws, we, rng.below(101) as f32);
                // Sprinkle trust flags so `uncertain_flags` is exercised too.
                word.locked = rng.below(7) == 0;
                word.edited = rng.below(7) == 0;
                word
            })
            .collect();
        table.push((
            format!("sweep-{i:02}"),
            cap(start, start + span, words),
            70.0,
        ));
    }

    table
}

fn karaoke_fixture() -> Vec<KaraokeCase> {
    karaoke_cases()
        .into_iter()
        .map(|(name, c, threshold)| {
            let out = karaoke::karaoke_words(&c);
            let times = sample_times(&c, &out);
            KaraokeCase {
                name,
                caption_start_ms: c.start_ms,
                caption_end_ms: c.end_ms,
                threshold,
                words: c.words.iter().map(enc_word_in).collect(),
                uncertain: karaoke::uncertain_flags(&c, threshold)
                    .into_iter()
                    .map(|f| if f { '1' } else { '0' })
                    .collect(),
                samples: times
                    .iter()
                    .map(|t| {
                        enc_sample(
                            *t,
                            &karaoke::word_states_at(&out, *t),
                            karaoke::active_index_at(&out, *t),
                        )
                    })
                    .collect(),
                out: out.iter().map(enc_word_out).collect(),
            }
        })
        .collect()
}

fn tile_cases() -> Vec<TileCase> {
    let mut cases = Vec::new();
    let mut push = |tier: u32, start_ms: i64, end_ms: i64| {
        let index = tiles::tile_index_at(start_ms, tier);
        cases.push(TileCase {
            tier,
            start_ms,
            end_ms,
            span_ms: tiles::tile_span_ms(tier),
            key: tiles::tile_key(tier, index),
            index_at_start: index,
            range_at_index: tiles::tile_range_ms(tier, index),
            parent: tiles::parent_tile(tier, index),
            covering: tiles::tiles_covering(start_ms, end_ms, tier)
                .iter()
                .map(enc_tile)
                .collect(),
        });
    };

    for tier in 0..=tiles::TILE_MAX_TIER + 1 {
        let span = tiles::tile_span_ms(tier);
        // Boundary-exact ranges: an end sitting exactly on a tile boundary must
        // not pull in the next tile — the classic fencepost.
        push(tier, 0, span);
        push(tier, 0, span + 1);
        push(tier, span, span * 2);
        push(tier, span - 1, span + 1);
        // Empty and inverted ranges.
        push(tier, span * 3, span * 3);
        push(tier, span * 5, span * 2);
        // Negative times clamp to tile 0.
        push(tier, -5_000, 1_000);
        push(tier, -50_000, -1_000);
        // Deep into a long service recording.
        push(tier, 7_199_993, 7_203_337);
    }

    let mut rng = Rng(0x0DDB_A11C_0FFE_E123);
    for _ in 0..48 {
        let tier = rng.below(tiles::TILE_MAX_TIER as u64 + 2) as u32;
        let start = rng.below(3_600_000) - 100_000;
        // Range length in TILES, not in ms: a fixed 400 s window at tier 8
        // (250 ms tiles) would freeze 1 600 rows per case and bloat the golden
        // file without testing anything the first dozen tiles don't. Negative
        // lengths are kept — an inverted range is a case in its own right.
        let end = start + tiles::tile_span_ms(tier) * (rng.below(13) - 1) + rng.below(500) - 250;
        push(tier, start, end);
    }
    cases
}

fn effect_cases() -> Vec<EffectCase> {
    let mut cases: Vec<(&str, serde_json::Value, bool)> = vec![
        // Unknown kinds, disabled effects, and garbage bags.
        ("bloom", json!({ "radius": 4 }), true),
        ("brightness", json!({ "amount": 0.5 }), false),
        ("brightness", json!({}), true),
        ("brightness", json!({ "amount": "loud" }), true),
        ("brightness", json!({ "amount": null }), true),
        ("grayscale", json!({}), true),
        ("grayscale", json!({ "amount": 5 }), true),
        ("grayscale", json!({}), false),
        // Values that stress the shared number formatter.
        ("brightness", json!({ "amount": 0.000_04 }), true),
        ("brightness", json!({ "amount": -0.000_04 }), true),
        ("brightness", json!({ "amount": 0.123_456_789 }), true),
        ("brightness", json!({ "amount": -0.123_456_789 }), true),
        ("contrast", json!({ "amount": 1e308 }), true),
        ("contrast", json!({ "amount": -1e308 }), true),
        ("saturation", json!({ "amount": 2.100_04 }), true),
        ("saturation", json!({ "amount": 2.000_01 }), true),
    ];

    // Sweep every curated effect across (and past) its declared range, on a
    // grid fine enough to hit the trailing-zero and clamping cases.
    for def in effects::CURATED {
        if let Some(p) = def.params.first() {
            for step in -2..=22 {
                let v = p.min + (p.max - p.min) * f64::from(step) / 20.0;
                cases.push((def.id, json!({ p.name: v }), true));
            }
        }
    }

    cases
        .into_iter()
        .map(|(kind, params, enabled)| {
            let e = Effect {
                id: format!("fx-{kind}"),
                kind: kind.into(),
                params: params.clone(),
                enabled,
            };
            EffectCase {
                fragment: effects::filter_fragment(&e),
                kind: kind.to_string(),
                params,
                enabled,
            }
        })
        .collect()
}

// ── The test ────────────────────────────────────────────────────────────────

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/lib/__fixtures__/mirror-parity.json")
}

fn build() -> Fixture {
    Fixture {
        note: "GENERATED by src-tauri/tests/mirror_fixture_parity.rs — do not hand-edit. \
               Inputs AND the Rust outputs the TypeScript mirrors must reproduce \
               (src/lib/mirrorParity.test.ts). Regenerate with UPDATE_MIRROR_FIXTURE=1 \
               cargo test --manifest-path src-tauri/Cargo.toml --test mirror_fixture_parity.",
        encoding: Encoding {
            word_in: "text|start_ms|end_ms|confidence|locked|edited",
            word_out: "text|start_ms|end_ms|duration_cs|confidence",
            sample: "t_ms|states (p=pending a=active d=done, one char per word)|active index or -",
            tile: "tier|index|start_ms|end_ms|key",
        },
        karaoke: karaoke_fixture(),
        tiles: TilesFixture {
            base_span_ms: tiles::TILE_BASE_SPAN_MS,
            max_tier: tiles::TILE_MAX_TIER,
            cols_default: tiles::TILE_COLS_DEFAULT,
            height_px: tiles::TILE_HEIGHT_PX,
            cases: tile_cases(),
        },
        effects: effect_cases(),
    }
}

#[test]
fn the_committed_mirror_fixture_matches_what_rust_produces_today() {
    let mut rendered = serde_json::to_string_pretty(&build()).expect("serialize fixture");
    rendered.push('\n');
    let path = fixture_path();

    if std::env::var_os("UPDATE_MIRROR_FIXTURE").is_some() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("create fixture dir");
        }
        std::fs::write(&path, &rendered).expect("write fixture");
        return;
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "read {}: {e}\nRegenerate with: UPDATE_MIRROR_FIXTURE=1 cargo test \
             --manifest-path src-tauri/Cargo.toml --test mirror_fixture_parity",
            path.display()
        )
    });

    if committed != rendered {
        // Point at the first differing line: a whole-file JSON diff in a panic
        // message is unreadable.
        let at = committed
            .lines()
            .zip(rendered.lines())
            .position(|(a, b)| a != b)
            .map(|i| i + 1)
            .unwrap_or_else(|| committed.lines().count().min(rendered.lines().count()) + 1);
        panic!(
            "src/lib/__fixtures__/mirror-parity.json is stale (first difference at line {at}).\n\
             A Rust change to karaoke/tiles/effects moved the shared truth. Regenerate:\n\
             \n    UPDATE_MIRROR_FIXTURE=1 cargo test --manifest-path src-tauri/Cargo.toml \
             --test mirror_fixture_parity\n\n\
             then run `npm test` — src/lib/mirrorParity.test.ts will say whether the \
             TypeScript mirror still agrees."
        );
    }
}

#[test]
fn the_fixture_actually_exercises_the_interesting_shapes() {
    // A parity fixture holding only easy cases is worse than none: it reads as
    // coverage. Assert the table keeps its teeth.
    let f = build();
    let cases = karaoke_cases();

    assert!(f.karaoke.len() >= 60, "karaoke table shrank");
    assert!(
        cases.iter().any(|(_, c, _)| c.words.is_empty()),
        "no empty-word-list case"
    );
    assert!(
        cases.iter().any(|(_, c, _)| c.end_ms < c.start_ms),
        "no inverted-caption case"
    );
    assert!(
        cases.iter().any(|(_, c, _)| c.start_ms < 0),
        "no negative-start case"
    );
    assert!(
        cases
            .iter()
            .any(|(_, c, _)| karaoke::karaoke_words(c).iter().any(|w| w.duration_cs == 0)),
        "no zero-duration word — the collapsed-span case is untested"
    );
    assert!(
        f.karaoke.iter().any(|c| c.uncertain.contains('1')),
        "no low-confidence word — the tint flag is untested"
    );
    assert!(
        f.karaoke
            .iter()
            .any(|c| c.samples.iter().any(|s| s.ends_with("|-"))),
        "no instant with NO active word — `active_index_at`'s None arm is untested"
    );

    assert!(
        f.tiles.cases.iter().any(|c| c.parent.is_none()),
        "no tier-0 case (the parent chain's root)"
    );
    assert!(
        f.tiles.cases.iter().any(|c| c.start_ms < 0),
        "no negative-time case"
    );
    assert!(
        f.tiles.cases.iter().any(|c| c.end_ms <= c.start_ms),
        "no empty/inverted range case"
    );
    assert!(
        f.tiles.cases.iter().any(|c| c.tier > tiles::TILE_MAX_TIER),
        "no over-max tier case (clamping is untested)"
    );

    assert!(
        f.effects.iter().any(|c| c.fragment.is_none() && c.enabled),
        "no enabled-but-silent effect case"
    );
    assert!(
        f.effects
            .iter()
            .any(|c| c.fragment.as_deref().is_some_and(|s| s.contains('-'))),
        "no negative-value fragment case"
    );
    for def in effects::CURATED {
        assert!(
            f.effects.iter().any(|c| c.kind == def.id),
            "curated effect `{}` is missing from the parity table",
            def.id
        );
    }
}
