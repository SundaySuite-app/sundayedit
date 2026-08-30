//! MEASURED audio parity between what the compose graph says and what ffmpeg
//! actually renders (R2).
//!
//! The unit tests in `compose.rs` assert on the filter STRING. That proves the
//! builder emits what we meant to emit — it proves nothing about what comes
//! out of the encoder. A `volume=-6dB` node in the wrong place in the chain, a
//! fade positioned against timeline time instead of clip time, or an
//! `alimiter` whose auto-level quietly normalises the mix back up would all
//! sail past a string assertion. So these tests RENDER the file and measure it
//! with `volumedetect` / `astats`:
//!
//!   * a −6 dB gain really lands ~6 dB down;
//!   * a fade-in really starts at silence, and a fade-out really ends there;
//!   * two summed clips really do not exceed 0 dBFS;
//!   * a 2× clip really comes out at half the duration.
//!
//! The source is a constant-amplitude sine at a known level, which makes every
//! prediction a single number rather than an average over changing material.
//!
//! `#[ignore]`d like the other live compose tests. Run:
//!
//! ```sh
//! cargo test --manifest-path src-tauri/Cargo.toml \
//!   --test compose_audio_ffmpeg -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use sundayedit_lib::model::{
    Caption, ExportConfig, MediaItem, Project, ProjectMeta, Style, TimelineItem, TimelineItemKind,
    Track, TrackKind, Transform,
};
use sundayedit_lib::services::burnin::{Encoder, VideoCodec};
use sundayedit_lib::services::compose::ComposeSettings;
use sundayedit_lib::services::video::{parse_ffprobe_json, MediaKind};

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

const W: i32 = 160;
const H: i32 = 120;
const CLIP_MS: i64 = 4_000;
/// Target level for the generated sine, in dBFS. Comfortably below full scale
/// so a +12 dB boost still has somewhere to go before the limiter engages.
///
/// This is only the TARGET: `ffmpeg`'s `sine` source is not full scale (it
/// emits 1/8 amplitude, i.e. about -18 dBFS) and that is an implementation
/// detail of whichever ffmpeg is on PATH. Every assertion below therefore
/// measures the generated file and compares against `Fixture::src_dbfs`, so a
/// version whose sine sits somewhere else changes nothing.
const TARGET_DBFS: f64 = -12.0;

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

fn media(id: &str, path: &str) -> MediaItem {
    MediaItem {
        id: id.into(),
        path: path.into(),
        content_hash: format!("h-{id}"),
        kind: MediaKind::Video,
        duration_ms: CLIP_MS,
        width: W,
        height: H,
        fps: 30.0,
        has_audio: true,
        audio_wav_path: None,
        original_filename: format!("{id}.mkv"),
        added_at: 0,
    }
}

fn track(id: &str, index: i32) -> Track {
    Track {
        id: id.into(),
        kind: TrackKind::Video,
        name: id.into(),
        index,
        enabled: true,
        locked: false,
        muted: false,
        solo: false,
        volume_db: 0.0,
    }
}

fn item(id: &str, track_id: &str, media_id: &str, start: i64) -> TimelineItem {
    TimelineItem {
        id: id.into(),
        track_id: track_id.into(),
        kind: TimelineItemKind::Av,
        source_media_id: Some(media_id.into()),
        in_ms: 0,
        out_ms: CLIP_MS,
        timeline_start_ms: start,
        speed: 1.0,
        gain_db: 0.0,
        fade_in_ms: 0,
        fade_out_ms: 0,
        transform: Transform::default(),
        effects: vec![],
        transition_in: None,
        text: None,
        enabled: true,
        locked: false,
    }
}

fn project(media: Vec<MediaItem>, tracks: Vec<Track>, items: Vec<TimelineItem>) -> Project {
    Project {
        id: "p".into(),
        name: "t".into(),
        // Deliberately NOT any pooled media path: this project must never look
        // like the pristine baseline that takes the burn-in fast path.
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
        media,
        tracks,
        timeline_items: items,
    }
}

