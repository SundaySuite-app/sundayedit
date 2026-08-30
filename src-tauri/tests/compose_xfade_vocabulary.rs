//! Seam guard: ClipInspector transition vocabulary ↔ ffmpeg `xfade` enum.
//!
//! The Clip Inspector offers `TRANSITION_KINDS = ["", "fade", "crossfade",
//! "dip"]` (src/features/timeline/ClipInspector.tsx) and the chosen kind
//! string flows verbatim through `op_set_transition` into the project.
//! ffmpeg's `xfade` filter accepts a fixed enum of transition names (fade,
//! dissolve, fadeblack, …) that contains NEITHER `crossfade` NOR `dip` —
//! `build_filter_complex` therefore maps the friendly UI names to real xfade
//! names at the seam (`xfade_transition_name`: crossfade→dissolve,
//! dip→fadeblack). Regression (seam-xfade-transition-vocabulary): the kind
//! used to be emitted literally, so 2 of the 3 picker options aborted the
//! export with `Unable to parse option value "crossfade" as transition`,
//! while both layers stayed individually green.
//!
//! These tests pin the seam shut from both ends:
//!
//!   1. `ui_transition_kinds_survive_the_compose_seam` — reads the picker
//!      vocabulary out of ClipInspector.tsx, pushes each kind through the real
//!      `build_filter_complex`, extracts the emitted `xfade=transition=<name>`
//!      and asserts the bundled ffmpeg actually accepts that name (via
//!      `ffmpeg -h filter=xfade`). Deterministic, no sample videos needed —
//!      and it keeps guarding any FUTURE picker kind added without a mapping.
//!
//!   2. `every_ui_transition_kind_composes_with_real_ffmpeg` — the end-user
//!      path: a real two-clip compose per picker kind. `#[ignore]`d like the
//!      other live compose tests; needs SUNDAYEDIT_TEST_VIDEO + ffmpeg on
//!      PATH:
//!
//!      ```sh
//!      SUNDAYEDIT_TEST_VIDEO=/path/to/sample.mp4 \
//!        cargo test --manifest-path src-tauri/Cargo.toml \
//!        --test compose_xfade_vocabulary -- --ignored
//!      ```

use std::path::{Path, PathBuf};
use std::process::Command;

use sundayedit_lib::model::{
    MediaItem, Project, Style, TimelineItem, TimelineItemKind, Track, TrackKind, Transform,
    Transition,
};
use sundayedit_lib::services::burnin::{Encoder, VideoCodec};
use sundayedit_lib::services::compose::ComposeSettings;
use sundayedit_lib::services::video::MediaKind;

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

// ── ffmpeg resolution ────────────────────────────────────────────────────────

/// The bundled sidecar ffmpeg (what the shipped app runs), falling back to
/// whatever `ffmpeg` is on PATH so the test also works on CI runners.
fn ffmpeg_bin() -> String {
    let bindir = Path::new(env!("CARGO_MANIFEST_DIR")).join("binaries");
    for name in [
        "ffmpeg-aarch64-apple-darwin",
        "ffmpeg-x86_64-apple-darwin",
        "ffmpeg-universal-apple-darwin",
    ] {
        let p = bindir.join(name);
        if p.exists() {
            return p.to_string_lossy().into_owned();
        }
    }
    "ffmpeg".into()
}

/// The transition names ffmpeg's `xfade` filter actually accepts, parsed from
/// `ffmpeg -h filter=xfade`. Enum rows look like `     fade   0   ...` —
/// a name token followed by an integer value.
fn xfade_vocabulary() -> Vec<String> {
    let out = Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-h", "filter=xfade"])
        .output()
        .expect("spawn ffmpeg -h filter=xfade");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    let mut names = Vec::new();
    for line in text.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.len() >= 2 && toks[1].parse::<i64>().is_ok() {
            names.push(toks[0].to_string());
        }
    }
    assert!(
        names.iter().any(|n| n == "fade"),
        "failed to parse xfade vocabulary out of ffmpeg -h filter=xfade:\n{text}"
    );
    names
}

// ── UI vocabulary ────────────────────────────────────────────────────────────

/// The non-empty transition kinds the Clip Inspector picker offers, read from
/// the ACTUAL frontend source so the test tracks the real UI vocabulary.
fn ui_transition_kinds() -> Vec<String> {
    let tsx =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/features/timeline/ClipInspector.tsx");
    let src =
        std::fs::read_to_string(&tsx).unwrap_or_else(|e| panic!("read {}: {e}", tsx.display()));
    let start = src
        .find("TRANSITION_KINDS")
        .expect("ClipInspector.tsx declares TRANSITION_KINDS");
    let open = src[start..].find('[').expect("array literal") + start;
    let close = src[open..].find(']').expect("array literal end") + open;
    let kinds: Vec<String> = src[open + 1..close]
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        kinds.contains(&"fade".to_string()),
        "picker vocabulary parse broke: {kinds:?}"
    );
    kinds
}

