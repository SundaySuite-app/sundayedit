//! Fit-to-canvas: the export must SHOW the whole clip, the way the preview
//! does — MEASURED in the rendered pixels.
//!
//! The defect: `transform_filters` scaled only when the user had moved the
//! scale slider, and nothing else resized anything, so every clip composited
//! at NATIVE source size at its transform offset. A 4K clip in a 1080p project
//! showed its top-left quadrant and nothing else. A 1080x1920 phone clip in a
//! 1920x1080 project sat as a strip against the left edge. Meanwhile the
//! DEFAULT preview is `<video class="max-h-full max-w-full">`, which fits — so
//! the preview fit and the export cropped, the exact divergence this codebase
//! forbids.
//!
//! The fix fits CONTAIN (never crop, never stretch, centred) to the canvas
//! BEFORE the transform, so a user scale of 0.4 still means "40 % of the
//! frame". Bars are transparent when the aspect ratios differ, so a layer's
//! letterboxing cannot paint over the layer beneath it.
//!
//! Fixtures are solid-colour quadrants: "which part of the source is this
//! pixel?" then has a numeric answer no codec ringing can blur.
//!
//! Run (needs `ffmpeg`/`ffprobe` on PATH; generates its own fixtures):
//! ```sh
//! cargo test --manifest-path src-tauri/Cargo.toml \
//!   --test compose_fit_to_canvas -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use sundayedit_lib::services::compose::build_filter_complex;

mod common;
use common::{
    canvas_settings, item, media_sized, project, run_compose_argv, sample_rgb, track, TrackKind,
    Transform,
};

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

/// A `w`x`h` clip cut into four solid quadrants: red TL, green TR, blue BL,
/// white BR. Every corner of the picture is therefore identifiable on its own,
/// which is what makes "is the WHOLE frame here?" a measurable question.
///
/// Spelled as hex, not as ffmpeg's colour NAMES: `color=c=green` is #008000,
/// half-lit, and a classifier tuned for it would also accept a dimmed or
/// blended patch. Full-scale primaries keep every channel unambiguously at one
/// end or the other.
fn generate_quadrants(dst: &Path, w: i32, h: i32, seconds: f64) {
    let _ = std::fs::remove_file(dst);
    let (hw, hh) = (w / 2, h / 2);
    let src = |c: &str| format!("color=c={c}:s={hw}x{hh}:r=25:d={seconds}");
    // #FF0000 / #00FF00 / #0000FF / #FFFFFF
    let st = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &src("0xFF0000"),
            "-f",
            "lavfi",
            "-i",
            &src("0x00FF00"),
            "-f",
            "lavfi",
            "-i",
            &src("0x0000FF"),
            "-f",
            "lavfi",
            "-i",
            &src("0xFFFFFF"),
            "-filter_complex",
            "[0:v][1:v]hstack[t];[2:v][3:v]hstack[b];[t][b]vstack,format=yuv420p[v]",
            "-map",
            "[v]",
            "-c:v",
            "libx264",
            "-an",
        ])
        .arg(dst)
        .status()
        .expect("spawn ffmpeg (quadrant fixture)");
    assert!(st.success(), "quadrant fixture generation failed");
}

fn generate_solid(dst: &Path, colour: &str, w: i32, h: i32, seconds: f64) {
    common::generate_solid(dst, colour, w, h, seconds);
}

/// Which of the fixture's four colours (or the empty canvas) a patch shows.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Patch {
    Red,
    Green,
    Blue,
    White,
    Black,
    Ambiguous,
}

fn classify(px: (f64, f64, f64)) -> Patch {
    let (r, g, b) = px;
    let hi = |v: f64| v > 150.0;
    let lo = |v: f64| v < 70.0;
    match (hi(r), hi(g), hi(b)) {
        (true, true, true) => Patch::White,
        (true, false, false) if lo(g) && lo(b) => Patch::Red,
        (false, true, false) if lo(r) && lo(b) => Patch::Green,
        (false, false, true) if lo(r) && lo(g) => Patch::Blue,
        _ if lo(r) && lo(g) && lo(b) => Patch::Black,
        _ => Patch::Ambiguous,
    }
}

fn at(out: &Path, t: f64, x: i64, y: i64) -> Patch {
    classify(sample_rgb(out, t, x - 6, y - 6, 12, 12))
}

// ── the tests ────────────────────────────────────────────────────────────────