/// A constant-amplitude sine at a known level, muxed with a plain colour
/// video. Lossless FLAC audio, so the measurements below are of OUR filter
/// chain and not of a source codec's noise.
fn generate_tone(dst: &Path, freq: u32) {
    // `sine` is ~-18 dBFS, so lift it toward the target. The exact result is
    // measured afterwards rather than assumed.
    let amplitude = 10f64.powf((TARGET_DBFS + 18.06) / 20.0);
    let status = Command::new(ffmpeg())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=0x203040:s={W}x{H}:r=30:d={}", CLIP_MS / 1000),
            "-f",
            "lavfi",
            "-i",
            &format!(
                "sine=frequency={freq}:duration={},volume={amplitude}",
                CLIP_MS / 1000
            ),
            "-c:v",
            "ffv1",
            "-c:a",
            "flac",
            "-shortest",
        ])
        .arg(dst)
        .status()
        .expect("spawn ffmpeg (tone generation)");
    assert!(status.success(), "tone generation failed");
}

/// Render a project through the REAL compose builder.
fn render(p: &Project, out: &Path) -> String {
    let args = sundayedit_lib::services::compose::build_filter_complex(
        p,
        &settings(),
        None,
        &out.to_string_lossy(),
    )
    .expect("fixture must be composable");
    // Lossless audio out, so a measurement never blames the AAC encoder for
    // something our filter chain did (or did not) do.
    let mut args = args;
    if let Some(i) = args.iter().position(|a| a == "-c:a") {
        args[i + 1] = "pcm_s16le".into();
    }
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
    args.iter()
        .position(|a| a == "-filter_complex")
        .map(|i| args[i + 1].clone())
        .unwrap_or_default()
}

/// `(mean_dBFS, max_dBFS)` of a whole file, via `volumedetect`.
fn volume(file: &Path) -> (f64, f64) {
    let out = Command::new(ffmpeg())
        .args(["-hide_banner", "-nostats", "-i"])
        .arg(file)
        .args(["-map", "0:a", "-af", "volumedetect", "-f", "null", "-"])
        .output()
        .expect("spawn ffmpeg (volumedetect)");
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    let grab = |needle: &str| -> f64 {
        let line = text
            .lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no `{needle}` in volumedetect output:\n{text}"));
        line.split(needle)
            .nth(1)
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or_else(|| panic!("unparseable `{needle}`: {line}"))
    };
    (grab("mean_volume:"), grab("max_volume:"))
}

/// Peak level, in dBFS, of ONE time window — the tool that lets a fade be
/// measured where it actually happens instead of averaged away.
fn peak_dbfs_between(file: &Path, from_s: f64, to_s: f64) -> f64 {
    let out = Command::new(ffmpeg())
        .args(["-hide_banner", "-nostats", "-ss"])
        .arg(format!("{from_s}"))
        .arg("-to")
        .arg(format!("{to_s}"))
        .arg("-i")
        .arg(file)
        .args([
            "-map",
            "0:a",
            "-af",
            "astats=measure_overall=Peak_level:measure_perchannel=none",
            "-f",
            "null",
            "-",
        ])
        .output()
        .expect("spawn ffmpeg (astats)");
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    let line = text
        .lines()
        .find(|l| l.contains("Peak level dB:"))
        .unwrap_or_else(|| panic!("no peak level in astats output:\n{text}"));
    let raw = line.split("Peak level dB:").nth(1).unwrap().trim();
    // A fully silent window reports `-inf`.
    if raw.starts_with("-inf") {
        return f64::NEG_INFINITY;
    }
    raw.parse::<f64>()
        .unwrap_or_else(|_| panic!("unparseable peak level: {line}"))
}

fn duration_ms(file: &Path) -> i64 {
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
    parse_ffprobe_json(&String::from_utf8_lossy(&out.stdout))
        .expect("ffprobe json parses")
        .duration_ms
}

