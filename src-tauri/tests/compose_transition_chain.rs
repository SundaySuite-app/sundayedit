//! Chained transitions must not eat the tail — MEASURED as the duration of
//! the rendered file, and as the picture that is still there at the end.
//!
//! The defect (found while fixing `seam-xfade-drops-transform`, documented in
//! `compose_transition_transform.rs` and left there as out of scope): the
//! xfade `offset` was computed in TIMELINE coordinates
//! (`prev.timeline_end_ms - transition.duration_ms`) but applied to the
//! PREVIOUS xfade's output — a stream already shorter by the earlier
//! transition. With equal durations the second offset landed exactly on its
//! input's last frame, xfade had nothing left to blend, and a 6 s timeline
//! rendered as ~3.43 s. Everything after the second cut was simply gone.
//!
//! ── The arithmetic this pins ────────────────────────────────────────────────
//! `xfade` CONCATENATES: `offset` seconds of input 1, then `duration` seconds
//! of blend, then the REST of input 2 — `offset + len2` in total. The blend is
//! played once where the butt-joined timeline holds two clips, so the
//! composite is exactly `duration` SHORTER than the timeline, per transition,
//! and everything after the boundary runs that much early. `compose.rs` calls
//! that the composite clock (`composite_shift_at`) and every downstream time —
//! the next offset, the overlay `enable=` windows, the audio `adelay`, the
//! base canvas, the progress total — is expressed on it.
//!
//! So a 3-clip / 2-transition project of 2 s clips and 0.5 s fades renders
//! 6.0 - 0.5 - 0.5 = 5.0 s. Not 6.0 (nothing can make xfade play the blend
//! twice) and emphatically not 3.43.
//!
//! Run (needs `ffmpeg`/`ffprobe` on PATH; generates its own fixtures):
//! ```sh
//! cargo test --manifest-path src-tauri/Cargo.toml \
//!   --test compose_transition_chain -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use sundayedit_lib::model::{TimelineItem, Transition};
use sundayedit_lib::services::compose::{build_filter_complex, rendered_duration_ms};

mod common;
use common::{
    canvas_settings, container_duration_secs, generate_solid, generate_solid_with_tone, item,
    media_sized, media_sized_with_audio, project, run_compose_argv, sample_rgb,
    stream_duration_secs, track, TrackKind, Transform,
};

const W: i32 = 320;
const H: i32 = 240;
const CLIP_MS: i64 = 2_000;
const FADE_MS: i64 = 500;

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

fn fade(ms: i64) -> Option<Transition> {
    Some(Transition {
        kind: "fade".into(),
        duration_ms: ms,
    })
}

fn with_transition(mut it: TimelineItem, ms: i64) -> TimelineItem {
    it.transition_in = fade(ms);
    it
}

/// Three butt-joined clips on one track; clips 2 and 3 enter through a fade.
fn three_clip_project(paths: [&str; 3], with_audio: bool) -> sundayedit_lib::model::Project {
    let mk = |id: &str, path: &str| {
        if with_audio {
            media_sized_with_audio(id, path, W, H, CLIP_MS)
        } else {
            media_sized(id, path, W, H, CLIP_MS)
        }
    };
    project(
        vec![mk("m1", paths[0]), mk("m2", paths[1]), mk("m3", paths[2])],
        vec![track("t1", TrackKind::Video, 0)],
        vec![
            item("a", "t1", "m1", 0, 0, CLIP_MS, Transform::default()),
            with_transition(
                item("b", "t1", "m2", CLIP_MS, 0, CLIP_MS, Transform::default()),
                FADE_MS,
            ),
            with_transition(
                item(
                    "c",
                    "t1",
                    "m3",
                    2 * CLIP_MS,
                    0,
                    CLIP_MS,
                    Transform::default(),
                ),
                FADE_MS,
            ),
        ],
        W,
        H,
    )
}

/// The whole timeline survives two chained transitions.
#[test]
#[ignore = "needs ffmpeg/ffprobe on PATH (generates its own fixtures)"]
fn a_three_clip_two_transition_render_keeps_its_tail() {
    let red = tmp("sundayedit_chain_red.mp4");
    let green = tmp("sundayedit_chain_green.mp4");
    let blue = tmp("sundayedit_chain_blue.mp4");
    let out = tmp("sundayedit_chain_out.mp4");
    generate_solid(&red, "0xFF0000", W, H, 3.0);
    generate_solid(&green, "0x00FF00", W, H, 3.0);
    generate_solid(&blue, "0x0000FF", W, H, 3.0);

    let p = three_clip_project(
        [
            &red.to_string_lossy(),
            &green.to_string_lossy(),
            &blue.to_string_lossy(),
        ],
        false,
    );

    // The arithmetic, stated where a reader can check it: 6.0 s of timeline,
    // two 0.5 s fades, 5.0 s of render.
    let expected_ms = rendered_duration_ms(&p);
    assert_eq!(
        expected_ms,
        3 * CLIP_MS - 2 * FADE_MS,
        "rendered_duration_ms must account for BOTH transitions"
    );

    let args = build_filter_complex(
        &p,
        &canvas_settings(W, H, 25.0),
        None,
        &out.to_string_lossy(),
    )
    .expect("composable");
    run_compose_argv(&args, &out);

    let secs = container_duration_secs(&out);
    let want = expected_ms as f64 / 1000.0;
    assert!(
        (secs - want).abs() < 0.15,
        "the render is {secs:.3} s; the timeline less its two fades is \
         {want:.3} s. (Before the offset fix this file was ~3.43 s — the whole \
         third clip and half the second were missing.)"
    );

    // …and the tail is really the THIRD clip, not a frozen frame of the second.
    let near_end = want - 0.25;
    let px = sample_rgb(&out, near_end, W as i64 / 2 - 8, H as i64 / 2 - 8, 16, 16);
    assert!(
        px.2 > 90.0 && px.0 < 90.0 && px.1 < 90.0,
        "at t={near_end:.2} the THIRD clip (blue) must own the frame, got rgb {px:?}"
    );

    // Each clip still gets its own solo stretch, in order.
    let first = sample_rgb(&out, 0.5, W as i64 / 2 - 8, H as i64 / 2 - 8, 16, 16);
    assert!(
        first.0 > 90.0 && first.2 < 90.0,
        "first clip red, got {first:?}"
    );
    let second = sample_rgb(&out, 2.5, W as i64 / 2 - 8, H as i64 / 2 - 8, 16, 16);
    assert!(
        second.1 > 90.0 && second.2 < 90.0,
        "second clip green in the middle, got {second:?}"
    );

    for f in [&red, &green, &blue, &out] {
        let _ = std::fs::remove_file(f);
    }
}

