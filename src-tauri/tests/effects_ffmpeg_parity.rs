//! Real-ffmpeg parity for the curated clip effects (E6).
//!
//! `services::effects` emits the filter fragments and its unit tests pin the
//! STRINGS. Strings are not the risk: an option name that ffmpeg does not
//! accept aborts the whole export at render time, and a fragment that parses
//! but does the opposite of what the slider said is worse. So these tests take
//! the real `build_filter_complex` argv, run the bundled ffmpeg, ffprobe the
//! result, and then MEASURE it — `signalstats` reports the average luma
//! (`YAVG`) and saturation (`SATAVG`) of the rendered file, so each effect is
//! checked against the thing it is supposed to change:
//!
//! | effect            | fragment            | measured                       |
//! | ----------------- | ------------------- | ------------------------------ |
//! | brightness ±0.3   | `eq=brightness=…`   | YAVG up / down                 |
//! | contrast 2.0      | `eq=contrast=2`     | YAVG pushed AWAY from mid grey |
//! | saturation 2 / 0.2| `eq=saturation=…`   | SATAVG up / down               |
//! | grayscale         | `hue=s=0`           | SATAVG ≈ 0                     |
//!
//! The source is a uniform colour (`0x805030`), which makes every prediction
//! unambiguous: its luma sits below mid grey, so a contrast boost must DARKEN
//! it — a filter that silently did nothing, or that ran on the wrong plane,
//! cannot pass by accident.
//!
//! `#[ignore]`d like the other live compose tests. Run:
//!
//! ```sh
//! cargo test --manifest-path src-tauri/Cargo.toml \
//!   --test effects_ffmpeg_parity -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

use sundayedit_lib::model::{
    Caption, Effect, ExportConfig, MediaItem, Project, ProjectMeta, Style, TimelineItem,
    TimelineItemKind, Track, TrackKind, Transform,
};
use sundayedit_lib::services::burnin::{Encoder, VideoCodec};
use sundayedit_lib::services::compose::ComposeSettings;
use sundayedit_lib::services::video::{parse_ffprobe_json, MediaKind};

/// Test shim: the real builder is fallible (it refuses item kinds the compose
/// graph cannot render — see `compose::validate_composable`); every fixture in
/// this file is composable.
fn build_filter_complex(
    project: &Project,
    settings: &ComposeSettings,
    ass_file: Option<&str>,
    output: &str,
) -> Vec<String> {
    sundayedit_lib::services::compose::build_filter_complex(project, settings, ass_file, output)
        .expect("fixture must be composable")
}

// ── binaries ─────────────────────────────────────────────────────────────────

fn sidecar(stem: &str) -> String {
    let bindir = Path::new(env!("CARGO_MANIFEST_DIR")).join("binaries");
    for arch in [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "universal-apple-darwin",
    ] {
        let p = bindir.join(format!("{stem}-{arch}"));
        if p.exists() {
            return p.to_string_lossy().into_owned();
        }
    }
    stem.into()
}

fn ffmpeg() -> String {
    sidecar("ffmpeg")
}
fn ffprobe() -> String {
    sidecar("ffprobe")
}

// ── scaffolding ──────────────────────────────────────────────────────────────

const W: i32 = 320;
const H: i32 = 240;
const CLIP_MS: i64 = 1_000;

fn settings() -> ComposeSettings {
    ComposeSettings {
        width: W,
        height: H,
        fps: 30.0,
        codec: VideoCodec::H264,
        encoder: Encoder::Cpu,
        bitrate_kbps: None,
    }
}

