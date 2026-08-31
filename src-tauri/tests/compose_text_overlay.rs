//! Text timeline items must actually reach the pixels — MEASURED (R5-C).
//!
//! Before R5-C a `TimelineItemKind::Text` item was REFUSED by the export (R1
//! made the older silent loss loud). It is now rendered, and it is rendered
//! through the layer the codebase already owns: `export::write_ass` emits one
//! `Dialogue:` line per text item and `compose` hangs the same final `ass=`
//! node it has always used for captions. No `drawtext`, no second font or
//! escaping regime.
//!
//! What is measured here, not asserted as a string:
//!
//!   1. INK LANDS WHERE THE TRANSFORM SAYS. `Transform.x/.y` are fractions of
//!      the output frame; the ASS header writes `PlayResX/Y` from the project
//!      dimensions, so `\pos(round(W*x), round(H*y))` with `\an7` (top-left
//!      anchor) is the same arithmetic `overlay=<W*x>:<H*y>` applies to a
//!      picture clip. A probe at that spot must brighten; a probe just LEFT of
//!      it must stay black, so "the text is somewhere on the frame" cannot
//!      pass for "the text is where the user put it".
//!   2. THE SAME PROJECT WITHOUT THE SIDECAR IS BLACK THERE — so the ink is
//!      provably the overlay's and not the source's.
//!   3. BOTH RENDER PATHS AGREE. A baseline import plus a text overlay keeps
//!      the burn-in FAST PATH (that is the R5-C decision: `burnin` applies the
//!      very same sidecar), so the fast path is rendered through
//!      `burnin::build_ffmpeg_args` and probed with the identical assertions.
//!
//! Fixtures are solid black, generated here — white text on black makes "is
//! there ink in this patch?" a number no codec ringing can blur.
//!
//! Run (needs `ffmpeg`/`ffprobe` on PATH; generates its own fixtures):
//! ```sh
//! cargo test --manifest-path src-tauri/Cargo.toml \
//!   --test compose_text_overlay -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

use sundayedit_lib::model::{TextSpec, TimelineItem, TimelineItemKind};
use sundayedit_lib::services::burnin::{build_ffmpeg_args, BurnInOptions, Encoder, VideoCodec};
use sundayedit_lib::services::compose::{build_filter_complex, is_simple_timeline};
use sundayedit_lib::services::export::write_ass;

mod common;
use common::{
    canvas_settings, generate_solid, item, media_sized, project, run_compose_argv, sample_rgb,
    track, TrackKind, Transform,
};

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

// ── the frame we measure in ──────────────────────────────────────────────────
//
// Deliberately NOT square and deliberately not the same fraction on both axes:
// with `W == H`, or with `x == y`, a writer that multiplied the fractions by
// the wrong dimension would put the ink in the same place as a correct one and
// this whole file would prove nothing.
const W: i32 = 800;
const H: i32 = 450;
/// 800 * 0.5 = 400 px from the left, 450 * 0.2 = 90 px from the top.
const FRAC_X: f32 = 0.5;
const FRAC_Y: f32 = 0.2;
const POS_X: i64 = 400;
const POS_Y: i64 = 90;

/// Long enough to be unmissable, short enough to stay inside the frame from
/// `POS_X` at the default style's 42 px.
const OVERLAY_TEXT: &str = "HALLO";

fn text_item(id: &str, track_id: &str, start: i64, dur: i64) -> TimelineItem {
    TimelineItem {
        id: id.into(),
        track_id: track_id.into(),
        kind: TimelineItemKind::Text,
        source_media_id: None,
        in_ms: 0,
        out_ms: dur,
        timeline_start_ms: start,
        speed: 1.0,
        gain_db: 0.0,
        fade_in_ms: 0,
        fade_out_ms: 0,
        transform: Transform {
            x: FRAC_X,
            y: FRAC_Y,
            ..Transform::default()
        },
        effects: vec![],
        transition_in: None,
        text: Some(TextSpec {
            text: OVERLAY_TEXT.into(),
            style_id: None,
        }),
        enabled: true,
        locked: false,
    }
}

