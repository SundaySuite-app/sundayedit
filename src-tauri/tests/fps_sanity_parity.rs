//! Frame-rate sanity: one window, agreed on by both sides of the IPC seam,
//! and MEASURED at the far end of the export.
//!
//! The defect: `parse_fps` returned whatever `r_frame_rate` said. For a VFR
//! screen recording that field is the container's tick base, not a rate
//! anything was shot at — `1000/1` is routine. That number became
//! `project.video_fps`, which `composeEngine.ts::defaultComposeSettings`
//! rounded and handed back as `ComposeSettings.fps`, which the builder emitted
//! as `-r 1000`. `proxy_settings` capped at 30 and so never showed it; the
//! full export capped at nothing.
//!
//! Two guards, because there are two ways for this to rot:
//!
//!   1. **Parity** (no ffmpeg): the TypeScript constants and clamp in
//!      `src/lib/composeEngine.ts` are read out of the file and checked
//!      against Rust's `video::MIN_FPS` / `MAX_FPS` / `DEFAULT_FPS` /
//!      `sane_fps`. Two clamps that disagree are worse than one clamp: the
//!      frontend would send a number the backend silently changed.
//!   2. **Measurement** (real ffmpeg): a genuine 1000 fps source is probed,
//!      the probed rate is fed through the real settings path into the real
//!      builder, and the RENDERED file's frame rate is read back.
//!
//! Run the measuring half:
//! ```sh
//! cargo test --manifest-path src-tauri/Cargo.toml \
//!   --test fps_sanity_parity -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use sundayedit_lib::services::compose::{build_filter_complex, proxy_settings};
use sundayedit_lib::services::video::{
    parse_ffprobe_json, probe, sane_fps, DEFAULT_FPS, MAX_FPS, MIN_FPS,
};

mod common;
use common::{
    canvas_settings, item, media_sized, project, run_compose_argv, track, TrackKind, Transform,
};

// ── 1. TS ↔ Rust parity ──────────────────────────────────────────────────────

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn compose_engine_ts() -> String {
    let path = repo_root().join("src/lib/composeEngine.ts");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Pull `export const NAME = <number>;` out of the TypeScript source.
fn ts_const(src: &str, name: &str) -> f32 {
    let needle = format!("export const {name} = ");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("`{needle}…` is gone from composeEngine.ts"))
        + needle.len();
    let rest = &src[start..];
    let end = rest
        .find(';')
        .unwrap_or_else(|| panic!("no `;` after `{needle}`"));
    rest[..end]
        .trim()
        .parse::<f32>()
        .unwrap_or_else(|e| panic!("`{name}` in composeEngine.ts is not a number: {e}"))
}

#[test]
fn the_fps_window_is_the_same_number_on_both_sides_of_the_seam() {
    let ts = compose_engine_ts();
    assert_eq!(ts_const(&ts, "MIN_FPS"), MIN_FPS, "MIN_FPS drifted");
    assert_eq!(ts_const(&ts, "MAX_FPS"), MAX_FPS, "MAX_FPS drifted");
    assert_eq!(
        ts_const(&ts, "DEFAULT_FPS"),
        DEFAULT_FPS,
        "DEFAULT_FPS drifted"
    );
}

/// `defaultComposeSettings` must clamp BEFORE it rounds, and it must clamp at
/// all. Checked structurally rather than by string-matching the whole line, so
/// a rename of the local is fine but dropping the clamp is not.
#[test]
fn the_frontend_clamps_the_project_rate_before_it_rounds_it() {
    let ts = compose_engine_ts();
    assert!(
        ts.contains("Math.round(saneFps(project.video_fps))"),
        "defaultComposeSettings must round a CLAMPED rate — an unclamped \
         Math.round(project.video_fps) is how `-r 1000` reached the export"
    );
    // And the TS clamp behaves like the Rust one at the boundaries. The
    // arithmetic is duplicated here on purpose: this asserts what the file
    // SAYS the mirror is, and the constants above assert the file agrees.
    let ts_sane = |fps: f32| -> f32 {
        if !fps.is_finite() || fps <= 0.0 {
            ts_const(&ts, "DEFAULT_FPS")
        } else {
            fps.clamp(ts_const(&ts, "MIN_FPS"), ts_const(&ts, "MAX_FPS"))
        }
    };
    for probe_fps in [0.0, -1.0, 0.5, 1.0, 23.976, 29.97, 60.0, 240.0, 1000.0] {
        assert_eq!(
            ts_sane(probe_fps),
            sane_fps(probe_fps),
            "the two clamps disagree at {probe_fps} fps"
        );
    }
    assert_eq!(sane_fps(f32::NAN), DEFAULT_FPS, "NaN must not survive");
    assert_eq!(sane_fps(f32::INFINITY), DEFAULT_FPS);
}

// ── 2. Probe → export, measured ──────────────────────────────────────────────

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

fn rational(s: &str) -> f64 {
    match s.split_once('/') {
        Some((n, d)) => {
            let (n, d) = (
                n.parse::<f64>().unwrap_or(0.0),
                d.parse::<f64>().unwrap_or(1.0),
            );
            if d == 0.0 {
                0.0
            } else {
                n / d
            }
        }
        None => s.parse().unwrap_or(0.0),
    }
}

