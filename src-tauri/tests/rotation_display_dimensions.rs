//! Rotated sources: probe the DISPLAY frame, and let ffmpeg do the rotating.
//!
//! The defect: `parse_ffprobe_json` read `width`/`height` and nothing else. A
//! portrait phone clip is stored LANDSCAPE (1920x1080) with a display matrix
//! asking for a quarter turn, so we probed it as landscape, built a landscape
//! project canvas from it — and then ffmpeg auto-rotated the decoded frame to
//! 1080x1920 at export. Preview and canvas said one thing, the render did
//! another.
//!
//! ── What was MEASURED here, not assumed ─────────────────────────────────────
//! Both halves of the fix rest on one empirical fact, and this file re-proves
//! it on every run rather than trusting a changelog:
//!
//!   1. `probe()` must report the DISPLAY size (w/h swapped for 90/270),
//!      because that is the size everything downstream sees.
//!   2. The filtergraph must NOT rotate again. ffmpeg auto-rotates on decode,
//!      and — this is the part worth measuring — it does so INSIDE
//!      `-filter_complex` too: a stream ffprobe reports as `320x120` arrives
//!      at `[0:v]` already transposed to `120x320`. A `transpose=` in
//!      `transform_filters` would therefore turn the picture twice.
//!
//! The test renders through the REAL `build_filter_complex` and reads the
//! pixels back: the fixture is a two-colour split whose orientation is
//! unambiguous (red/green side by side before rotation, one above the other
//! after it), so a missing rotation, a double rotation, or a rotation in the
//! wrong direction each land a different colour under the probe.
//!
//! Run (needs `ffmpeg`/`ffprobe` on PATH; generates its own fixtures):
//! ```sh
//! cargo test --manifest-path src-tauri/Cargo.toml \
//!   --test rotation_display_dimensions -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use sundayedit_lib::services::video::{parse_ffprobe_json, probe};

mod common;
use common::{
    canvas_settings, item, media_sized, project, sample_rgb, track, TrackKind, Transform,
};

// ── fixtures ─────────────────────────────────────────────────────────────────

const SRC_W: i64 = 320;
const SRC_H: i64 = 120;

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

/// A LANDSCAPE 320x120 clip: red on the left half, green on the right.
/// After a quarter turn the two colours stack instead — which is exactly the
/// signal the pixel probes below read.
fn generate_split(dst: &Path) {
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
            &format!("color=c=red:s={}x{SRC_H}:r=25:d=2", SRC_W / 2),
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=green:s={}x{SRC_H}:r=25:d=2", SRC_W / 2),
            "-filter_complex",
            "[0:v][1:v]hstack=inputs=2,format=yuv420p[v]",
            "-map",
            "[v]",
            "-c:v",
            "libx264",
            "-an",
        ])
        .arg(dst)
        .status()
        .expect("spawn ffmpeg (split fixture)");
    assert!(st.success(), "split fixture generation failed");
}

/// Stamp a rotation onto `src` without re-encoding: write the mov/mp4 display
/// matrix, the same thing an iPhone writes when it records with the phone held
/// upright.
///
/// `-display_rotation` (an INPUT option) rather than `-metadata:s:v rotate=N`.
/// The metadata spelling was deprecated in ffmpeg 7 and is silently DROPPED by
/// ffmpeg 8 — the fixture came out unrotated and the test measured nothing.
/// `-display_rotation` produces a byte-identical matrix on 6.0 and 8.1.
fn stamp_rotation(src: &Path, dst: &Path, degrees: i32) {
    let _ = std::fs::remove_file(dst);
    let st = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-display_rotation",
            &degrees.to_string(),
            "-i",
        ])
        .arg(src)
        .args(["-c", "copy"])
        .arg(dst)
        .status()
        .expect("spawn ffmpeg (rotation stamp)");
    assert!(st.success(), "could not stamp a {degrees}° display matrix");
    // A fixture that quietly lost its rotation would make every assertion
    // below pass for the wrong reason. Prove the matrix is there.
    let raw = Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_streams"])
        .arg(dst)
        .output()
        .expect("spawn ffprobe");
    let json = String::from_utf8_lossy(&raw.stdout);
    assert!(
        json.contains("\"rotation\""),
        "the {degrees}° fixture carries no display matrix — the recipe that \
         writes one has changed under this ffmpeg build:\n{json}"
    );
}

// ── the tests ────────────────────────────────────────────────────────────────

/// A quarter turn swaps the reported frame. A half turn does not.
#[test]
#[ignore = "needs ffmpeg/ffprobe on PATH (generates its own fixtures)"]
fn probe_reports_the_display_frame_not_the_coded_one() {
    let land = tmp("sundayedit_rot_land.mp4");
    generate_split(&land);

    let flat = probe(&land).expect("probe the unrotated fixture");
    assert_eq!(
        (flat.width as i64, flat.height as i64),
        (SRC_W, SRC_H),
        "an unrotated clip must be reported exactly as stored"
    );

    for (deg, want) in [
        (90, (SRC_H, SRC_W)),
        (270, (SRC_H, SRC_W)),
        (180, (SRC_W, SRC_H)),
    ] {
        let rotated = tmp(&format!("sundayedit_rot_{deg}.mp4"));
        stamp_rotation(&land, &rotated, deg);

        // The container really does still store the landscape frame — if this
        // ever stops being true the test below is measuring nothing.
        let raw = Command::new("ffprobe")
            .args(["-v", "error", "-print_format", "json", "-show_streams"])
            .arg(&rotated)
            .output()
            .expect("spawn ffprobe");
        let json = String::from_utf8_lossy(&raw.stdout);
        assert!(
            json.contains(&format!("\"width\": {SRC_W}")),
            "rotate={deg}: the coded frame should still be {SRC_W} wide"
        );

        let m = probe(&rotated).expect("probe the rotated fixture");
        assert_eq!(
            (m.width as i64, m.height as i64),
            want,
            "rotate={deg}: probe must report the DISPLAY frame"
        );

        let _ = std::fs::remove_file(&rotated);
    }
    let _ = std::fs::remove_file(&land);
}