fn project_with(effects: Vec<Effect>, src: &str) -> Project {
    let media = MediaItem {
        id: "m1".into(),
        path: src.into(),
        content_hash: "h".into(),
        kind: MediaKind::Video,
        duration_ms: CLIP_MS,
        width: W,
        height: H,
        fps: 30.0,
        has_audio: false,
        audio_wav_path: None,
        original_filename: "flat.mkv".into(),
        added_at: 0,
    };
    let track = Track {
        id: "v1".into(),
        kind: TrackKind::Video,
        name: "v1".into(),
        index: 0,
        enabled: true,
        locked: false,
        muted: false,
        solo: false,
        volume_db: 0.0,
    };
    let item = TimelineItem {
        id: "i0".into(),
        track_id: "v1".into(),
        kind: TimelineItemKind::Av,
        source_media_id: Some("m1".into()),
        in_ms: 0,
        out_ms: CLIP_MS,
        timeline_start_ms: 0,
        speed: 1.0,
        gain_db: 0.0,
        fade_in_ms: 0,
        fade_out_ms: 0,
        transform: Transform::default(),
        effects,
        transition_in: None,
        text: None,
        enabled: true,
        locked: false,
    };
    Project {
        id: "p".into(),
        name: "t".into(),
        video_path: "/nowhere.mp4".into(),
        video_content_hash: "other".into(),
        video_duration_ms: CLIP_MS,
        video_width: W,
        video_height: H,
        video_fps: 30.0,
        audio_wav_path: None,
        language: "no".into(),
        default_style: Style::broadcast_news(),
        context_description: None,
        captions: Vec::<Caption>::new(),
        speakers: vec![],
        glossary: vec![],
        clips: vec![],
        talk_summary: None,
        export_config: ExportConfig::default(),
        project_meta: ProjectMeta::default(),
        created_at: 0,
        updated_at: 0,
        media: vec![media],
        tracks: vec![track],
        timeline_items: vec![item],
    }
}

fn effect(kind: &str, params: serde_json::Value) -> Effect {
    Effect {
        id: format!("fx-{kind}"),
        kind: kind.into(),
        params,
        enabled: true,
    }
}

/// A flat, uniform-colour clip: every prediction below is then a single number,
/// not an average over a moving test pattern.
fn generate_flat_source(dst: &Path) {
    let status = Command::new(ffmpeg())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=0x805030:s={W}x{H}:r=30:d=1"),
            "-c:v",
            "ffv1",
            "-an",
        ])
        .arg(dst)
        .status()
        .expect("spawn ffmpeg (sample generation)");
    assert!(status.success(), "flat sample generation failed");
}

/// Render `effects` through the REAL compose builder and return the output path.
fn compose_with(effects: Vec<Effect>, src: &Path, out: &Path) -> Vec<String> {
    let p = project_with(effects, &src.to_string_lossy());
    let args = build_filter_complex(&p, &settings(), None, &out.to_string_lossy());
    let output = Command::new(ffmpeg())
        .args(&args)
        .output()
        .expect("spawn ffmpeg (compose)");
    assert!(
        output.status.success(),
        "compose failed.\nargv: {args:?}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(out.exists(), "compose wrote no file");
    args
}

/// The `-filter_complex` graph out of a compose argv (the only part two
/// renders of the same timeline to different files can be compared on).
fn graph(args: &[String]) -> Option<String> {
    args.iter()
        .position(|a| a == "-filter_complex")
        .map(|i| args[i + 1].clone())
}

/// One `signalstats` statistic of the FIRST frame of a rendered file.
fn stat(file: &Path, key: &str) -> f64 {
    let out = Command::new(ffmpeg())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(file)
        .args([
            "-vf",
            &format!("signalstats,metadata=print:key=lavfi.signalstats.{key}:file=-"),
            "-frames:v",
            "1",
            "-f",
            "null",
            "-",
        ])
        .output()
        .expect("spawn ffmpeg (signalstats)");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    let needle = format!("lavfi.signalstats.{key}=");
    let line = text
        .lines()
        .find(|l| l.contains(&needle))
        .unwrap_or_else(|| panic!("no {key} in signalstats output:\n{text}"));
    line.split('=')
        .nth(1)
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or_else(|| panic!("unparseable {key}: {line}"))
}

/// `ffprobe` the rendered file through the app's OWN probe parser — the same
/// path an import takes, so a broken render is caught as a broken file.
fn probe(file: &Path) -> sundayedit_lib::services::video::VideoMetadata {
    let out = Command::new(ffprobe())
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(file)
        .output()
        .expect("spawn ffprobe");
    parse_ffprobe_json(&String::from_utf8_lossy(&out.stdout)).expect("ffprobe json parses")
}

struct Fixture {
    dir: PathBuf,
    src: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("sundayedit_fx_{tag}_src.mkv"));
        let _ = std::fs::remove_file(&src);
        generate_flat_source(&src);
        Fixture { dir, src }
    }
    fn out(&self, tag: &str) -> PathBuf {
        let p = self.dir.join(format!("sundayedit_fx_{tag}.mp4"));
        let _ = std::fs::remove_file(&p);
        p
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "needs the bundled ffmpeg/ffprobe (generates its own sample)"]
fn every_curated_effect_renders_a_valid_file() {
    // The failure this catches: a fragment ffmpeg refuses to parse aborts the
    // ENTIRE export, not just the effect.
    let f = Fixture::new("valid");
    let cases: Vec<(&str, Effect)> = vec![
        ("brightness", effect("brightness", json!({ "amount": 0.3 }))),
        ("contrast", effect("contrast", json!({ "amount": 2.0 }))),
        ("saturation", effect("saturation", json!({ "amount": 2.0 }))),
        ("grayscale", effect("grayscale", json!({}))),
    ];
    for (tag, fx) in cases {
        let out = f.out(&format!("valid_{tag}"));
        let args = compose_with(vec![fx], &f.src, &out);
        let fc = graph(&args).expect("a filter_complex graph");
        assert!(
            fc.contains("eq=") || fc.contains("hue="),
            "`{tag}` emitted no eq/hue filter: {fc}"
        );

        let meta = probe(&out);
        assert_eq!(meta.width, W, "`{tag}` width");
        assert_eq!(meta.height, H, "`{tag}` height");
        assert!(meta.duration_ms > 0, "`{tag}` has no duration");
        let _ = std::fs::remove_file(&out);
    }
    let _ = std::fs::remove_file(&f.src);
}