/// Mean of the three channels over a patch — "how much ink is here".
fn ink(video: &Path, t: f64, x: i64, y: i64, w: i64, h: i64) -> f64 {
    let (r, g, b) = sample_rgb(video, t, x, y, w, h);
    (r + g + b) / 3.0
}

/// Anything above this is ink; the fixture underneath is solid black, which
/// probes at ~1 after the h264 round trip.
const INK: f64 = 4.0;
const BLACK: f64 = 2.0;

/// The three probes every render in this file goes through.
///
/// `where_the_transform_says` is the patch the glyphs must fill. The other two
/// are the ones that make the first one mean something: a strip immediately to
/// the LEFT of `POS_X` (nothing may spill back over the anchor) and a band far
/// below (the default style's own Alignment is bottom-centre, so a writer that
/// forgot `\an7` — or dropped `\pos` altogether — lands there instead).
fn assert_text_is_at_the_transform(out: &Path, t: f64, ctx: &str) {
    let on = ink(out, t, POS_X + 4, POS_Y + 6, 180, 34);
    assert!(
        on > INK,
        "{ctx}: no ink at the transform's own position ({POS_X},{POS_Y}) — mean {on:.2}"
    );
    let left = ink(out, t, POS_X - 70, POS_Y + 6, 60, 34);
    assert!(
        left < BLACK,
        "{ctx}: ink LEFT of the anchor — the overlay is not top-left anchored at \
         \\pos({POS_X},{POS_Y}); mean {left:.2}"
    );
    let bottom = ink(out, t, 100, (H - 90) as i64, 600, 60);
    assert!(
        bottom < BLACK,
        "{ctx}: ink in the caption band — the overlay fell back to the style's \
         own Alignment instead of \\an7\\pos; mean {bottom:.2}"
    );
}

// ── the tests ────────────────────────────────────────────────────────────────

/// THE FEATURE: a text item renders, at its transform, on the composite path.
#[test]
#[ignore = "needs ffmpeg/ffprobe on PATH (generates its own fixtures)"]
fn a_text_item_renders_ink_where_its_transform_says() {
    let src = tmp("sundayedit_text_src.mp4");
    let out = tmp("sundayedit_text_out.mp4");
    let bare = tmp("sundayedit_text_bare.mp4");
    let ass_path = tmp("sundayedit_text_out.ass");
    generate_solid(&src, "black", W, H, 3.0);

    let mut p = project(
        vec![media_sized("m1", &src.to_string_lossy(), W, H, 3_000)],
        vec![
            track("v1", TrackKind::Video, 0),
            track("o1", TrackKind::Overlay, 1),
        ],
        vec![
            item("a", "v1", "m1", 0, 0, 3_000, Transform::default()),
            text_item("tx1", "o1", 500, 2_000),
        ],
        W,
        H,
    );
    p.video_duration_ms = 3_000;

    // Exactly what `run_compose` does: sidecar from the writer, `ass=` node.
    std::fs::write(&ass_path, write_ass(&p)).unwrap();
    let settings = canvas_settings(W, H, 25.0);
    let args = build_filter_complex(
        &p,
        &settings,
        Some(&ass_path.to_string_lossy()),
        &out.to_string_lossy(),
    )
    .expect("a text overlay must compose");
    run_compose_argv(&args, &out);

    // 1.5 s is inside the overlay's 0.5–2.5 s span.
    assert_text_is_at_the_transform(&out, 1.5, "composite path");

    // …and OUTSIDE its span the frame is clean again: the Dialogue's Start/End
    // are the item's timeline bounds, not "the whole render".
    let before = ink(&out, 0.2, POS_X + 4, POS_Y + 6, 180, 34);
    assert!(
        before < BLACK,
        "overlay ink before its timeline_start_ms; mean {before:.2}"
    );
    let after = ink(&out, 2.8, POS_X + 4, POS_Y + 6, 180, 34);
    assert!(
        after < BLACK,
        "overlay ink after its timeline_end_ms; mean {after:.2}"
    );

    // 2. THE REFERENCE: the same project, same graph, NO sidecar. Whatever the
    //    probe found above has to be the overlay — this frame is black there.
    let bare_args =
        build_filter_complex(&p, &settings, None, &bare.to_string_lossy()).expect("composable");
    run_compose_argv(&bare_args, &bare);
    let reference = ink(&bare, 1.5, POS_X + 4, POS_Y + 6, 180, 34);
    assert!(
        reference < BLACK,
        "the text-free render must be black where the overlay draws; mean {reference:.2}"
    );

    for f in [&src, &out, &bare, &ass_path] {
        let _ = std::fs::remove_file(f);
    }
}