/// The compose graph must not rotate a rotated source a SECOND time.
///
/// A 320x120 red|green split, stamped `rotate=90`, composed onto a canvas of
/// the probed (display) size. If ffmpeg's decode-side auto-rotation reaches
/// the filtergraph — the thing this asserts — the frame arrives 120x320 with
/// the colours stacked, fits the canvas edge-to-edge, and the probes below
/// find one colour on top of the other. If the graph rotated as well, the
/// picture would be back to side-by-side (or upside down) and the probes swap.
#[test]
#[ignore = "needs ffmpeg/ffprobe on PATH (generates its own fixtures)"]
fn a_rotated_source_composes_upright_without_the_graph_rotating_it() {
    let land = tmp("sundayedit_rot2_land.mp4");
    let rotated = tmp("sundayedit_rot2_90.mp4");
    let out = tmp("sundayedit_rot2_out.mp4");
    generate_split(&land);
    stamp_rotation(&land, &rotated, 90);

    let m = probe(&rotated).expect("probe");
    let (w, h) = (m.width, m.height);
    assert_eq!((w as i64, h as i64), (SRC_H, SRC_W), "portrait after probe");

    // Canvas = the probed display frame, which is what the frontend derives
    // its project geometry from.
    let settings = canvas_settings(w, h, 25.0);
    let p = project(
        vec![media_sized("m1", &rotated.to_string_lossy(), w, h, 2_000)],
        vec![track("t1", TrackKind::Video, 0)],
        vec![item("a", "t1", "m1", 0, 0, 2_000, Transform::default())],
        w,
        h,
    );
    let _ = std::fs::remove_file(&out);
    let args = sundayedit_lib::services::compose::build_filter_complex(
        &p,
        &settings,
        None,
        &out.to_string_lossy(),
    )
    .expect("composable");
    let res = Command::new("ffmpeg")
        .args(&args)
        .output()
        .expect("spawn ffmpeg (compose)");
    assert!(
        res.status.success(),
        "compose failed; argv {args:?}\n{}",
        String::from_utf8_lossy(&res.stderr)
    );

    // The rendered frame is the display frame, edge to edge.
    let probe_out = Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_streams"])
        .arg(&out)
        .output()
        .expect("spawn ffprobe");
    let om = parse_ffprobe_json(&String::from_utf8_lossy(&probe_out.stdout)).expect("probe output");
    assert_eq!(
        (om.width, om.height),
        (w, h),
        "rendered at the display frame"
    );

    // The REFERENCE orientation: a plain transcode, which is ffmpeg's own
    // (and any player's) idea of which way up this clip goes. Comparing
    // against it rather than against a hand-written "red ends up on top"
    // keeps the test honest about the one thing it is really asserting —
    // the compose graph rotates the picture exactly as many times as a plain
    // decode does, i.e. once, by ffmpeg, and not again by us.
    let reference = tmp("sundayedit_rot2_ref.mp4");
    let _ = std::fs::remove_file(&reference);
    let st = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&rotated)
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-an"])
        .arg(&reference)
        .status()
        .expect("spawn ffmpeg (reference transcode)");
    assert!(st.success(), "reference transcode failed");

    let s = 24i64;
    let cx = w as i64 / 2 - s / 2;
    let ref_top = sample_rgb(&reference, 1.0, cx, h as i64 / 4, s, s);
    let ref_bottom = sample_rgb(&reference, 1.0, cx, h as i64 * 3 / 4, s, s);
    // Guard against a vacuous comparison: the two halves must really differ,
    // and they must be STACKED (a source that never got rotated would still
    // have red beside green, and both probes would read the same).
    assert!(
        (ref_top.0 - ref_bottom.0).abs() > 60.0 || (ref_top.1 - ref_bottom.1).abs() > 60.0,
        "the reference must show two distinct halves stacked vertically, \
         got top {ref_top:?} bottom {ref_bottom:?}"
    );

    let top = sample_rgb(&out, 1.0, cx, h as i64 / 4, s, s);
    let bottom = sample_rgb(&out, 1.0, cx, h as i64 * 3 / 4, s, s);
    let close = |a: (f64, f64, f64), b: (f64, f64, f64)| {
        (a.0 - b.0).abs() < 30.0 && (a.1 - b.1).abs() < 30.0 && (a.2 - b.2).abs() < 30.0
    };
    assert!(
        close(top, ref_top),
        "the composed frame's TOP must match a plain decode's top — a second \
         rotation in the filtergraph would swap it. compose {top:?} vs reference {ref_top:?}"
    );
    assert!(
        close(bottom, ref_bottom),
        "the composed frame's BOTTOM must match a plain decode's bottom. \
         compose {bottom:?} vs reference {ref_bottom:?}"
    );

    // …and the halves are stacked, not side by side: left and right of the
    // same row read the same colour.
    let row = h as i64 / 4;
    let left = sample_rgb(&out, 1.0, 4, row, s, s);
    let right = sample_rgb(&out, 1.0, w as i64 - s - 4, row, s, s);
    assert!(
        close(left, right),
        "a row of the composed frame must be one colour (the picture is \
         rotated, not letterboxed side by side): left {left:?} right {right:?}"
    );

    for f in [&land, &rotated, &out, &reference] {
        let _ = std::fs::remove_file(f);
    }
}