/// A source BIGGER than the canvas must arrive whole. Before the fix the
/// overlay pasted 640x480 native pixels at 0:0 onto a 320x240 canvas and the
/// output was the red quadrant, edge to edge — three quarters of the picture
/// simply gone.
#[test]
#[ignore = "needs ffmpeg/ffprobe on PATH (generates its own fixtures)"]
fn an_oversized_clip_is_fitted_not_cropped_to_its_top_left_quadrant() {
    let src = tmp("sundayedit_fit_big.mp4");
    let out = tmp("sundayedit_fit_big_out.mp4");
    generate_quadrants(&src, 640, 480, 2.0);

    // Canvas at HALF the source's size — same 4:3 aspect, so a correct fit is
    // edge to edge with no bars at all.
    let (cw, ch) = (320, 240);
    let p = project(
        vec![media_sized("m1", &src.to_string_lossy(), 640, 480, 2_000)],
        vec![track("t1", TrackKind::Video, 0)],
        vec![item("a", "t1", "m1", 0, 0, 2_000, Transform::default())],
        cw,
        ch,
    );
    let args = build_filter_complex(
        &p,
        &canvas_settings(cw, ch, 25.0),
        None,
        &out.to_string_lossy(),
    )
    .expect("composable");
    run_compose_argv(&args, &out);

    // All four quadrants present, in the right corners.
    assert_eq!(at(&out, 1.0, 80, 60), Patch::Red, "top-left quadrant");
    assert_eq!(at(&out, 1.0, 240, 60), Patch::Green, "top-right quadrant");
    assert_eq!(at(&out, 1.0, 80, 180), Patch::Blue, "bottom-left quadrant");
    assert_eq!(
        at(&out, 1.0, 240, 180),
        Patch::White,
        "bottom-right quadrant"
    );

    for f in [&src, &out] {
        let _ = std::fs::remove_file(f);
    }
}

/// A PORTRAIT clip in a landscape project is centred and letterboxed, not
/// parked against the left edge at native size.
#[test]
#[ignore = "needs ffmpeg/ffprobe on PATH (generates its own fixtures)"]
fn a_portrait_clip_in_a_landscape_project_is_centred_and_letterboxed() {
    let src = tmp("sundayedit_fit_port.mp4");
    let out = tmp("sundayedit_fit_port_out.mp4");
    generate_quadrants(&src, 240, 480, 2.0);

    let (cw, ch) = (640, 480);
    let p = project(
        vec![media_sized("m1", &src.to_string_lossy(), 240, 480, 2_000)],
        vec![track("t1", TrackKind::Video, 0)],
        vec![item("a", "t1", "m1", 0, 0, 2_000, Transform::default())],
        cw,
        ch,
    );
    let args = build_filter_complex(
        &p,
        &canvas_settings(cw, ch, 25.0),
        None,
        &out.to_string_lossy(),
    )
    .expect("composable");
    run_compose_argv(&args, &out);

    // Fitted by height (480 → 480), so the picture is 240 wide, centred:
    // columns 200..440. Bars either side.
    assert_eq!(at(&out, 1.0, 40, 240), Patch::Black, "left letterbox bar");
    assert_eq!(at(&out, 1.0, 600, 240), Patch::Black, "right letterbox bar");
    // The picture itself, quadrant by quadrant. Before the fix the clip sat at
    // columns 0..240 and every one of these probes read the wrong thing.
    assert_eq!(at(&out, 1.0, 260, 120), Patch::Red, "picture: top-left");
    assert_eq!(at(&out, 1.0, 380, 120), Patch::Green, "picture: top-right");
    assert_eq!(at(&out, 1.0, 260, 360), Patch::Blue, "picture: bottom-left");
    assert_eq!(
        at(&out, 1.0, 380, 360),
        Patch::White,
        "picture: bottom-right"
    );

    for f in [&src, &out] {
        let _ = std::fs::remove_file(f);
    }
}