// ── Project scaffolding (mirrors the compose.rs test helpers) ────────────────

fn settings() -> ComposeSettings {
    ComposeSettings {
        width: 640,
        height: 480,
        fps: 30.0,
        codec: VideoCodec::H264,
        encoder: Encoder::Cpu,
        bitrate_kbps: None,
    }
}

fn media(id: &str, path: &str, audio: bool) -> MediaItem {
    MediaItem {
        id: id.into(),
        path: path.into(),
        content_hash: "h".into(),
        kind: MediaKind::Video,
        duration_ms: 60_000,
        width: 640,
        height: 480,
        fps: 30.0,
        has_audio: audio,
        audio_wav_path: None,
        original_filename: format!("{id}.mp4"),
        added_at: 0,
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
        volume_db: 0.0,
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
        video_path: "/x.mp4".into(),
        video_content_hash: "h".into(),
        video_duration_ms: 60_000,
        video_width: 640,
        video_height: 480,
        video_fps: 30.0,
        audio_wav_path: None,
        language: "no".into(),
        default_style: Style::broadcast_news(),
        context_description: None,
        captions: vec![],
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

/// Two adjacent clips on one video track, the second entering through the
/// given picker transition kind — exactly what ClipInspector's picker +
/// `op_set_transition` produce.
fn two_clip_project(source: &str, kind: &str) -> Project {
    let mut second = item("i1", "v1", "m1", 2500, 0, 3000);
    second.transition_in = Some(Transition {
        kind: kind.into(),
        duration_ms: 500,
    });
    project(
        vec![media("m1", source, false)],
        vec![track("v1", TrackKind::Video, 0)],
        vec![item("i0", "v1", "m1", 0, 0, 3000), second],
    )
}

/// Pull the `-filter_complex` graph out of the built arg list.
fn fc(args: &[String]) -> String {
    let i = args
        .iter()
        .position(|a| a == "-filter_complex")
        .expect("compose args carry -filter_complex");
    args[i + 1].clone()
}

/// Extract the `transition=<name>` value the graph hands to xfade.
fn emitted_transition_name(graph: &str) -> String {
    let i = graph
        .find("xfade=transition=")
        .expect("graph contains an xfade node")
        + "xfade=transition=".len();
    graph[i..]
        .chars()
        .take_while(|c| *c != ':' && *c != ',' && *c != '[')
        .collect()
}

// ── The seam test ────────────────────────────────────────────────────────────

/// Every transition kind the Clip Inspector picker offers must — after passing
/// through the real `build_filter_complex` — reach ffmpeg as a name its
/// `xfade` filter actually accepts. Fails today for "crossfade" and "dip".
#[test]
fn ui_transition_kinds_survive_the_compose_seam() {
    let vocab = xfade_vocabulary();
    let mut invalid = Vec::new();

    for kind in ui_transition_kinds() {
        let p = two_clip_project("/fake/source.mp4", &kind);
        let args = build_filter_complex(&p, &settings(), None, "/fake/out.mp4");
        let emitted = emitted_transition_name(&fc(&args));
        if !vocab.contains(&emitted) {
            invalid.push((kind, emitted));
        }
    }

    assert!(
        invalid.is_empty(),
        "picker transition kinds that reach ffmpeg as names its xfade filter \
         rejects (the compose export aborts with 'Unable to parse option value'): \
         {invalid:?}\naccepted xfade names: {vocab:?}"
    );
}

// ── The live end-user failure ────────────────────────────────────────────────

/// Run the real compose pipeline once per picker kind: two adjacent clips,
/// transition picked in the inspector, multi-track export. With "crossfade" or
/// "dip", ffmpeg exits non-zero while parsing the filter graph and no output
/// file is produced.
#[test]
#[ignore = "needs SUNDAYEDIT_TEST_VIDEO + ffmpeg on PATH"]
fn every_ui_transition_kind_composes_with_real_ffmpeg() {
    let sample = std::env::var("SUNDAYEDIT_TEST_VIDEO").expect("set SUNDAYEDIT_TEST_VIDEO");
    let mut failed = Vec::new();

    for kind in ui_transition_kinds() {
        let out: PathBuf = std::env::temp_dir().join(format!("sundayedit_repro_xfade_{kind}.mp4"));
        let _ = std::fs::remove_file(&out);

        let p = two_clip_project(&sample, &kind);
        let out_str = out.to_string_lossy().into_owned();
        let args = build_filter_complex(&p, &settings(), None, &out_str);

        let status = Command::new("ffmpeg")
            .args(&args)
            .status()
            .expect("spawn ffmpeg");
        if !status.success() || !out.exists() {
            failed.push(kind);
        }
        let _ = std::fs::remove_file(&out);
    }

    assert!(
        failed.is_empty(),
        "compose export failed (ffmpeg rejected the filter graph) for picker \
         transition kinds: {failed:?}"
    );
}