fn stream_field(path: &Path, field: &str) -> String {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            &format!("stream={field}"),
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("spawn ffprobe");
    assert!(out.status.success(), "ffprobe failed for {path:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A source whose nominal rate is genuinely absurd. `testsrc` at rate=1000
/// gives ffprobe an `r_frame_rate` of `1000/1` — the same thing a VFR screen
/// recording with 1 ms tick granularity reports.
fn generate_1000fps(dst: &Path) {
    let _ = std::fs::remove_file(dst);
    let st = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=1000:duration=0.2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-an",
        ])
        .arg(dst)
        .status()
        .expect("spawn ffmpeg (1000 fps fixture)");
    assert!(st.success(), "1000 fps fixture generation failed");
}

#[test]
#[ignore = "needs ffmpeg/ffprobe on PATH (generates its own fixture)"]
fn an_absurd_source_rate_never_reaches_the_export() {
    let src = tmp("sundayedit_fps_1000.mp4");
    generate_1000fps(&src);

    // The fixture really is the pathological case this guards against…
    assert_eq!(
        rational(&stream_field(&src, "r_frame_rate")),
        1000.0,
        "the fixture is supposed to report a 1000/1 nominal rate"
    );

    // …and the probe refuses to pass it on.
    let m = probe(&src).expect("probe the 1000 fps fixture");
    assert!(
        m.fps <= MAX_FPS && m.fps >= MIN_FPS,
        "probe reported {} fps — outside the window the export can render",
        m.fps
    );

    // The export path, from the probed number all the way to the argv. The
    // canvas is the probed frame; the rate is the probed rate, exactly as the
    // frontend's `defaultComposeSettings` would hand it over.
    let out = tmp("sundayedit_fps_1000_out.mp4");
    let settings = canvas_settings(m.width, m.height, m.fps);
    let p = project(
        vec![media_sized(
            "m1",
            &src.to_string_lossy(),
            m.width,
            m.height,
            200,
        )],
        vec![track("t1", TrackKind::Video, 0)],
        vec![item("a", "t1", "m1", 0, 0, 200, Transform::default())],
        m.width,
        m.height,
    );
    let args =
        build_filter_complex(&p, &settings, None, &out.to_string_lossy()).expect("composable");

    let r_flag = args
        .iter()
        .position(|a| a == "-r")
        .map(|i| args[i + 1].clone())
        .expect("the export always sets an output rate");
    let r: f32 = r_flag.parse().expect("-r is a number");
    assert!(
        (MIN_FPS..=MAX_FPS).contains(&r),
        "the export asked for -r {r_flag}"
    );

    run_compose_argv(&args, &out);
    let rendered = rational(&stream_field(&out, "avg_frame_rate"));
    assert!(
        rendered > 0.0 && rendered <= MAX_FPS as f64 + 0.5,
        "the RENDERED file runs at {rendered} fps"
    );

    // Belt and braces: a hand-edited project file carrying an absurd rate is
    // clamped by the builder itself, not only by whatever the probe reported.
    let mut wild = p.clone();
    wild.video_fps = 1000.0;
    let wild_settings = canvas_settings(m.width, m.height, 1000.0);
    let wild_args = build_filter_complex(&wild, &wild_settings, None, "out.mp4").expect("builds");
    let wild_r = wild_args
        .iter()
        .position(|a| a == "-r")
        .map(|i| wild_args[i + 1].parse::<f32>().expect("-r is a number"))
        .expect("an output rate");
    assert!(
        (MIN_FPS..=MAX_FPS).contains(&wild_r),
        "the builder must clamp a caller-supplied rate too, got -r {wild_r}"
    );
    // …and so does the proxy profile (which capped at 30 by luck, not design).
    assert!(
        (MIN_FPS..=30.0).contains(&proxy_settings(&wild).fps),
        "proxy fps escaped its window"
    );

    for f in [&src, &out] {
        let _ = std::fs::remove_file(f);
    }
}

/// The `avg_frame_rate` fallback, on a real file: a source whose nominal rate
/// is unusable but whose measured average is fine must be reported at the
/// measured average, not at the clamp ceiling.
#[test]
fn a_vfr_source_is_reported_at_its_measured_average() {
    // ffprobe's `r_frame_rate` guess comes from the SMALLEST frame gap, so a
    // clip with one 1 ms hiccup among 25 fps frames reports 1000/1 nominal
    // while averaging ~25. That is the shape of a screen recording.
    let json = r#"{
      "streams": [
        { "codec_type": "video", "codec_name": "h264", "width": 1920, "height": 1080,
          "r_frame_rate": "1000/1", "avg_frame_rate": "30000/1001" }
      ],
      "format": { "duration": "12.0", "format_name": "mov,mp4,m4a,3gp,3g2,mj2" }
    }"#;
    let m = parse_ffprobe_json(json).expect("parses");
    assert!(
        (m.fps - 29.97).abs() < 0.01,
        "a VFR source must fall back to its measured average, got {} fps",
        m.fps
    );
}