struct Fixture {
    dir: PathBuf,
    a: PathBuf,
    b: PathBuf,
    /// The MEASURED peak level of the generated sources, in dBFS. Every level
    /// assertion is relative to this, never to a hardcoded constant.
    src_dbfs: f64,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir();
        let a = dir.join(format!("sundayedit_aud_{tag}_a.mkv"));
        let b = dir.join(format!("sundayedit_aud_{tag}_b.mkv"));
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
        generate_tone(&a, 440);
        // A different frequency for the second source: two identical sines
        // could sum to +6 dB or cancel to silence depending on phase, and a
        // headroom test must not be a coin flip on phase.
        generate_tone(&b, 660);
        let src_dbfs = volume(&a).1;
        assert!(
            (src_dbfs - TARGET_DBFS).abs() < 3.0,
            "generated tone landed at {src_dbfs} dBFS, far from the {TARGET_DBFS} target — \
             the fixture, not the code under test, is broken"
        );
        let (b_peak, _) = (volume(&b).1, ());
        assert!(
            (b_peak - src_dbfs).abs() < 0.5,
            "the two sources must sit at the same level ({src_dbfs} vs {b_peak})"
        );
        Fixture {
            dir,
            a,
            b,
            src_dbfs,
        }
    }
    fn out(&self, tag: &str) -> PathBuf {
        let p = self.dir.join(format!("sundayedit_aud_{tag}.mkv"));
        let _ = std::fs::remove_file(&p);
        p
    }
    fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.a);
        let _ = std::fs::remove_file(&self.b);
    }
}

/// One clip, one track, with whatever audio settings the caller wants.
fn one_clip(f: &Fixture, gain_db: f32, fade_in: i64, fade_out: i64, track_db: f32) -> Project {
    let mut tr = track("t1", 0);
    tr.volume_db = track_db;
    let mut it = item("i0", "t1", "m1", 0);
    it.gain_db = gain_db;
    it.fade_in_ms = fade_in;
    it.fade_out_ms = fade_out;
    project(
        vec![media("m1", &f.a.to_string_lossy())],
        vec![tr],
        vec![it],
    )
}

// ── tests ────────────────────────────────────────────────────────────────────

/// The headline claim: "−6 dB" means −6 dB in the rendered file, not "a
/// `volume` node appeared somewhere in the graph".
#[test]
#[ignore = "needs the bundled ffmpeg/ffprobe (generates its own sample)"]
fn a_minus_6_db_gain_really_lands_6_db_down() {
    let f = Fixture::new("gain");
    let flat = f.out("gain_flat");
    let down = f.out("gain_down");

    render(&one_clip(&f, 0.0, 0, 0, 0.0), &flat);
    render(&one_clip(&f, -6.0, 0, 0, 0.0), &down);

    let (flat_mean, flat_max) = volume(&flat);
    let (down_mean, down_max) = volume(&down);

    let src = f.src_dbfs;
    assert!(
        (flat_max - src).abs() < 0.6,
        "the untouched render should still be ~{src} dBFS, got {flat_max}"
    );
    assert!(
        (flat_mean - down_mean - 6.0).abs() < 0.3,
        "mean should drop by 6 dB: {flat_mean} -> {down_mean}"
    );
    assert!(
        (flat_max - down_max - 6.0).abs() < 0.3,
        "peak should drop by 6 dB: {flat_max} -> {down_max}"
    );

    let _ = std::fs::remove_file(&flat);
    let _ = std::fs::remove_file(&down);
    f.cleanup();
}

