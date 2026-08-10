//! Contract guard (E4a): every path that burns captions into pixels must take
//! its `.ass` sidecar from `export::write_ass`, so karaoke is identical in the
//! preview proxy and in the final export.
//!
//! SundayEdit promises "what gets exported matches what the user saw in
//! preview". Karaoke makes that promise expensive to keep: the same per-word
//! ladder has to come out of four different render entry points —
//!
//!   - `burnin::render`                  (single-file burn-in / clip export)
//!   - `compose::run_simple_compose`     (the DEFAULT path of every fresh import)
//!   - `compose::run_compose`            (the filter_complex composite)
//!   - `compose::run_compose_proxy`      (the low-res preview-render fallback)
//!
//! — and if any one of them built its own ASS, or read a karaoke setting from
//! somewhere other than the project, the preview and the delivered file would
//! quietly disagree. The settings therefore live on `ExportConfig` (persisted
//! with the project) and `write_ass` reads them itself, so no caller can
//! forget to pass them.
//!
//! Two guards below: a behavioural premise (the persisted setting really does
//! change the bytes `write_ass` returns) and a source tripwire (no render path
//! hand-rolls a sidecar).

use sundayedit_lib::model::{Caption, ExportConfig, Project, ProjectMeta, Style, Word};
use sundayedit_lib::services::export::write_ass;
use sundayedit_lib::services::karaoke::{KaraokeOptions, KaraokeStyle};

fn project(karaoke: Option<KaraokeOptions>) -> Project {
    Project {
        id: "p".into(),
        name: "sermon.mp4".into(),
        video_path: "/sermon.mp4".into(),
        video_content_hash: "hash".into(),
        video_duration_ms: 10_000,
        video_width: 1920,
        video_height: 1080,
        video_fps: 30.0,
        audio_wav_path: None,
        language: "no".into(),
        default_style: Style::broadcast_news(),
        context_description: None,
        captions: vec![Caption {
            id: "c1".into(),
            start_ms: 1_000,
            end_ms: 4_000,
            words: vec![
                Word::new("Nåde", 1_000, 1_800, 96.0),
                Word::new("og", 1_900, 2_300, 91.0),
                Word::new("fred", 2_400, 4_000, 44.0),
            ],
            speaker_id: None,
            style_id: None,
            notes: None,
            ai_generated: true,
            last_edited_at: 0,
            track_id: None,
        }],
        speakers: vec![],
        glossary: vec![],
        clips: vec![],
        talk_summary: None,
        export_config: ExportConfig {
            karaoke,
            ..ExportConfig::default()
        },
        project_meta: ProjectMeta::default(),
        created_at: 0,
        updated_at: 0,
        media: vec![],
        tracks: vec![],
        timeline_items: vec![],
    }
}

/// PREMISE: the karaoke setting is carried by the PROJECT, not by a per-call
/// argument — so the one `write_ass(project)` call each render path already
/// makes is enough to get karaoke right everywhere.
#[test]
fn premise_write_ass_reads_karaoke_from_the_project() {
    let off = write_ass(&project(None));
    assert!(
        !off.contains("{\\k"),
        "a project without karaoke settings must render plain captions"
    );

    let on = write_ass(&project(Some(KaraokeOptions {
        enabled: true,
        style: KaraokeStyle::Highlight,
        ..Default::default()
    })));
    assert!(
        on.contains("{\\k"),
        "the persisted setting must reach the ASS writer with no extra plumbing"
    );

    // The ladder closes on the Dialogue span: 1.00 s → 4.00 s is 300 cs.
    let text = on
        .lines()
        .find(|l| l.starts_with("Dialogue:"))
        .and_then(|l| l.splitn(10, ',').nth(9).map(str::to_string))
        .expect("a Dialogue line");
    let sum: i64 = text
        .split("{\\k")
        .skip(1)
        .map(|c| c.split('}').next().unwrap().parse::<i64>().unwrap())
        .sum();
    assert_eq!(
        sum, 300,
        "\\k durations are cumulative — they must close exactly"
    );
}

/// TRIPWIRE: no render path may hand-roll its caption sidecar.
///
/// Each of the functions below writes an `.ass` file next to its output. If one
/// ever stops sourcing that string from `export::write_ass` / `write_clip_ass`
/// — building the Dialogue lines inline, or caching a sidecar produced before
/// the karaoke settings changed — the preview proxy and the final export drift
/// apart, which is exactly the failure the shared timing module exists to
/// prevent. Silent, too: both files still play.
#[test]
fn every_burn_in_path_sources_its_sidecar_from_the_shared_writer() {
    for (file, fns) in [
        (
            "/src/services/burnin.rs",
            &["fn run_burnin(", "pub fn render(", "pub fn render_clip("][..],
        ),
        (
            "/src/services/compose.rs",
            &[
                "fn run_simple_compose(",
                "pub fn run_compose(",
                "pub fn run_compose_proxy(",
            ][..],
        ),
    ] {
        let src = std::fs::read_to_string(format!("{}{file}", env!("CARGO_MANIFEST_DIR")))
            .unwrap_or_else(|e| panic!("read {file}: {e}"));

        for f in fns {
            let start = src
                .find(f)
                .unwrap_or_else(|| panic!("{f} still exists in {file}"));
            // Body = up to the next top-level `\n}` after the signature.
            let rest = &src[start..];
            let end = rest.find("\n}").map(|i| i + 2).unwrap_or(rest.len());
            let body = &rest[..end];

            // A function that writes an .ass sidecar must get it from the
            // shared writer; a function that only delegates is fine.
            if !body.contains(".ass") && !body.contains("ass_path") {
                continue;
            }
            let sources_shared = body.contains("export::write_ass")
                || body.contains("export::write_clip_ass")
                // `run_burnin` is the shared tail — it receives the already
                // generated ASS from its callers, which ARE checked above.
                || body.contains("ass: String");
            assert!(
                sources_shared,
                "{f} in {file} builds an ASS sidecar without going through \
                 export::write_ass — karaoke (and every future caption \
                 feature) would then differ between this path and the others.\n\
                 Body:\n{body}"
            );
        }
    }
}