#[test]
#[ignore = "needs the bundled ffmpeg (generates its own sample)"]
fn brightness_moves_the_luma_in_the_direction_the_slider_says() {
    let f = Fixture::new("bri");
    let base = f.out("bri_base");
    let up = f.out("bri_up");
    let down = f.out("bri_down");

    compose_with(vec![], &f.src, &base);
    compose_with(
        vec![effect("brightness", json!({ "amount": 0.3 }))],
        &f.src,
        &up,
    );
    compose_with(
        vec![effect("brightness", json!({ "amount": -0.3 }))],
        &f.src,
        &down,
    );

    let (y0, yup, ydown) = (stat(&base, "YAVG"), stat(&up, "YAVG"), stat(&down, "YAVG"));
    assert!(
        yup > y0 + 20.0,
        "+0.3 brightness must brighten: {y0} → {yup}"
    );
    assert!(
        ydown < y0 - 20.0,
        "-0.3 brightness must darken: {y0} → {ydown}"
    );

    for p in [&f.src, &base, &up, &down] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
#[ignore = "needs the bundled ffmpeg (generates its own sample)"]
fn contrast_pushes_the_source_away_from_mid_grey() {
    // The sample's luma sits BELOW mid grey, so `vf_eq`'s
    // `v = contrast*(v-0.5)+0.5` must darken it. A no-op filter, or one applied
    // to the wrong plane, lands on "unchanged" and fails.
    let f = Fixture::new("con");
    let base = f.out("con_base");
    let hi = f.out("con_hi");
    let lo = f.out("con_lo");

    compose_with(vec![], &f.src, &base);
    compose_with(
        vec![effect("contrast", json!({ "amount": 2.0 }))],
        &f.src,
        &hi,
    );
    compose_with(
        vec![effect("contrast", json!({ "amount": 0.5 }))],
        &f.src,
        &lo,
    );

    let (y0, yhi, ylo) = (stat(&base, "YAVG"), stat(&hi, "YAVG"), stat(&lo, "YAVG"));
    assert!(y0 < 128.0, "fixture must sit below mid grey, got {y0}");
    assert!(yhi < y0 - 10.0, "contrast 2.0 must darken it: {y0} → {yhi}");
    assert!(
        ylo > y0 + 10.0,
        "contrast 0.5 must pull it toward mid grey: {y0} → {ylo}"
    );

    for p in [&f.src, &base, &hi, &lo] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
#[ignore = "needs the bundled ffmpeg (generates its own sample)"]
fn saturation_moves_the_chroma_in_the_direction_the_slider_says() {
    let f = Fixture::new("sat");
    let base = f.out("sat_base");
    let up = f.out("sat_up");
    let down = f.out("sat_down");

    compose_with(vec![], &f.src, &base);
    compose_with(
        vec![effect("saturation", json!({ "amount": 2.0 }))],
        &f.src,
        &up,
    );
    compose_with(
        vec![effect("saturation", json!({ "amount": 0.2 }))],
        &f.src,
        &down,
    );

    let (s0, sup, sdown) = (
        stat(&base, "SATAVG"),
        stat(&up, "SATAVG"),
        stat(&down, "SATAVG"),
    );
    assert!(sup > s0 + 5.0, "saturation 2.0 must saturate: {s0} → {sup}");
    assert!(
        sdown < s0 - 5.0,
        "saturation 0.2 must desaturate: {s0} → {sdown}"
    );

    for p in [&f.src, &base, &up, &down] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
#[ignore = "needs the bundled ffmpeg (generates its own sample)"]
fn grayscale_removes_the_chroma_entirely() {
    let f = Fixture::new("gray");
    let base = f.out("gray_base");
    let gray = f.out("gray_out");

    compose_with(vec![], &f.src, &base);
    compose_with(vec![effect("grayscale", json!({}))], &f.src, &gray);

    let s0 = stat(&base, "SATAVG");
    let s1 = stat(&gray, "SATAVG");
    assert!(s0 > 10.0, "fixture must start colourful, got {s0}");
    assert!(s1 < 3.0, "`hue=s=0` must remove the colour, got {s1}");
    // Luma survives — this is a desaturation, not a fade to black.
    assert!(
        (stat(&gray, "YAVG") - stat(&base, "YAVG")).abs() < 6.0,
        "grayscale must preserve brightness"
    );

    for p in [&f.src, &base, &gray] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
#[ignore = "needs the bundled ffmpeg (generates its own sample)"]
fn a_non_curated_effect_renders_exactly_like_no_effect_at_all() {
    // Rule 1 of the registry: an unknown kind must be INERT, never an invented
    // filter name that aborts the render. Proven against real ffmpeg, not just
    // against the emitted string.
    let f = Fixture::new("unknown");
    let base = f.out("unknown_base");
    let with = f.out("unknown_with");

    let base_args = compose_with(vec![], &f.src, &base);
    let with_args = compose_with(vec![effect("bloom", json!({ "radius": 8 }))], &f.src, &with);

    assert_eq!(
        graph(&base_args),
        graph(&with_args),
        "an unknown effect changed the filtergraph"
    );
    assert_eq!(stat(&base, "YAVG"), stat(&with, "YAVG"));
    assert_eq!(stat(&base, "SATAVG"), stat(&with, "SATAVG"));

    for p in [&f.src, &base, &with] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
#[ignore = "needs the bundled ffmpeg (generates its own sample)"]
fn a_neutral_effect_renders_exactly_like_no_effect_at_all() {
    // Rule 2: enabling an effect and leaving it alone must not change a byte.
    let f = Fixture::new("neutral");
    let base = f.out("neutral_base");
    let with = f.out("neutral_with");

    let base_args = compose_with(vec![], &f.src, &base);
    let with_args = compose_with(
        vec![
            effect("brightness", json!({ "amount": 0.0 })),
            effect("contrast", json!({ "amount": 1.0 })),
            effect("saturation", json!({ "amount": 1.0 })),
        ],
        &f.src,
        &with,
    );
    assert_eq!(
        graph(&base_args),
        graph(&with_args),
        "neutral effects changed the filtergraph"
    );
    assert_eq!(stat(&base, "YAVG"), stat(&with, "YAVG"));

    for p in [&f.src, &base, &with] {
        let _ = std::fs::remove_file(p);
    }
}

#[test]
#[ignore = "needs the bundled ffmpeg (generates its own sample)"]
fn a_stack_of_effects_applies_in_order() {
    // brightness −0.3 then grayscale: darker AND colourless. If the chain were
    // dropped after the first fragment, one of the two assertions fails.
    let f = Fixture::new("stack");
    let base = f.out("stack_base");
    let out = f.out("stack_out");

    compose_with(vec![], &f.src, &base);
    compose_with(
        vec![
            effect("brightness", json!({ "amount": -0.3 })),
            effect("grayscale", json!({})),
        ],
        &f.src,
        &out,
    );

    assert!(stat(&out, "YAVG") < stat(&base, "YAVG") - 20.0);
    assert!(stat(&out, "SATAVG") < 3.0);

    for p in [&f.src, &base, &out] {
        let _ = std::fs::remove_file(p);
    }
}