/// The track fader is a real level, and it ADDS to the clip's gain in dB —
/// −4 on the clip plus −2 on the track is −6 in the file, not −4 and not −8.
#[test]
#[ignore = "needs the bundled ffmpeg/ffprobe (generates its own sample)"]
fn clip_gain_and_track_fader_add_in_db() {
    let f = Fixture::new("sum");
    let flat = f.out("sum_flat");
    let both = f.out("sum_both");

    render(&one_clip(&f, 0.0, 0, 0, 0.0), &flat);
    render(&one_clip(&f, -4.0, 0, 0, -2.0), &both);

    let (flat_mean, _) = volume(&flat);
    let (both_mean, _) = volume(&both);
    assert!(
        (flat_mean - both_mean - 6.0).abs() < 0.3,
        "-4 dB clip + -2 dB fader must be -6 dB total: {flat_mean} -> {both_mean}"
    );

    let _ = std::fs::remove_file(&flat);
    let _ = std::fs::remove_file(&both);
    f.cleanup();
}

/// A fade-in must START at silence and reach full level by the time it ends;
/// a fade-out must do the reverse. Measured in windows, because a whole-file
/// mean would happily pass with the ramp in the wrong place.
#[test]
#[ignore = "needs the bundled ffmpeg/ffprobe (generates its own sample)"]
fn a_fade_in_starts_silent_and_a_fade_out_ends_silent() {
    let f = Fixture::new("fade");
    let out = f.out("fade_both");
    // 1 s in, 1 s out, on a 4 s clip.
    render(&one_clip(&f, 0.0, 1000, 1000, 0.0), &out);

    let src = f.src_dbfs;
    // 10 ms into a 1 s linear ramp the gain is 0.01, i.e. 40 dB down.
    let head = peak_dbfs_between(&out, 0.0, 0.01);
    let middle = peak_dbfs_between(&out, 1.5, 2.5);
    let tail = peak_dbfs_between(&out, 3.99, 4.0);

    assert!(
        head < src - 30.0,
        "the first 10 ms of a 1 s fade-in must be near silence, got {head} dBFS"
    );
    assert!(
        (middle - src).abs() < 1.0,
        "the middle of the clip must be at full level (~{src}), got {middle} dBFS"
    );
    assert!(
        tail < src - 30.0,
        "the last 10 ms of a 1 s fade-out must be near silence, got {tail} dBFS"
    );

    let _ = std::fs::remove_file(&out);
    f.cleanup();
}

/// THE fade seam, measured. A clip placed at 0:02 with a 1 s fade-out must go
/// quiet at 0:05 — the end of the CLIP — not at 0:03 (fade positioned against
/// the clip's source length while ignoring the delay) and not never (fade
/// positioned against timeline time and landing past the clip's end).
#[test]
#[ignore = "needs the bundled ffmpeg/ffprobe (generates its own sample)"]
fn a_delayed_clips_fade_lands_at_the_clips_own_end() {
    let f = Fixture::new("fadepos");
    let out = f.out("fadepos_out");

    let mut it = item("i0", "t1", "m1", 2_000);
    it.fade_out_ms = 1_000;
    let p = project(
        vec![media("m1", &f.a.to_string_lossy())],
        vec![track("t1", 0)],
        vec![it],
    );
    render(&p, &out);

    // Timeline: silence 0–2 s, clip 2–6 s, fading out over 5–6 s.
    let src = f.src_dbfs;
    let before = peak_dbfs_between(&out, 0.0, 1.9);
    let steady = peak_dbfs_between(&out, 2.5, 4.5);
    let mid_fade = peak_dbfs_between(&out, 5.45, 5.55);
    let end = peak_dbfs_between(&out, 5.99, 6.0);

    assert!(
        before < src - 30.0,
        "nothing plays before the clip starts, got {before} dBFS"
    );
    assert!(
        (steady - src).abs() < 1.0,
        "the clip body must be at full level, got {steady} dBFS"
    );
    assert!(
        (mid_fade - (src - 6.0)).abs() < 2.0,
        "halfway through the fade should be ~6 dB down (~{}), got {mid_fade} dBFS — \
         a fade at the wrong second is exactly what this measures",
        src - 6.0
    );
    assert!(
        end < src - 30.0,
        "the clip must be near silence at its own end, got {end} dBFS"
    );

    let _ = std::fs::remove_file(&out);
    f.cleanup();
}

