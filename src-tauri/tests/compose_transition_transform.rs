//! Seam guard (seam-xfade-drops-transform): a clip's `Transform` must survive
//! a transition — MEASURED in the rendered pixels, not asserted on the
//! filtergraph string.
//!
//! Regression: `build_filter_complex`'s xfade branch emitted
//! `[pv{n}]scale={W}:{H}` for the incoming clip. That did two silent things:
//! it OVERWROTE the transform's own `scale=iw*s:ih*s`, and — because the
//! branch never reached the `overlay` that carries the placement — it dropped
//! `transform.x` / `transform.y` entirely. A clip inset at 40 % in the
//! top-right corner previewed as an inset (the Pixi compositor positions it at
//! `round(width * t.x)`, `round(height * t.y)`) and EXPORTED full-frame. Both
//! layers were internally consistent; only the seam lied.
//!
//! The fix composites the transformed clip onto its own full-frame black
//! canvas at the very offsets the plain `overlay` branch uses, then hands that
//! full-frame stream to `xfade` — so both xfade inputs share geometry AND the
//! transform survives.
//!
//! What the tests below measure: distinctly coloured solid sources are
//! rendered, then single frames are sampled with `crop` + raw rgb24 and
//! averaged. The inset rectangle's four edges are probed from both sides, so a
//! wrong offset OR a wrong scale fails. The second test renders the SAME
//! project twice — with and without the transition — and requires identical
//! placement, which is the drift guard: if either branch's geometry changes
//! alone, it goes red.
//!
//! Run (needs `ffmpeg` on PATH; generates its own samples):
//! ```sh
//! cargo test --manifest-path src-tauri/Cargo.toml \
//!   --test compose_transition_transform -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use sundayedit_lib::model::{
    Caption, MediaItem, Project, Style, TimelineItem, TimelineItemKind, Track, TrackKind,
    Transform, Transition,
};
use sundayedit_lib::services::burnin::{Encoder, VideoCodec};
use sundayedit_lib::services::compose::{build_filter_complex, ComposeSettings};
use sundayedit_lib::services::video::MediaKind;

// ── output geometry the whole file reasons in ────────────────────────────────

const OUT_W: i32 = 640;
const OUT_H: i32 = 480;
/// The inset transform under test: 40 % scale, parked in the top-right.
const SCALE: f32 = 0.4;
const T_X: f32 = 0.55;
const T_Y: f32 = 0.10;

/// Where the inset MUST land, in output pixels — the same arithmetic
/// `compositor/scene.ts::describeScene` uses for the preview layer.
fn expected_rect() -> (i64, i64, i64, i64) {
    let x = (OUT_W as f32 * T_X).round() as i64;
    let y = (OUT_H as f32 * T_Y).round() as i64;
    // The sources are authored at the output size, so the drawn size is just
    // the source size times the transform scale.
    let w = (OUT_W as f32 * SCALE).round() as i64;
    let h = (OUT_H as f32 * SCALE).round() as i64;
    (x, y, w, h)
}

// ── fixtures ─────────────────────────────────────────────────────────────────

fn project(media: Vec<MediaItem>, tracks: Vec<Track>, items: Vec<TimelineItem>) -> Project {
    Project {
        id: "p".into(),
        name: "t".into(),
        video_path: "/x.mp4".into(),
        video_content_hash: "h".into(),
        video_duration_ms: 4_000,
        video_width: OUT_W,
        video_height: OUT_H,
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
        export_config: sundayedit_lib::model::ExportConfig::default(),
        project_meta: sundayedit_lib::model::ProjectMeta::default(),
        created_at: 0,
        updated_at: 0,
        media,
        tracks,
        timeline_items: items,
    }
}

fn media(id: &str, path: &str) -> MediaItem {
    MediaItem {
        id: id.into(),
        path: path.into(),
        content_hash: "h".into(),
        kind: MediaKind::Video,
        duration_ms: 3_000,
        width: OUT_W,
        height: OUT_H,
        fps: 30.0,
        has_audio: false,
        audio_wav_path: None,
        original_filename: format!("{id}.mp4"),
        added_at: 0,
    }
}

fn item(
    id: &str,
    track_id: &str,
    media_id: &str,
    start: i64,
    in_ms: i64,
    out_ms: i64,
) -> TimelineItem {
    TimelineItem {
        id: id.into(),
        track_id: track_id.into(),
        kind: TimelineItemKind::Av,
        source_media_id: Some(media_id.into()),
        in_ms,
        out_ms,
        timeline_start_ms: start,
        speed: 1.0,
        transform: Transform::default(),
        effects: vec![],
        transition_in: None,
        text: None,
        enabled: true,
        locked: false,
    }
}