/// The audio bus rides the SAME clock as the picture.
///
/// `adelay` used to be handed raw timeline positions, so after a transition
/// pulled the video forward the sound stayed where the lane said it was: the
/// audio stream ran to the full 6 s while the video ended at 5 s, and every
/// clip after the first cut played half a second late against its own picture.
/// Measured as the two streams' lengths, which is where that divergence shows.
#[test]
#[ignore = "needs ffmpeg/ffprobe on PATH (generates its own fixtures)"]
fn the_audio_bus_ends_with_the_picture_not_with_the_timeline() {
    let a = tmp("sundayedit_chain_a_tone.mp4");
    let b = tmp("sundayedit_chain_b_tone.mp4");
    let c = tmp("sundayedit_chain_c_tone.mp4");
    let out = tmp("sundayedit_chain_tone_out.mp4");
    generate_solid_with_tone(&a, "0xFF0000", W, H, 3.0, 220);
    generate_solid_with_tone(&b, "0x00FF00", W, H, 3.0, 440);
    generate_solid_with_tone(&c, "0x0000FF", W, H, 3.0, 880);

    let p = three_clip_project(
        [
            &a.to_string_lossy(),
            &b.to_string_lossy(),
            &c.to_string_lossy(),
        ],
        true,
    );
    let args = build_filter_complex(
        &p,
        &canvas_settings(W, H, 25.0),
        None,
        &out.to_string_lossy(),
    )
    .expect("composable");
    run_compose_argv(&args, &out);

    let want = rendered_duration_ms(&p) as f64 / 1000.0;
    let v = stream_duration_secs(&out, "v:0");
    let audio = stream_duration_secs(&out, "a:0");
    assert!(
        (v - want).abs() < 0.15,
        "video stream is {v:.3} s, expected {want:.3} s"
    );
    assert!(
        (audio - v).abs() < 0.2,
        "the audio stream ({audio:.3} s) must end with the picture ({v:.3} s). \
         A whole-timeline-length audio bus means every clip after the first \
         transition is playing late against its own frames."
    );

    for f in [&a, &b, &c, &out] {
        let _ = std::fs::remove_file(f);
    }
}

/// UNEQUAL fade lengths, because equal ones can hide an accounting error that
/// happens to cancel. 0.75 s then 0.25 s: the shortening is the sum, and the
/// second offset has to know about the first fade's full length, not its own.
#[test]
#[ignore = "needs ffmpeg/ffprobe on PATH (generates its own fixtures)"]
fn unequal_chained_fades_shorten_by_exactly_their_sum() {
    let red = tmp("sundayedit_chain2_red.mp4");
    let green = tmp("sundayedit_chain2_green.mp4");
    let blue = tmp("sundayedit_chain2_blue.mp4");
    let out = tmp("sundayedit_chain2_out.mp4");
    generate_solid(&red, "0xFF0000", W, H, 3.0);
    generate_solid(&green, "0x00FF00", W, H, 3.0);
    generate_solid(&blue, "0x0000FF", W, H, 3.0);

    let mut p = three_clip_project(
        [
            &red.to_string_lossy(),
            &green.to_string_lossy(),
            &blue.to_string_lossy(),
        ],
        false,
    );
    p.timeline_items[1].transition_in = fade(750);
    p.timeline_items[2].transition_in = fade(250);

    let expected_ms = rendered_duration_ms(&p);
    assert_eq!(expected_ms, 3 * CLIP_MS - 750 - 250);

    let args = build_filter_complex(
        &p,
        &canvas_settings(W, H, 25.0),
        None,
        &out.to_string_lossy(),
    )
    .expect("composable");
    run_compose_argv(&args, &out);

    let secs = container_duration_secs(&out);
    let want = expected_ms as f64 / 1000.0;
    assert!(
        (secs - want).abs() < 0.15,
        "unequal fades: rendered {secs:.3} s, expected {want:.3} s"
    );
    let px = sample_rgb(
        &out,
        want - 0.25,
        W as i64 / 2 - 8,
        H as i64 / 2 - 8,
        16,
        16,
    );
    assert!(
        px.2 > 90.0,
        "the third clip must reach the end, got rgb {px:?}"
    );

    for f in [&red, &green, &blue, &out] {
        let _ = std::fs::remove_file(f);
    }
}