/// Headroom, measured. Two clips summing on a `normalize=0` bus, each boosted
/// so the raw sum would clip, must still come out at or below 0 dBFS.
#[test]
#[ignore = "needs the bundled ffmpeg/ffprobe (generates its own sample)"]
fn two_summed_clips_do_not_exceed_0_dbfs() {
    let f = Fixture::new("mix");
    let out = f.out("mix_out");

    let mut a = item("i0", "t1", "m1", 0);
    a.gain_db = 12.0;
    let mut b = item("i1", "t2", "m2", 0);
    b.gain_db = 12.0;
    let p = project(
        vec![
            media("m1", &f.a.to_string_lossy()),
            media("m2", &f.b.to_string_lossy()),
        ],
        vec![track("t1", 0), track("t2", 1)],
        vec![a, b],
    );
    let graph = render(&p, &out);
    assert!(graph.contains("amix=inputs=2:normalize=0"), "got {graph}");
    assert!(graph.contains("alimiter="), "got {graph}");

    let (mean, max) = volume(&out);
    // The whole point: two sources at ~0 dBFS after their +12 dB boost would
    // sum to about +6 dBFS on an unlimited `normalize=0` bus.
    assert!(
        max <= 0.0,
        "the summed bus clipped: max {max} dBFS (mean {mean})"
    );
    // …and the limiter must not have thrown the baby out either: the mix has
    // to still be LOUD, not squashed to nothing.
    assert!(
        max > -6.0,
        "the limiter over-attenuated: max {max} dBFS — a ceiling, not a fader"
    );

    let _ = std::fs::remove_file(&out);
    f.cleanup();
}

/// `alimiter`'s `level` option defaults to ENABLED, which multiplies the
/// output by `1/limit` — with our -1 dBFS ceiling that is a silent +1 dB on
/// everything the limiter touches, i.e. the render would be a decibel louder
/// than the level the user set and monitored.
///
/// So: with the limiter ARMED but nothing anywhere near the ceiling, a +2 dB
/// clip must come out exactly 2 dB up. This is the test that fails if
/// `level=disabled` is ever dropped from `BUS_LIMITER`; a loose "is it still
/// quiet?" assertion would sail straight past a 1 dB lie.
#[test]
#[ignore = "needs the bundled ffmpeg/ffprobe (generates its own sample)"]
fn the_bus_limiter_is_transparent_below_its_ceiling() {
    let f = Fixture::new("quiet");
    let flat = f.out("quiet_flat");
    let boosted = f.out("quiet_boost");

    // Reference: no gain, so no limiter at all.
    let flat_graph = render(&one_clip(&f, 0.0, 0, 0, 0.0), &flat);
    assert!(!flat_graph.contains("alimiter="), "got {flat_graph}");

    // +2 dB ARMS the limiter (any boost can clip in principle) but leaves the
    // signal ~10 dB below the ceiling, so a correct limiter does nothing.
    let boost_graph = render(&one_clip(&f, 2.0, 0, 0, 0.0), &boosted);
    assert!(boost_graph.contains("alimiter="), "got {boost_graph}");

    let (_, flat_max) = volume(&flat);
    let (_, boost_max) = volume(&boosted);
    assert!(
        boost_max < -5.0,
        "the fixture must stay well below the ceiling for this to mean anything, got {boost_max}"
    );
    assert!(
        (boost_max - flat_max - 2.0).abs() < 0.3,
        "a +2 dB clip must land exactly 2 dB up ({flat_max} -> {boost_max}) — \
         any extra gain is alimiter's auto-level rewriting the user's mix"
    );

    let _ = std::fs::remove_file(&flat);
    let _ = std::fs::remove_file(&boosted);
    f.cleanup();
}