/// THE FAST-PATH DECISION, in pixels.
///
/// A baseline import plus a text overlay stays "simple", because
/// `burnin::build_ffmpeg_args` applies the SAME `ass=` sidecar. If that
/// reasoning were wrong the shortcut would render the video without the text —
/// the silent loss R1 made loud — so it is pinned by rendering through the
/// burn-in argv itself and running the identical probes.
#[test]
#[ignore = "needs ffmpeg/ffprobe on PATH (generates its own fixtures)"]
fn the_burn_in_fast_path_renders_the_same_overlay() {
    let src = tmp("sundayedit_text_fast_src.mp4");
    let out = tmp("sundayedit_text_fast_out.mp4");
    let ass_path = tmp("sundayedit_text_fast.ass");
    generate_solid(&src, "black", W, H, 3.0);

    let src_str = src.to_string_lossy().into_owned();
    let mut p = project(
        vec![media_sized("m1", &src_str, W, H, 3_000)],
        vec![
            track("v1", TrackKind::Video, 0),
            track("o1", TrackKind::Overlay, 1),
        ],
        vec![
            // The pristine baseline clip `backfill_default_timeline` builds.
            item("a", "v1", "m1", 0, 0, 3_000, Transform::default()),
            text_item("tx1", "o1", 500, 2_000),
        ],
        W,
        H,
    );
    p.video_path = src_str.clone();
    p.video_content_hash = "h-m1".into();
    p.video_duration_ms = 3_000;

    assert!(
        is_simple_timeline(&p),
        "R5-C decision: a text overlay inside the video's length keeps the \
         burn-in fast path"
    );

    std::fs::write(&ass_path, write_ass(&p)).unwrap();
    let args = build_ffmpeg_args(
        &src_str,
        &ass_path.to_string_lossy(),
        &out.to_string_lossy(),
        &BurnInOptions {
            codec: VideoCodec::H264,
            encoder: Encoder::Cpu,
            out_width: Some(W),
            out_height: Some(H),
            bitrate_kbps: None,
            clip_start_ms: None,
            clip_end_ms: None,
        },
    );
    run_compose_argv(&args, &out);

    assert_text_is_at_the_transform(&out, 1.5, "burn-in fast path");

    for f in [&src, &out, &ass_path] {
        let _ = std::fs::remove_file(f);
    }
}

/// A `Graphic` overlay is a genuinely different job — an extra `-i` input and
/// its own `overlay` node, neither of which this graph builds — so it keeps
/// the loud refusal that `Text` just left. Pinned here beside the feature so
/// nobody widens the predicate by reflex.
#[test]
fn a_graphic_item_is_still_refused() {
    let mut p = project(
        vec![media_sized("m1", "/a.mp4", W, H, 3_000)],
        vec![
            track("v1", TrackKind::Video, 0),
            track("o1", TrackKind::Overlay, 1),
        ],
        vec![item("a", "v1", "m1", 0, 0, 3_000, Transform::default())],
        W,
        H,
    );
    let mut g = text_item("gx1", "o1", 0, 1_000);
    g.kind = TimelineItemKind::Graphic;
    g.text = None;
    p.timeline_items.push(g);

    let err = build_filter_complex(&p, &canvas_settings(W, H, 25.0), None, "out.mp4")
        .expect_err("a graphic overlay must not compose silently");
    let msg = err.to_string();
    assert!(msg.contains("Graphic"), "must name the kind: {msg}");
    assert!(msg.contains("gx1"), "must name the item: {msg}");
    assert!(
        !is_simple_timeline(&p),
        "…and it must not be swallowed by the burn-in shortcut either"
    );
}
