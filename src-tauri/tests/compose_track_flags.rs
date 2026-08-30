//! Track-flag parity between preview and compose export.
//!
//! The live preview honours track-level flags — `previewMap.ts` skips video
//! tracks with `enabled === false`, and the Timeline track headers expose
//! mute/solo buttons persisted via `op_set_track_flags`. `build_filter_complex`
//! must apply the same semantics: a disabled track's clips are not composited,
//! a muted audio track is not mixed, and while any track is soloed only
//! soloed tracks are audible — otherwise the export contradicts the preview,
//! breaking the "what gets exported must match what the user saw in preview"
//! promise (CLAUDE.md).
//!
//! Regression guards for seam-compose-ignores-track-flags /
//! ops-track-mute-solo-dead / diff-export-ignores-track-mute.

use sundayedit_lib::model::{
    MediaItem, Project, Style, TimelineItem, TimelineItemKind, Track, TrackKind, Transform,
};
use sundayedit_lib::services::compose::{is_simple_timeline, ComposeSettings};
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

fn media(id: &str, path: &str, audio: bool) -> MediaItem {
    MediaItem {
        id: id.into(),
        path: path.into(),
        content_hash: "h".into(),
        kind: MediaKind::Video,
        duration_ms: 60_000,
        width: 1920,
        height: 1080,
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

fn project(media: Vec<MediaItem>, tracks: Vec<Track>, items: Vec<TimelineItem>) -> Project {
    Project {
        id: "p".into(),
        name: "t".into(),
        video_path: "/x.mp4".into(),
        video_content_hash: "h".into(),
        video_duration_ms: 60_000,
        video_width: 1920,
        video_height: 1080,
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

fn fc(args: &[String]) -> String {
    let i = args.iter().position(|a| a == "-filter_complex").unwrap();
    args[i + 1].clone()
}

/// A video track with `enabled = false` is invisible in the preview
/// (previewMap.ts skips it), so its clips must NOT be composited into the
/// export.
#[test]
fn disabled_video_track_is_excluded_from_export() {
    let mut off_track = track("v2", TrackKind::Video, 1);
    off_track.enabled = false;

    let p = project(
        vec![media("m1", "/a.mp4", false), media("m2", "/b.mp4", false)],
        vec![track("v1", TrackKind::Video, 0), off_track],
        vec![
            item("i0", "v1", "m1", 0, 0, 2000),
            // Clip on the DISABLED track — hidden in preview.
            item("i1", "v2", "m2", 0, 0, 2000),
        ],
    );
    let g = fc(&build_filter_complex(
        &p,
        &ComposeSettings::default(),
        None,
        "out.mp4",
    ));

    assert!(
        !g.contains("[pv1]"),
        "clip on disabled video track v2 was composited into the export \
         (preview hides it): {g}"
    );
}

/// Same rule for a disabled Overlay track.
#[test]
fn disabled_overlay_track_is_excluded_from_export() {
    let mut hidden = track("v2", TrackKind::Overlay, 1);
    hidden.enabled = false;

    let p = project(
        vec![media("m1", "/a.mp4", false), media("m2", "/b.mp4", false)],
        vec![track("v1", TrackKind::Video, 0), hidden],
        vec![
            item("i0", "v1", "m1", 0, 0, 2000),
            item("i1", "v2", "m2", 0, 0, 2000),
        ],
    );
    let g = fc(&build_filter_complex(
        &p,
        &ComposeSettings::default(),
        None,
        "out.mp4",
    ));

    assert!(
        !g.contains("[pv1]"),
        "disabled overlay track's clip was composited into the export: {g}"
    );
}

/// The M button: an audio track with `muted = true` is silent in the preview,
/// so its clips must NOT be mixed into the export.
#[test]
fn muted_audio_track_is_excluded_from_export_mix() {
    let mut muted_track = track("a1", TrackKind::Audio, 1);
    muted_track.muted = true;

    let p = project(
        vec![media("m1", "/a.mp4", true), media("m2", "/nar.mp4", true)],
        vec![track("v1", TrackKind::Video, 0), muted_track],
        vec![
            item("i0", "v1", "m1", 0, 0, 2000),
            // Scratch narration on the MUTED audio track.
            item("i1", "a1", "m2", 0, 0, 2000),
        ],
    );
    let g = fc(&build_filter_complex(
        &p,
        &ComposeSettings::default(),
        None,
        "out.mp4",
    ));

    assert!(
        !g.contains("[pa1]"),
        "clip on muted audio track a1 was mixed into the export audio: {g}"
    );
}

/// The S button: soloing one track must silence every OTHER track in the
/// export (DAW convention) — with a single audible item there is no amix.
#[test]
fn soloed_track_silences_other_tracks_in_export() {
    let mut soloed = track("a-solo", TrackKind::Audio, 1);
    soloed.solo = true;

    let p = project(
        vec![
            media("m1", "/other.mp4", true),
            media("m2", "/solo.mp4", true),
        ],
        vec![track("v1", TrackKind::Video, 0), soloed],
        vec![
            // Item on the NON-soloed track — must be silenced by the solo.
            item("i0", "v1", "m1", 0, 0, 2000),
            item("i1", "a-solo", "m2", 0, 0, 2000),
        ],
    );
    let g = fc(&build_filter_complex(
        &p,
        &ComposeSettings::default(),
        None,
        "out.mp4",
    ));

    assert!(
        !g.contains("amix=inputs=2"),
        "solo on a-solo silenced nothing: non-soloed track still in the mix: {g}"
    );
}

/// Solo with two AUDIO tracks: only the soloed track's clip may be processed
/// for audio at all. `[pa{n}]` labels stay tied to the item's position in the
/// full audio-bearing list, so the excluded bed's `[pa0]` must be absent.
#[test]
fn solo_excludes_non_soloed_audio_track_from_export() {
    let mut soloed = track("a-vox", TrackKind::Audio, 1);
    soloed.solo = true;

    let p = project(
        vec![media("m1", "/bed.mp4", true), media("m2", "/vox.mp4", true)],
        vec![
            // Non-soloed audio track — must fall silent while a-vox is soloed.
            track("a-bed", TrackKind::Audio, 0),
            soloed,
        ],
        vec![
            item("i0", "a-bed", "m1", 0, 0, 3000),
            item("i1", "a-vox", "m2", 0, 0, 3000),
        ],
    );
    let g = fc(&build_filter_complex(
        &p,
        &ComposeSettings::default(),
        None,
        "out.mp4",
    ));

    assert!(
        !g.contains("[pa0]"),
        "non-soloed track's audio survived a solo in the export mix: {g}"
    );
}

/// The burn-in fast path renders the primary video with full audio
/// passthrough and no track-flag awareness — so any non-default flag on the
/// pristine clip's track must route the export through the composite path,
/// where the flags are applied.
#[test]
fn track_flags_on_the_pristine_clip_leave_the_simple_path() {
    // The backfilled import shape: primary video placed as one pristine clip.
    fn baseline() -> Project {
        let mut p = project(
            vec![media("m1", "/x.mp4", true)], // matches video_path + hash
            vec![
                track("v1", TrackKind::Video, 0),
                track("c1", TrackKind::Caption, 1),
            ],
            vec![item("i0", "v1", "m1", 0, 0, 60_000)],
        );
        p.video_path = "/x.mp4".into();
        p
    }
    assert!(is_simple_timeline(&baseline()), "default flags stay simple");

    let mut p = baseline();
    p.tracks[0].enabled = false;
    assert!(!is_simple_timeline(&p), "disabled track must composite");

    let mut p = baseline();
    p.tracks[0].muted = true;
    assert!(!is_simple_timeline(&p), "muted track must composite");

    let mut p = baseline();
    p.tracks[1].solo = true; // solo elsewhere silences the primary clip
    assert!(!is_simple_timeline(&p), "active solo must composite");
}