fn track(id: &str, kind: TrackKind, index: i32) -> Track {
    Track {
        id: id.into(),
        kind,
        name: id.into(),
        index,
        enabled: true,
        locked: false,
        muted: false,
        solo: false,
    }
}

fn settings() -> ComposeSettings {
    ComposeSettings {
        width: OUT_W,
        height: OUT_H,
        fps: 30.0,
        codec: VideoCodec::H264,
        encoder: Encoder::Cpu,
        bitrate_kbps: None,
    }
}

// ── ffmpeg helpers ───────────────────────────────────────────────────────────

/// Render a SOLID-colour clip. Solid colours make a sampled mean unambiguous:
/// "is this region the incoming clip, the outgoing clip, or the canvas?" has a
/// numeric answer that no codec ringing can blur past the thresholds below.
fn generate_solid(dst: &Path, colour: &str, seconds: f64) {
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
            &format!("color=c={colour}:s={OUT_W}x{OUT_H}:r=30:d={seconds}"),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-an",
        ])
        .arg(dst)
        .status()
        .expect("spawn ffmpeg (sample generation)");
    assert!(st.success(), "solid sample generation failed for {colour}");
}

/// Mean RGB of a `w`x`h` patch at (`x`,`y`) in the frame at `t` seconds.
/// Output-side `-ss` (after `-i`) so the seek is frame-accurate.
fn sample_rgb(video: &Path, t: f64, x: i64, y: i64, w: i64, h: i64) -> (f64, f64, f64) {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(video)
        .args([
            "-ss",
            &format!("{t}"),
            "-frames:v",
            "1",
            "-vf",
            &format!("crop={w}:{h}:{x}:{y}"),
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-an",
            "-",
        ])
        .output()
        .expect("spawn ffmpeg (sample)");
    assert!(
        out.status.success(),
        "sampling {video:?} at t={t} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let px = out.stdout;
    let expect = (w * h * 3) as usize;
    assert_eq!(
        px.len(),
        expect,
        "expected {expect} raw bytes at t={t} ({w}x{h}), got {}",
        px.len()
    );
    let (mut r, mut g, mut b) = (0f64, 0f64, 0f64);
    for chunk in px.as_chunks::<3>().0 {
        r += chunk[0] as f64;
        g += chunk[1] as f64;
        b += chunk[2] as f64;
    }
    let n = (w * h) as f64;
    (r / n, g / n, b / n)
}

fn assert_greenish(px: (f64, f64, f64), what: &str) {
    assert!(
        px.1 > 90.0 && px.0 < 90.0,
        "{what} must show the INCOMING clip (green), got rgb {px:?}"
    );
}

fn assert_black(px: (f64, f64, f64), what: &str) {
    assert!(
        px.0 < 40.0 && px.1 < 40.0 && px.2 < 40.0,
        "{what} must be empty canvas (black), got rgb {px:?}"
    );
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

/// Two sequential clips on one track: red, then a 40 %-scale green inset in
/// the top-right. `with_transition` decides whether the second clip enters via
/// an `xfade` (the branch that used to lose the transform) or a plain
/// `overlay`. Rendered to `out`.
fn render(out: &Path, red: &Path, green: &Path, with_transition: bool) {
    let _ = std::fs::remove_file(out);
    let mut second = item("b", "t1", "m2", 2000, 0, 2000);
    second.transform = Transform {
        x: T_X,
        y: T_Y,
        scale: SCALE,
        ..Transform::default()
    };
    if with_transition {
        second.transition_in = Some(Transition {
            kind: "fade".into(),
            duration_ms: 500,
        });
    }
    let p = project(
        vec![
            media("m1", &red.to_string_lossy()),
            media("m2", &green.to_string_lossy()),
        ],
        vec![track("t1", TrackKind::Video, 0)],
        vec![item("a", "t1", "m1", 0, 0, 2000), second],
    );

    let args = build_filter_complex(&p, &settings(), None, &out.to_string_lossy())
        .expect("fixture is composable");
    let res = Command::new("ffmpeg")
        .args(&args)
        .output()
        .expect("spawn ffmpeg (compose)");
    assert!(
        res.status.success(),
        "compose render failed (with_transition={with_transition}); argv: {args:?}\nstderr:\n{}",
        String::from_utf8_lossy(&res.stderr)
    );
    assert!(out.exists(), "compose wrote nothing to {out:?}");
}

/// Probe the four edges of the inset from BOTH sides at `t`. A wrong offset or
/// a wrong scale moves at least one edge and fails.
fn assert_inset_placement(out: &Path, t: f64, label: &str) {
    let (x, y, w, h) = expected_rect();
    let s = 16i64; // patch size — small enough to sit clear of every edge

    // Inside, near each corner of the inset.
    assert_greenish(
        sample_rgb(out, t, x + 4, y + 4, s, s),
        &format!("{label}: inside top-left of the inset"),
    );
    assert_greenish(
        sample_rgb(out, t, x + w - s - 4, y + h - s - 4, s, s),
        &format!("{label}: inside bottom-right of the inset"),
    );

    // Outside each edge — this is what a full-frame export fails.
    assert_black(
        sample_rgb(out, t, x - s - 6, y + h / 2, s, s),
        &format!("{label}: left of the inset"),
    );
    assert_black(
        sample_rgb(out, t, x + w + 6, y + h / 2, s, s),
        &format!("{label}: right of the inset"),
    );
    assert_black(
        sample_rgb(out, t, x + w / 2, y - s - 6, s, s),
        &format!("{label}: above the inset"),
    );
    assert_black(
        sample_rgb(out, t, x + w / 2, y + h + 6, s, s),
        &format!("{label}: below the inset"),
    );

    // Far corners: the frame is overwhelmingly empty canvas, not a stretched clip.
    assert_black(
        sample_rgb(out, t, 8, OUT_H as i64 - s - 8, s, s),
        &format!("{label}: bottom-left corner"),
    );
}

// ── the tests ────────────────────────────────────────────────────────────────

/// A scaled + offset clip that enters via a TRANSITION must land exactly where
/// the transform says. Before the fix this rendered the green clip full-frame
/// and every "outside the inset" probe came back green.
#[test]
#[ignore = "needs ffmpeg on PATH (generates its own samples)"]
fn transitioned_clip_keeps_its_transform_placement() {
    let red = tmp("sundayedit_tt_red.mp4");
    let green = tmp("sundayedit_tt_green.mp4");
    let out = tmp("sundayedit_tt_xfade.mp4");
    generate_solid(&red, "red", 3.0);
    generate_solid(&green, "green", 3.0);

    render(&out, &red, &green, true);

    // Before the transition: the first clip fills the frame.
    let early = sample_rgb(&out, 0.5, 300, 240, 16, 16);
    assert!(
        early.0 > 90.0 && early.1 < 90.0,
        "at t=0.5 the OUTGOING clip (red) must fill the frame, got rgb {early:?}"
    );

    // xfade offset = prev_end(2.0s) - 0.5s = 1.5s, so the blend runs
    // 1.5s..2.0s and the incoming clip is alone from 2.0s. The xfade output
    // ends at offset + incoming duration = 3.5s; sample comfortably inside.
    assert_inset_placement(&out, 3.0, "after the transition");

    // And the transition really is a fade, not a hard cut: mid-blend the
    // outgoing red is dimmed but not gone (the incoming stream's canvas is
    // black there).
    let mid = sample_rgb(&out, 1.75, 60, 240, 16, 16);
    assert!(
        mid.0 > 15.0 && mid.0 < early.0 * 0.85,
        "mid-transition the outgoing clip must be partially faded \
         (t=1.75 red {mid:?} vs t=0.5 red {early:?})"
    );

    for f in [&red, &green, &out] {
        let _ = std::fs::remove_file(f);
    }
}

/// DRIFT GUARD: the transition branch and the plain-overlay branch must place
/// an identically-transformed clip in the SAME pixels. The two branches are
/// separate code paths in `build_filter_complex`; this is the test that goes
/// red if either one's geometry changes alone.
#[test]
#[ignore = "needs ffmpeg on PATH (generates its own samples)"]
fn transition_and_plain_branch_place_the_clip_identically() {
    let red = tmp("sundayedit_tt2_red.mp4");
    let green = tmp("sundayedit_tt2_green.mp4");
    let with_tr = tmp("sundayedit_tt2_with.mp4");
    let without_tr = tmp("sundayedit_tt2_without.mp4");
    generate_solid(&red, "red", 3.0);
    generate_solid(&green, "green", 3.0);

    render(&with_tr, &red, &green, true);
    render(&without_tr, &red, &green, false);

    // Both must satisfy the same absolute placement…
    assert_inset_placement(&with_tr, 3.0, "with transition");
    assert_inset_placement(&without_tr, 3.0, "without transition");

    // …and agree patch-for-patch along a horizontal scan across the inset's
    // left edge, which is where an offset error shows up first.
    let (x, y, _w, h) = expected_rect();
    for dx in [-40i64, -20, -6, 6, 20, 40] {
        let px = x + dx;
        let a = sample_rgb(&with_tr, 3.0, px, y + h / 2, 4, 4);
        let b = sample_rgb(&without_tr, 3.0, px, y + h / 2, 4, 4);
        let same_side = (a.1 > 90.0) == (b.1 > 90.0);
        assert!(
            same_side,
            "the two branches disagree {dx} px from the inset's left edge: \
             with-transition rgb {a:?} vs without-transition rgb {b:?}"
        );
    }

    for f in [&red, &green, &with_tr, &without_tr] {
        let _ = std::fs::remove_file(f);
    }
}

/// Two transitions in ONE graph: each incoming clip now brings its OWN `color`
/// canvas source node, so this pins that a second source node in the same
/// `filter_complex` still composes (a graph error here would abort the whole
/// render).
///
/// NOTE — deliberately not asserted here: the rendered timeline is TRUNCATED
/// for chained transitions. `offset` is computed in timeline coordinates
/// (`prev.timeline_end_ms - transition.duration_ms`) while the stream it is
/// applied to is the PREVIOUS xfade's output, which is already shorter by the
/// earlier transition — with equal transition durations the second offset lands
/// exactly at its input's end and the tail is lost. That is a PRE-EXISTING
/// defect of the offset arithmetic (verified: the same 3-clip project truncates
/// identically on the pre-fix `scale={W}:{H}` branch, 3.467 s vs 3.434 s), and
/// is out of scope for the transform fix.
#[test]
#[ignore = "needs ffmpeg on PATH (generates its own samples)"]
fn two_transitions_in_one_graph_still_compose() {
    let red = tmp("sundayedit_tt3_red.mp4");
    let green = tmp("sundayedit_tt3_green.mp4");
    let blue = tmp("sundayedit_tt3_blue.mp4");
    let out = tmp("sundayedit_tt3_out.mp4");
    generate_solid(&red, "red", 3.0);
    generate_solid(&green, "green", 3.0);
    generate_solid(&blue, "blue", 3.0);

    let inset = Transform {
        x: T_X,
        y: T_Y,
        scale: SCALE,
        ..Transform::default()
    };
    let fade = || {
        Some(Transition {
            kind: "fade".into(),
            duration_ms: 500,
        })
    };

    let mut b = item("b", "t1", "m2", 2000, 0, 2000);
    b.transform = inset.clone();
    b.transition_in = fade();
    let mut c = item("c", "t1", "m3", 4000, 0, 2000);
    c.transform = inset.clone();
    c.transition_in = fade();

    let p = project(
        vec![
            media("m1", &red.to_string_lossy()),
            media("m2", &green.to_string_lossy()),
            media("m3", &blue.to_string_lossy()),
        ],
        vec![track("t1", TrackKind::Video, 0)],
        vec![item("a", "t1", "m1", 0, 0, 2000), b, c],
    );
    let _ = std::fs::remove_file(&out);
    let args = build_filter_complex(&p, &settings(), None, &out.to_string_lossy())
        .expect("fixture is composable");
    let graph = args
        .iter()
        .position(|a| a == "-filter_complex")
        .map(|i| args[i + 1].clone())
        .expect("a composite graph");
    assert!(
        graph.contains("[xc1]") && graph.contains("[xc2]"),
        "each transitioned clip needs its own full-frame canvas: {graph}"
    );

    let res = Command::new("ffmpeg")
        .args(&args)
        .output()
        .expect("spawn ffmpeg");
    assert!(
        res.status.success(),
        "two transitions must compose; argv: {args:?}\nstderr:\n{}",
        String::from_utf8_lossy(&res.stderr)
    );

    // The SECOND transitioned clip is still an inset, not a stretched frame:
    // probe just after the first transition completes (t = 2.5 s), where the
    // green clip owns the composite.
    let (x, y, _w, h) = expected_rect();
    let inside = sample_rgb(&out, 2.5, x + 4, y + h / 2, 16, 16);
    assert_greenish(inside, "second clip after the first transition");
    assert_black(
        sample_rgb(&out, 2.5, x - 30, y + h / 2, 16, 16),
        "left of the second clip's inset",
    );

    for f in [&red, &green, &blue, &out] {
        let _ = std::fs::remove_file(f);
    }
}
