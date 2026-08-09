//! Contract guard: the simple-timeline fast path in `run_compose` must keep
//! progress + cancellation.
//!
//! `run_compose` shortcuts simple timelines (the exact shape
//! `Project::backfill_default_timeline` synthesizes on every fresh import)
//! into the burn-in argument builder for hardware encoding + audio
//! passthrough. That is the DEFAULT export path — `ExportPanel.tsx` mounts
//! `ComposeExport` whenever `timeline_items.length > 0` — so it must stream
//! `compose-render-progress` and poll the shared cancel flag exactly like the
//! composite path (`run_simple_compose` does; a bare `burnin::render`
//! delegate did neither: 0%-forever bar, Cancel button that flipped a bool
//! nobody read — diff-simple-path-no-progress-no-cancel).
//!
//! A behavioural test through `run_compose` is not constructible here: its
//! `window: &tauri::Window` is the concrete `Window<Wry>`, a Wry event loop
//! cannot be created off the main thread, and `tauri::test` MockRuntime
//! windows are a different type. The contract is pinned at the two seams that
//! ARE reachable: a premise test proving the fresh-import shape routes into
//! the fast path, and a source-contract tripwire asserting the branch
//! forwards both `cancel` and `window` into whatever it delegates to.

use sundayedit_lib::model::{ExportConfig, Project, ProjectMeta, Style};
use sundayedit_lib::services::compose::is_simple_timeline;

/// PREMISE: a freshly imported project — scalar `video_*` fields set, NLE
/// arrays backfilled by `Project::backfill_default_timeline` — satisfies
/// `is_simple_timeline`, i.e. every new project's very first compose export
/// takes the fast path. This is why that path must carry progress + cancel.
#[test]
fn premise_fresh_import_default_shape_routes_into_the_fast_path() {
    let mut p = Project {
        id: "p".into(),
        name: "movie.mp4".into(),
        video_path: "/movie.mp4".into(),
        video_content_hash: "hash".into(),
        video_duration_ms: 3_600_000, // a long file — where cancel matters
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
        export_config: ExportConfig::default(),
        project_meta: ProjectMeta::default(),
        created_at: 0,
        updated_at: 0,
        media: vec![],
        tracks: vec![],
        timeline_items: vec![],
    };
    p.backfill_default_timeline(true);

    assert!(
        !p.timeline_items.is_empty(),
        "backfill places the primary clip, so ExportPanel mounts ComposeExport \
         (timeline_items.length > 0)"
    );
    assert!(
        is_simple_timeline(&p),
        "the backfilled import shape is the simple timeline — run_compose \
         takes the fast path for it"
    );
}

/// TRIPWIRE: the simple-timeline branch inside `run_compose` must forward
/// BOTH the cancel flag and the progress window to the render it delegates
/// to. Delegating to a render that takes neither (e.g. `burnin::render`,
/// which blocks on `Command::status()`) silently regresses the default
/// export path to un-cancellable and progress-less.
#[test]
fn simple_path_forwards_cancel_and_progress_to_its_render() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/services/compose.rs"
    ))
    .expect("read compose.rs");

    // Isolate run_compose's body.
    let start = src
        .find("pub fn run_compose(")
        .expect("run_compose exists in compose.rs");
    let body = &src[start..];
    let end = body.find("pub fn run_compose_proxy").unwrap_or(body.len());
    let body = &body[..end];

    // If the shortcut was removed outright, the bug is gone by construction.
    let Some(branch_start) = body.find("if is_simple_timeline") else {
        return;
    };
    // The fast-path branch: from the `if` to its 4-space-indented closing
    // brace (the branch body is indented deeper).
    let branch = &body[branch_start..];
    let branch_end = branch
        .find("\n    }")
        .map(|i| i + 1)
        .unwrap_or(branch.len());
    let branch = &branch[..branch_end];

    assert!(
        branch.contains("cancel"),
        "run_compose's simple-timeline fast path never passes the `cancel` \
         AtomicBool to the render it delegates to — compose_cancel becomes a \
         no-op on the DEFAULT export path of every freshly imported project.\n\
         Offending branch:\n{branch}"
    );
    assert!(
        branch.contains("window"),
        "run_compose's simple-timeline fast path never receives the `window`, \
         so no compose-render-progress events are emitted — the progress modal \
         sits at 0% for the whole render.\n\
         Offending branch:\n{branch}"
    );
}