/// The letterbox bars of an UPPER layer must be see-through. A picture-in-
/// picture with a different aspect ratio must not arrive as a black box with a
/// picture in it, blotting out the clip underneath.
#[test]
#[ignore = "needs ffmpeg/ffprobe on PATH (generates its own fixtures)"]
fn a_stacked_clips_letterbox_bars_do_not_paint_over_the_layer_below() {
    let bg = tmp("sundayedit_fit_bg.mp4");
    let fg = tmp("sundayedit_fit_fg.mp4");
    let out = tmp("sundayedit_fit_stack_out.mp4");
    let (cw, ch) = (640, 480);
    // Background fills the canvas exactly (4:3), foreground is portrait.
    generate_solid(&bg, "0xFF0000", 640, 480, 2.0);
    generate_solid(&fg, "0x0000FF", 240, 480, 2.0);

    let p = project(
        vec![
            media_sized("m1", &bg.to_string_lossy(), 640, 480, 2_000),
            media_sized("m2", &fg.to_string_lossy(), 240, 480, 2_000),
        ],
        vec![
            track("t1", TrackKind::Video, 0),
            track("t2", TrackKind::Overlay, 1),
        ],
        vec![
            item("a", "t1", "m1", 0, 0, 2_000, Transform::default()),
            item("b", "t2", "m2", 0, 0, 2_000, Transform::default()),
        ],
        cw,
        ch,
    );
    let args = build_filter_complex(
        &p,
        &canvas_settings(cw, ch, 25.0),
        None,
        &out.to_string_lossy(),
    )
    .expect("composable");
    run_compose_argv(&args, &out);

    assert_eq!(
        at(&out, 1.0, 320, 240),
        Patch::Blue,
        "the overlay's picture is on top"
    );
    assert_eq!(
        at(&out, 1.0, 40, 240),
        Patch::Red,
        "the overlay's letterbox bar must be transparent — the background \
         layer shows through it, it is not a black box"
    );
    assert_eq!(
        at(&out, 1.0, 600, 240),
        Patch::Red,
        "…and on the other side"
    );

    for f in [&bg, &fg, &out] {
        let _ = std::fs::remove_file(f);
    }
}

/// `Transform.scale` is a fraction of the OUTPUT FRAME, not of whatever the
/// camera happened to shoot. This is the number the preview compositor
/// mirrors, so it is the number the export must mean.
///
/// A 640x480 source at scale 0.4 in a 320x240 project draws a 128x96 box —
/// 40 % of the canvas. Before the fit it drew 0.4 x 640 = 256 px wide, twice
/// as big, and ran off the right-hand edge.
#[test]
#[ignore = "needs ffmpeg/ffprobe on PATH (generates its own fixtures)"]
fn a_user_scale_is_a_fraction_of_the_frame_not_of_the_source() {
    let src = tmp("sundayedit_fit_scale.mp4");
    let out = tmp("sundayedit_fit_scale_out.mp4");
    generate_solid(&src, "0x00FF00", 640, 480, 2.0);

    let (cw, ch) = (320, 240);
    let (tx, ty, scale) = (0.15f32, 0.20f32, 0.4f32);
    let transform = Transform {
        x: tx,
        y: ty,
        scale,
        ..Transform::default()
    };
    let p = project(
        vec![media_sized("m1", &src.to_string_lossy(), 640, 480, 2_000)],
        vec![track("t1", TrackKind::Video, 0)],
        vec![item("a", "t1", "m1", 0, 0, 2_000, transform)],
        cw,
        ch,
    );
    let args = build_filter_complex(
        &p,
        &canvas_settings(cw, ch, 25.0),
        None,
        &out.to_string_lossy(),
    )
    .expect("composable");
    run_compose_argv(&args, &out);

    // Where the inset MUST land — the same arithmetic the preview uses.
    let x = (cw as f32 * tx).round() as i64;
    let y = (ch as f32 * ty).round() as i64;
    let w = (cw as f32 * scale).round() as i64;
    let h = (ch as f32 * scale).round() as i64;

    // Inside both far corners of the inset…
    assert_eq!(
        at(&out, 1.0, x + 10, y + 10),
        Patch::Green,
        "inset top-left"
    );
    assert_eq!(
        at(&out, 1.0, x + w - 10, y + h - 10),
        Patch::Green,
        "inset bottom-right"
    );
    // …and empty canvas just outside every edge. A source-relative scale would
    // have made the box 2.5× too wide and these would all read green.
    assert_eq!(at(&out, 1.0, x - 10, y + h / 2), Patch::Black, "left of it");
    assert_eq!(
        at(&out, 1.0, x + w + 10, y + h / 2),
        Patch::Black,
        "right of it"
    );
    assert_eq!(at(&out, 1.0, x + w / 2, y - 10), Patch::Black, "above it");
    assert_eq!(
        at(&out, 1.0, x + w / 2, y + h + 10),
        Patch::Black,
        "below it"
    );

    for f in [&src, &out] {
        let _ = std::fs::remove_file(f);
    }
}