/// The same claim on a real MIX: two deliberately quiet clips must stay quiet.
#[test]
#[ignore = "needs the bundled ffmpeg/ffprobe (generates its own sample)"]
fn a_quiet_mix_is_not_normalised_back_up() {
    let f = Fixture::new("quietmix");
    let out = f.out("quietmix_out");

    let mut a = item("i0", "t1", "m1", 0);
    a.gain_db = -20.0;
    let mut b = item("i1", "t2", "m2", 0);
    b.gain_db = -20.0;
    let p = project(
        vec![
            media("m1", &f.a.to_string_lossy()),
            media("m2", &f.b.to_string_lossy()),
        ],
        vec![track("t1", 0), track("t2", 1)],
        vec![a, b],
    );
    render(&p, &out);

    let (_, max) = volume(&out);
    // Two tones 20 dB down sum to at most ~6 dB above one of them.
    let ceiling = f.src_dbfs - 20.0 + 6.5;
    assert!(
        max < ceiling,
        "a deliberately quiet mix came back at {max} dBFS (expected below {ceiling})"
    );

    let _ = std::fs::remove_file(&out);
    f.cleanup();
}

/// Speed used to be modelled everywhere and ignored by the export. A 2× clip
/// must now actually come out at half the duration — in the file, not in a
/// string.
#[test]
#[ignore = "needs the bundled ffmpeg/ffprobe (generates its own sample)"]
fn a_2x_clip_really_lands_at_half_duration() {
    let f = Fixture::new("speed");
    let normal = f.out("speed_1x");
    let fast = f.out("speed_2x");

    render(&one_clip(&f, 0.0, 0, 0, 0.0), &normal);

    let mut it = item("i0", "t1", "m1", 0);
    it.speed = 2.0;
    let p = project(
        vec![media("m1", &f.a.to_string_lossy())],
        vec![track("t1", 0)],
        vec![it],
    );
    render(&p, &fast);

    let d1 = duration_ms(&normal);
    let d2 = duration_ms(&fast);
    assert!(
        (d1 - CLIP_MS).abs() < 200,
        "the 1x render should be ~{CLIP_MS} ms, got {d1}"
    );
    assert!(
        (d2 - CLIP_MS / 2).abs() < 200,
        "the 2x render should be ~{} ms, got {d2} — the export ignored `speed`",
        CLIP_MS / 2
    );

    // The AUDIO has to travel with it: a video-only `setpts` would leave 4 s of
    // tone under 2 s of picture. Level is preserved by `atempo`, so a
    // full-level tone across the whole (halved) file proves the audio was
    // time-compressed rather than truncated.
    let tail = peak_dbfs_between(&fast, 1.8, 2.0);
    assert!(
        (tail - f.src_dbfs).abs() < 1.5,
        "the sped-up clip must still be sounding at its end, got {tail} dBFS"
    );

    let _ = std::fs::remove_file(&normal);
    let _ = std::fs::remove_file(&fast);
    f.cleanup();
}

/// A 4× clip needs `atempo=2,atempo=2` — one `atempo=4.0` is REJECTED by
/// ffmpeg and would abort the whole export, so the chaining is what makes
/// speed usable at all outside 0.5..2.
#[test]
#[ignore = "needs the bundled ffmpeg/ffprobe (generates its own sample)"]
fn a_4x_clip_renders_through_a_chained_atempo() {
    let f = Fixture::new("speed4");
    let out = f.out("speed_4x");

    let mut it = item("i0", "t1", "m1", 0);
    it.speed = 4.0;
    let p = project(
        vec![media("m1", &f.a.to_string_lossy())],
        vec![track("t1", 0)],
        vec![it],
    );
    let graph = render(&p, &out);
    assert_eq!(
        graph.matches("atempo=").count(),
        2,
        "4x needs two atempo stages: {graph}"
    );

    let d = duration_ms(&out);
    assert!(
        (d - CLIP_MS / 4).abs() < 200,
        "the 4x render should be ~{} ms, got {d}",
        CLIP_MS / 4
    );

    let _ = std::fs::remove_file(&out);
    f.cleanup();
}
