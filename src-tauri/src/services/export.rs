//! Subtitle export writers (Phase 6.1).
//!
//! Pure functions: input a Project, output a string in the requested
//! format. The Tauri command layer is responsible for writing the
//! string to disk.
//!
//! Format priority:
//!   - SRT — universal, simple, no styling
//!   - VTT — web standard, slightly richer
//!   - ASS — full styling (used by Aegisub, libass; what Phase 6.2 burn-in
//!     uses, and — since R5-C — the one layer that also renders `Text`
//!     timeline items)
//!   - TXT — plain transcript, no timestamps
//!
//! All formats validated against their respective parsers' real-world
//! quirks (UTF-8 + appropriate line endings; SRT and VTT 0-padded;
//! ASS-escaped `{}` in text).

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::model::{
    Caption, Clip, Project, Speaker, Style, TimelineItem, TimelineItemKind, Transform,
};
use crate::services::karaoke::{karaoke_words, uncertain_flags, KaraokeOptions};

// ── SRT ─────────────────────────────────────────────────────────────────────

/// Generate SRT (.srt) content from a project.
///
/// SRT is the universal lowest common denominator. No styling, no
/// speakers (we surface them as a "Speaker:" prefix when more than one
/// speaker exists — and only if the caller asks for it).
pub fn write_srt(project: &Project, opts: SrtOptions) -> String {
    let mut out = String::with_capacity(project.captions.len() * 80);
    let speakers_map = speakers_by_id(&project.speakers);
    let mut idx = 1u32;
    for c in &project.captions {
        if opts.strip_empty && c.words.is_empty() {
            continue;
        }
        let text = sanitize_cue_text(&if opts.include_speakers {
            format_with_speaker(c, &speakers_map)
        } else {
            c.text()
        });

        // 1-based index, then "HH:MM:SS,mmm --> HH:MM:SS,mmm", then text
        out.push_str(&idx.to_string());
        out.push_str("\r\n");
        out.push_str(&fmt_srt_time(c.start_ms));
        out.push_str(" --> ");
        out.push_str(&fmt_srt_time(c.end_ms));
        out.push_str("\r\n");
        out.push_str(&text);
        out.push_str("\r\n\r\n");
        idx += 1;
    }
    out
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SrtOptions {
    pub include_speakers: bool,
    pub strip_empty: bool,
}

fn fmt_srt_time(ms: i64) -> String {
    let neg = ms < 0;
    let ms = ms.unsigned_abs();
    let hours = ms / 3_600_000;
    let minutes = (ms / 60_000) % 60;
    let seconds = (ms / 1_000) % 60;
    let millis = ms % 1_000;
    let sign = if neg { "-" } else { "" };
    format!(
        "{}{:02}:{:02}:{:02},{:03}",
        sign, hours, minutes, seconds, millis
    )
}

// ── VTT ─────────────────────────────────────────────────────────────────────

/// Generate WebVTT (.vtt) content.
///
/// Same shape as SRT but `.` instead of `,` for milliseconds, and a
/// `WEBVTT` header. Speakers (when present) are encoded as
/// `<v Speaker Name>text</v>` voice spans, which most web players honour.
pub fn write_vtt(project: &Project, opts: VttOptions) -> String {
    let mut out = String::with_capacity(project.captions.len() * 80 + 16);
    out.push_str("WEBVTT\n\n");
    let speakers_map = speakers_by_id(&project.speakers);
    let mut idx = 1u32;
    for c in &project.captions {
        if opts.strip_empty && c.words.is_empty() {
            continue;
        }
        // Cue id is optional in VTT; a contiguous 1-based counter (incremented
        // only on emit, like the SRT writer) is a nice debugging aid — using the
        // raw enumerate index would leave gaps where `strip_empty` dropped cues.
        out.push_str(&format!("{idx}\n"));
        idx += 1;
        out.push_str(&fmt_vtt_time(c.start_ms));
        out.push_str(" --> ");
        out.push_str(&fmt_vtt_time(c.end_ms));
        out.push('\n');
        if opts.include_speakers {
            if let Some(speaker_id) = &c.speaker_id {
                if let Some(name) = speakers_map.get(speaker_id) {
                    out.push_str(&format!(
                        "<v {}>{}</v>\n",
                        vtt_escape(&sanitize_cue_text(name)),
                        vtt_escape(&sanitize_cue_text(&c.text()))
                    ));
                    out.push('\n');
                    continue;
                }
            }
        }
        out.push_str(&vtt_escape(&sanitize_cue_text(&c.text())));
        out.push_str("\n\n");
    }
    out
}

#[derive(Debug, Clone, Copy, Default)]
pub struct VttOptions {
    pub include_speakers: bool,
    pub strip_empty: bool,
}

fn fmt_vtt_time(ms: i64) -> String {
    let ms = ms.max(0) as u64;
    let hours = ms / 3_600_000;
    let minutes = (ms / 60_000) % 60;
    let seconds = (ms / 1_000) % 60;
    let millis = ms % 1_000;
    format!("{:02}:{:02}:{:02}.{:03}", hours, minutes, seconds, millis)
}

/// Neutralise characters in caption/speaker text that would break SRT/VTT cue
/// framing. Both formats delimit cues with a BLANK LINE, so an embedded blank
/// line (from a multi-line word, e.g. a pasted find/replace value) truncates
/// the cue and desynchronises every following entry. Carriage returns are also
/// dropped so a stray '\r' can't forge an early line end. A single line break
/// inside a cue is legal (multi-line caption) and is preserved as a lone '\n';
/// runs of blank lines collapse to one break.
fn sanitize_cue_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_newline = false;
    for ch in s.chars() {
        match ch {
            '\r' => {} // drop CRs; never a meaningful in-cue character
            '\n' => {
                if !last_was_newline {
                    out.push('\n');
                    last_was_newline = true;
                }
                // collapse consecutive newlines (the blank line that breaks cues)
            }
            _ => {
                out.push(ch);
                last_was_newline = false;
            }
        }
    }
    out
}

fn vtt_escape(s: &str) -> String {
    // Minimal escaping for VTT — angle brackets and ampersand.
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ── ASS / SSA ───────────────────────────────────────────────────────────────

/// Generate Advanced SubStation Alpha (.ass) content.
///
/// Full styling preserved. This is the format that `libass` consumes for
/// burn-in (Phase 6.2). The default `Style` from the project becomes
/// `Style: Default` in the output; any style id a caption or a text overlay
/// REFERENCES becomes an additional named block (see [`named_styles`]).
///
/// Two kinds of event, in this order:
///   - one `Dialogue:` on layer 0 per caption (the flagship path, karaoke and
///     all);
///   - one `Dialogue:` on layer 1 per rendered `TimelineItemKind::Text`
///     overlay (R5-C), positioned from its `Transform` — see
///     [`ass_text_items`] and [`text_override_tags`]. This is also why the
///     sidecar EXPORT command now hands the user a file containing their
///     overlays: the .ass is the project's burn-in truth, not a
///     captions-only artefact.
///
/// Karaoke (E4a) is read from `project.export_config.karaoke`, so EVERY caller
/// — the sidecar export command, `burnin::render`, and all three `compose`
/// paths (simple, filter_complex, preview proxy) — gets the same answer
/// without threading an extra argument. Use [`write_ass_with`] to override.
pub fn write_ass(project: &Project) -> String {
    write_ass_with(project, &project_karaoke(project))
}

/// The karaoke options a project renders with. `None` (pre-E4a files) means
/// OFF, which is byte-for-byte the pre-E4a output.
pub fn project_karaoke(project: &Project) -> KaraokeOptions {
    project
        .export_config
        .karaoke
        .clone()
        .unwrap_or_else(KaraokeOptions::disabled)
}

/// [`write_ass`] with explicit karaoke options — used by the export command so
/// the UI can preview a karaoke toggle without mutating the project first.
///
/// With `karaoke.enabled == false` the output is byte-identical to the
/// pre-E4a writer (pinned by `ass_output_is_byte_identical_when_karaoke_off`).
pub fn write_ass_with(project: &Project, karaoke: &KaraokeOptions) -> String {
    let mut out = String::with_capacity(project.captions.len() * 100 + 1024);

    // ── [Script Info] ──
    out.push_str("[Script Info]\n");
    out.push_str(&format!("Title: {}\n", ass_escape(&project.name)));
    out.push_str("ScriptType: v4.00+\n");
    out.push_str(&format!("PlayResX: {}\n", project.video_width));
    out.push_str(&format!("PlayResY: {}\n", project.video_height));
    out.push_str("WrapStyle: 0\n");
    out.push_str("ScaledBorderAndShadow: yes\n");
    out.push_str("YCbCr Matrix: TV.709\n");
    out.push('\n');

    // ── [V4+ Styles] ──
    out.push_str("[V4+ Styles]\n");
    out.push_str("Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
    out.push_str(&format_ass_style_with(
        "Default",
        &project.default_style,
        karaoke_secondary(karaoke).as_deref(),
    ));
    // One named block per style id a caption or a text overlay actually
    // REFERENCES (R5-C; closes the standing "Phase 5.2 wires per-caption style
    // names" note). Keyed by id in a BTreeMap, so the emitted order is a
    // function of the project alone — a writer whose output moved with hash
    // iteration order could never be pinned byte-for-byte.
    let named = named_styles(project);
    for (name, style) in named.values() {
        // The karaoke `SecondaryColour` (the not-yet-sung colour) belongs on
        // EVERY block a caption can be rendered with, not just `Default` — a
        // caption that picked a named style would otherwise sweep against the
        // inert pre-E4a placeholder.
        out.push_str(&format_ass_style_with(
            name,
            style,
            karaoke_secondary(karaoke).as_deref(),
        ));
    }
    out.push('\n');

    // The Style field of one Dialogue line: the referenced block's name, or
    // `Default` for no reference / an id nothing resolves (a hand-edited file).
    // Naming a block libass does not carry would make it fall back silently;
    // resolving here means the writer only ever emits names it also defined.
    let style_field = |id: Option<&str>| -> &str {
        id.and_then(|i| named.get(i))
            .map(|(n, _)| n.as_str())
            .unwrap_or("Default")
    };

    // ── [Events] ──
    out.push_str("[Events]\n");
    out.push_str(
        "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
    );
    let speakers_map = speakers_by_id(&project.speakers);
    for c in &project.captions {
        let name_field = c
            .speaker_id
            .as_deref()
            .and_then(|id| speakers_map.get(id).map(|n| n.as_str()))
            .unwrap_or("");
        out.push_str(&format!(
            "Dialogue: 0,{},{},{},{},0,0,0,,{}\n",
            fmt_ass_time(c.start_ms),
            fmt_ass_time(c.end_ms),
            style_field(c.style_id.as_deref()),
            ass_field(name_field), // Name is comma-delimited — must not contain a raw comma
            // Text is the trailing field — commas stay literal.
            ass_dialogue_text(c, karaoke, &project.default_style),
        ));
    }

    // ── Text timeline items (R5-C) ──────────────────────────────────────────
    // A `TimelineItemKind::Text` overlay is rendered by the SAME libass layer
    // the captions ride on — no `drawtext` node, no second font/escaping
    // regime, and (because `compose.rs` applies `ass=` LAST on both the
    // composite and the burn-in path) export/preview parity by construction.
    //
    // Layer 1, so an overlay sits ABOVE the caption band the way
    // `write_clip_ass` already stacks its `Title` line. Times come straight
    // from the timeline (`timeline_end_ms` honours `speed`, so a text item
    // ends exactly where its lane does).
    for it in ass_text_items(project) {
        let Some(spec) = it.text.as_ref() else {
            continue; // unreachable: `ass_text_items` only yields items with a spec
        };
        out.push_str(&format!(
            "Dialogue: 1,{},{},{},,0,0,0,,{}{}\n",
            fmt_ass_time(it.timeline_start_ms),
            fmt_ass_time(it.timeline_end_ms()),
            style_field(spec.style_id.as_deref()),
            text_override_tags(&it.transform, project.video_width, project.video_height),
            ass_escape(&spec.text),
        ));
    }

    out
}

// ── Text timeline items → ASS Dialogue (R5-C) ───────────────────────────────

/// The `Text` timeline items [`write_ass`] renders, in project order.
///
/// The SINGLE definition of "this overlay reaches the render", shared with
/// [`ass_has_events`] and (through it) with `compose`'s decision whether to
/// hang an `ass=` node on the graph at all. Three conditions, each mirroring
/// something the rest of the pipeline already believes:
///
///   - `enabled` + a visible track — the same pair `compose::visual_stack`
///     applies to picture clips, so an overlay hidden in the preview is absent
///     from the render too;
///   - a `TextSpec` with non-empty text — an empty overlay has no ink to lose,
///     and emitting a Dialogue for it would make `ass_has_events` claim the
///     sidecar draws something it does not.
pub fn ass_text_items(project: &Project) -> Vec<&TimelineItem> {
    project
        .timeline_items
        .iter()
        .filter(|it| {
            it.kind == TimelineItemKind::Text
                && it.enabled
                && crate::services::compose::track_visible(project, it)
                && it
                    .text
                    .as_ref()
                    .is_some_and(|spec| !spec.text.trim().is_empty())
        })
        .collect()
}

/// Does [`write_ass`] emit ANY `Dialogue:` line for this project?
///
/// `run_compose` / `run_compose_proxy` used to ask `project.captions.is_empty()`
/// before attaching the `ass=` node — a private re-derivation of "the sidecar
/// is worth applying" that went stale the moment the writer learned to emit
/// text overlays (the sidecar would have carried the text and the graph would
/// have thrown it away). Asking the WRITER's own module keeps the two sides
/// from drifting; `ass_predicate_agrees_with_the_writer` pins that they agree.
pub fn ass_has_events(project: &Project) -> bool {
    !project.captions.is_empty() || !ass_text_items(project).is_empty()
}

/// The inline override block that positions ONE text overlay.
///
/// `Transform` is fractions of the OUTPUT FRAME, and the ASS header writes
/// `PlayResX/Y` from the project dimensions, so `round(PlayRes * fraction)` is
/// the exact arithmetic `build_filter_complex` uses for a picture clip
/// (`overlay=<W*x>:<H*y>`) and `compositor/scene.ts` mirrors in the preview.
/// libass scales PlayRes space to whatever the output resolution turns out to
/// be, so the placement is resolution-independent for free.
///
/// `\an7` is not decoration: it pins the text's TOP-LEFT to `\pos`, which is
/// what `overlay`'s x/y means. Without it the anchor would be the style's own
/// Alignment (bottom-centre by default) and the same fractions would land the
/// text somewhere else entirely.
///
/// `Transform::crop` is deliberately not mapped: there is no source frame to
/// cut out of generated text. Everything else the transform can say IS mapped,
/// so a value stored by the inspector cannot be silently ignored.
fn text_override_tags(t: &Transform, play_res_w: i32, play_res_h: i32) -> String {
    let x = (play_res_w as f32 * t.x).round() as i64;
    let y = (play_res_h as f32 * t.y).round() as i64;
    let mut tags = format!("{{\\an7\\pos({x},{y})");
    // Identity values emit nothing — an untouched overlay gets the shortest
    // block, and the golden-file tests stay readable.
    if (t.scale - 1.0).abs() > f32::EPSILON && t.scale > 0.0 {
        let pct = fmt_ass_num(t.scale * 100.0);
        tags.push_str(&format!("\\fscx{pct}\\fscy{pct}"));
    }
    if t.rotation_deg.abs() > f32::EPSILON {
        // ASS `\frz` is COUNTER-clockwise-positive; the export's `rotate=` (and
        // the Pixi preview's `rotationRad`) are clockwise-positive. Negate, or
        // a text overlay would spin the opposite way from a picture clip
        // carrying the same number.
        tags.push_str(&format!("\\frz{}", fmt_ass_num(-t.rotation_deg)));
    }
    if t.opacity < 1.0 {
        // `\alpha` is TRANSPARENCY: 00 = opaque, FF = invisible.
        let a = ((1.0 - t.opacity.clamp(0.0, 1.0)) * 255.0).round() as u8;
        tags.push_str(&format!("\\alpha&H{a:02X}&"));
    }
    tags.push('}');
    tags
}

/// Format a number for an ASS override tag: up to 3 decimals, trailing zeros
/// trimmed. `40.0` → `40`, `37.5` → `37.5` — f32 arithmetic otherwise leaks
/// `40.000004` into the file.
fn fmt_ass_num(v: f32) -> String {
    let s = format!("{v:.3}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    match trimmed {
        "" | "-" | "-0" => "0".to_string(),
        other => other.to_string(),
    }
}

/// Every style id a caption or a rendered text overlay references AND that
/// resolves to a real [`Style`], as `id → (ASS style name, style)`.
///
/// Resolution sources, in order: the project's own `default_style` (which is
/// already emitted as `Default`, so it yields no extra block) and the bundled
/// `style_presets::catalog()`. An id from neither resolves to nothing and the
/// Dialogue falls back to `Default` — the file never names a style it lacks.
fn named_styles(project: &Project) -> std::collections::BTreeMap<String, (String, Style)> {
    let mut ids: std::collections::BTreeSet<&str> = project
        .captions
        .iter()
        .filter_map(|c| c.style_id.as_deref())
        .collect();
    for it in ass_text_items(project) {
        if let Some(id) = it.text.as_ref().and_then(|s| s.style_id.as_deref()) {
            ids.insert(id);
        }
    }
    if ids.is_empty() {
        return Default::default();
    }
    let catalog = crate::services::style_presets::catalog();
    ids.into_iter()
        .filter(|id| *id != project.default_style.id)
        .filter_map(|id| {
            catalog
                .iter()
                .find(|p| p.style.id == id)
                .map(|p| (id.to_string(), (ass_style_name(id), p.style.clone())))
        })
        .collect()
}

/// A style id as an ASS `[V4+ Styles]` name: non-alphanumerics become `_`.
///
/// The `Name` field is comma-delimited and read back by libass as a key, so a
/// raw `preset:tiktok_bold` (or any id a future feature invents) has no
/// business going in verbatim. Every id the catalog carries starts with
/// `preset:`, so no generated name can collide with the literal `Default`.
fn ass_style_name(style_id: &str) -> String {
    let name: String = style_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if name.is_empty() {
        "Default".to_string()
    } else {
        name
    }
}

// ── Karaoke text (E4a) ──────────────────────────────────────────────────────

/// The Dialogue `Text` field for one caption: plain escaped text when karaoke
/// is off, otherwise the `{\kNN}word ` run sequence.
///
/// All timing comes from `services::karaoke` — the SHARED source the frontend
/// canvas overlay also renders from. Nothing here re-derives a duration.
fn ass_dialogue_text(c: &Caption, karaoke: &KaraokeOptions, style: &Style) -> String {
    if !karaoke.enabled {
        return ass_escape(&c.text());
    }

    let words = karaoke_words(c);
    let tag = karaoke.style.tag();
    // When tinting, EVERY word carries an explicit `\c` — an inline colour
    // override persists for the rest of the line, so one tinted word would
    // bleed into all the words after it unless each run restates its colour.
    // Nothing is computed at all when the option is off.
    let tint = karaoke.confidence_tint.then(|| {
        (
            uncertain_flags(c, karaoke.confidence_threshold),
            hex_to_ass_inline_color(&style.color_fg),
            hex_to_ass_inline_color(&karaoke.low_confidence_color),
        )
    });

    let last = words.len().saturating_sub(1);
    let mut text = String::with_capacity(words.len() * 16);
    for (i, w) in words.iter().enumerate() {
        text.push_str("{\\");
        text.push_str(tag);
        text.push_str(&w.duration_cs.to_string());
        if let Some((flags, normal_c, low_c)) = &tint {
            text.push_str("\\c");
            text.push_str(if flags.get(i).copied().unwrap_or(false) {
                low_c
            } else {
                normal_c
            });
        }
        text.push('}');
        text.push_str(&ass_escape(&w.text));
        // The separating space belongs to the PRECEDING run: putting it at the
        // start of the next run would highlight the space together with the
        // word that follows, which reads as a one-character early trigger.
        if i != last {
            text.push(' ');
        }
    }
    text
}

/// The ASS `SecondaryColour` to use for this karaoke config, or `None` to keep
/// the historical hard-coded value (karaoke off → byte-identical output).
///
/// `\k`/`\kf` fill PrimaryColour OVER SecondaryColour, so SecondaryColour is
/// literally "the not-yet-sung colour". The pre-E4a value was an inert
/// placeholder (red) that nothing rendered; it only becomes visible once
/// karaoke is on, so it must not change while karaoke is off.
fn karaoke_secondary(karaoke: &KaraokeOptions) -> Option<String> {
    karaoke
        .enabled
        .then(|| hex_to_ass_bgr(&karaoke.pending_color))
}

/// Generate ASS for a single social clip (SundayEdit), for burn-in into a
/// vertical export. Two differences from `write_ass`:
///   1. Caption timings are offset to clip-relative 0, because ffmpeg `-ss`
///      trims the input so the rendered clip's timeline starts at 0.
///   2. A second `Title` style renders the clip's main-point overlay, on a
///      higher layer, spanning the whole clip.
///
/// `play_res_w/h` are the OUTPUT (vertical) dimensions so libass sizes the
/// title for the target frame.
///
/// Karaoke follows the project's `export_config` exactly like [`write_ass`], so
/// a vertical social clip is highlighted the same way the full render is. Note
/// the timings are offset to clip-relative 0 BEFORE the karaoke ladder is built
/// (a shifted clone of the caption), so the `\k` steps close on the clipped
/// Dialogue span rather than the original one.
pub fn write_clip_ass(
    project: &Project,
    clip: &Clip,
    title_style: &Style,
    play_res_w: i32,
    play_res_h: i32,
) -> String {
    let karaoke = project_karaoke(project);
    let clip_dur = (clip.end_ms - clip.start_ms).max(1);
    let mut out = String::with_capacity(512);

    out.push_str("[Script Info]\n");
    out.push_str(&format!("Title: {}\n", ass_escape(&clip.title)));
    out.push_str("ScriptType: v4.00+\n");
    out.push_str(&format!("PlayResX: {play_res_w}\n"));
    out.push_str(&format!("PlayResY: {play_res_h}\n"));
    out.push_str("WrapStyle: 0\n");
    out.push_str("ScaledBorderAndShadow: yes\n");
    out.push_str("YCbCr Matrix: TV.709\n");
    out.push('\n');

    out.push_str("[V4+ Styles]\n");
    out.push_str("Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
    out.push_str(&format_ass_style_with(
        "Default",
        &project.default_style,
        karaoke_secondary(&karaoke).as_deref(),
    ));
    // The title overlay is never karaoke'd — it keeps the untouched style line.
    out.push_str(&format_ass_style("Title", title_style));
    out.push('\n');

    out.push_str("[Events]\n");
    out.push_str(
        "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
    );

    // Caption events overlapping the clip, offset to clip-relative time.
    for c in &project.captions {
        if c.end_ms <= clip.start_ms || c.start_ms >= clip.end_ms {
            continue;
        }
        let start = (c.start_ms - clip.start_ms).max(0);
        let end = (c.end_ms - clip.start_ms).min(clip_dur);
        if end <= start {
            continue;
        }
        // Shift the caption (and its words) into clip-relative time so the
        // karaoke ladder is derived from the SAME numbers libass will see.
        let shifted = shift_caption(c, clip.start_ms, start, end);
        out.push_str(&format!(
            "Dialogue: 0,{},{},Default,,0,0,0,,{}\n",
            fmt_ass_time(start),
            fmt_ass_time(end),
            ass_dialogue_text(&shifted, &karaoke, &project.default_style),
        ));
    }

    // The clip's main point as a title overlay spanning the whole clip.
    if !clip.title.trim().is_empty() {
        out.push_str(&format!(
            "Dialogue: 1,{},{},Title,,0,0,0,,{}\n",
            fmt_ass_time(0),
            fmt_ass_time(clip_dur),
            ass_escape(&clip.title),
        ));
    }

    out
}

/// Clone a caption with every timestamp moved into clip-relative time and the
/// caption bounds pinned to the already-clamped `[start, end]` the Dialogue
/// line uses. Words keep their relative positions; the karaoke derivation then
/// clamps any that fall outside.
fn shift_caption(c: &Caption, offset_ms: i64, start: i64, end: i64) -> Caption {
    let mut shifted = c.clone();
    shifted.start_ms = start;
    shifted.end_ms = end;
    for w in &mut shifted.words {
        w.start_ms -= offset_ms;
        w.end_ms -= offset_ms;
    }
    shifted
}

fn fmt_ass_time(ms: i64) -> String {
    let ms = ms.max(0) as u64;
    let hours = ms / 3_600_000;
    let minutes = (ms / 60_000) % 60;
    let seconds = (ms / 1_000) % 60;
    // ASS uses centiseconds (hundredths), not milliseconds.
    let centis = (ms % 1_000) / 10;
    format!("{}:{:02}:{:02}.{:02}", hours, minutes, seconds, centis)
}

fn ass_escape(s: &str) -> String {
    // ASS uses `{}` for inline override codes — escape literal braces. Newlines
    // become `\N`. Commas are left intact: this is used for the trailing Text
    // field (and freeform Title), where a comma is a literal character. Use
    // [`ass_field`] for any earlier comma-DELIMITED field (Name, Fontname).
    s.replace('{', "\\{")
        .replace('}', "\\}")
        .replace('\n', "\\N")
}

/// Escape a value destined for a comma-DELIMITED ASS field (`Name`, `Fontname`).
/// Same as [`ass_escape`] but also neutralizes commas → a speaker name like
/// "Smith, Jr." or a font with a comma would otherwise shift every following
/// field and corrupt the `Dialogue:`/`Style:` line.
fn ass_field(s: &str) -> String {
    ass_escape(s).replace(',', " ")
}

fn format_ass_style(name: &str, s: &Style) -> String {
    format_ass_style_with(name, s, None)
}

/// `format_ass_style` with an optional `SecondaryColour` override (karaoke's
/// pending colour). `None` keeps the historical placeholder value verbatim.
fn format_ass_style_with(name: &str, s: &Style, secondary_override: Option<&str>) -> String {
    // ASS uses BGR hex with `&H` prefix and alpha; we keep alpha 00 (opaque).
    let primary = hex_to_ass_bgr(&s.color_fg);
    let outline = hex_to_ass_bgr(&s.outline_color);
    // Alignment numpad (1-9). Map (align_h, align_v) → ASS code.
    let alignment = match (s.align_h.as_str(), s.align_v.as_str()) {
        ("left", "bottom") => 1,
        ("center", "bottom") => 2,
        ("right", "bottom") => 3,
        ("left", "middle") => 4,
        ("center", "middle") => 5,
        ("right", "middle") => 6,
        ("left", "top") => 7,
        ("center", "top") => 8,
        ("right", "top") => 9,
        _ => 2,
    };
    let bold = if s.font_weight >= 600 { -1 } else { 0 };
    let italic = if s.italic { -1 } else { 0 };
    format!(
        "Style: {name},{font},{size},{primary},{secondary},{outline},{back},{bold},{italic},0,0,100,100,{spacing},0,1,{outline_w},{shadow},{alignment},10,10,{marginv},1\n",
        name = name,
        font = ass_field(&s.font_family), // Fontname is comma-delimited
        size = s.font_size_px,
        primary = primary,
        secondary = secondary_override.unwrap_or("&H000000FF"),
        outline = outline,
        back = "&H64000000",
        bold = bold,
        italic = italic,
        spacing = s.letter_spacing,
        outline_w = s.outline_width_px,
        shadow = s.shadow_blur,
        alignment = alignment,
        marginv = 20,
    )
}

/// Convert "#RRGGBB" to ASS-style "&H00BBGGRR" (BGR + alpha).
fn hex_to_ass_bgr(hex: &str) -> String {
    let h = hex.trim_start_matches('#');
    // Need ≥6 ASCII hex digits. A non-conforming value (too short, or a multibyte
    // char inside the first 6 bytes from a hand-edited/migrated project file) must
    // fall back to white, NOT panic on a char-boundary byte-slice.
    if h.len() < 6 || !h.as_bytes()[..6].iter().all(u8::is_ascii_hexdigit) {
        return "&H00FFFFFF".to_string();
    }
    // First 6 bytes are ASCII hex (1 byte each) → these slices are on char boundaries.
    let r = &h[0..2];
    let g = &h[2..4];
    let b = &h[4..6];
    format!(
        "&H00{}{}{}",
        b.to_uppercase(),
        g.to_uppercase(),
        r.to_uppercase()
    )
}

/// Convert "#RRGGBB" to an INLINE ASS colour token `&HBBGGRR&` for `\c`
/// overrides. Six digits, no alpha byte: `\c` sets colour only (alpha is
/// `\1a`), and a trailing `&` is required to terminate the token — without it
/// libass keeps swallowing the following tag characters.
fn hex_to_ass_inline_color(hex: &str) -> String {
    // Reuse the validated conversion, then drop the `&H00` alpha prefix.
    let bgr = hex_to_ass_bgr(hex);
    format!("&H{}&", &bgr[4..])
}

// ── Plain text ──────────────────────────────────────────────────────────────

pub fn write_txt(project: &Project, opts: TxtOptions) -> String {
    let mut out = String::with_capacity(project.captions.len() * 60);
    let speakers_map = speakers_by_id(&project.speakers);
    let mut last_speaker: Option<&str> = None;
    for c in &project.captions {
        if c.words.is_empty() && opts.strip_empty {
            continue;
        }
        if opts.include_speakers {
            let current_speaker = c
                .speaker_id
                .as_deref()
                .and_then(|id| speakers_map.get(id).map(|n| n.as_str()));
            if current_speaker != last_speaker {
                if let Some(name) = current_speaker {
                    if !out.is_empty() {
                        out.push_str("\n\n");
                    }
                    out.push_str(name);
                    out.push_str(":\n");
                }
                last_speaker = current_speaker;
            }
        }
        out.push_str(&c.text());
        out.push(' ');
    }
    out.trim_end().to_string()
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TxtOptions {
    pub include_speakers: bool,
    pub strip_empty: bool,
}

// ── JSON ──────────────────────────────────────────────────────────────────

/// Developer-facing JSON export. A stable, documented schema kept separate
/// from the internal `Project` so the export contract doesn't shift when
/// internals change. Per-word timing + confidence are preserved.
pub fn write_json(project: &Project, opts: JsonOptions) -> String {
    let doc = JsonExport {
        format: "sundayedit-captions",
        version: 1,
        project: project.name.clone(),
        language: project.language.clone(),
        speakers: project
            .speakers
            .iter()
            .map(|s| JsonSpeaker {
                id: s.id.clone(),
                name: s.display_name.clone(),
                color: s.color_hex.clone(),
            })
            .collect(),
        captions: project
            .captions
            .iter()
            .filter(|c| !(opts.strip_empty && c.words.is_empty()))
            .map(|c| JsonCaption {
                id: c.id.clone(),
                start_ms: c.start_ms,
                end_ms: c.end_ms,
                text: c.text(),
                speaker_id: c.speaker_id.clone(),
                words: c
                    .words
                    .iter()
                    .map(|w| JsonWord {
                        text: w.text.clone(),
                        start_ms: w.start_ms,
                        end_ms: w.end_ms,
                        confidence: w.confidence,
                    })
                    .collect(),
            })
            .collect(),
    };
    // Serializing our own owned structs cannot fail.
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
}

#[derive(Debug, Clone, Copy, Default)]
pub struct JsonOptions {
    pub strip_empty: bool,
}

#[derive(Serialize)]
struct JsonExport {
    format: &'static str,
    version: u32,
    project: String,
    language: String,
    speakers: Vec<JsonSpeaker>,
    captions: Vec<JsonCaption>,
}

#[derive(Serialize)]
struct JsonSpeaker {
    id: String,
    name: String,
    color: Option<String>,
}

#[derive(Serialize)]
struct JsonCaption {
    id: String,
    start_ms: i64,
    end_ms: i64,
    text: String,
    speaker_id: Option<String>,
    words: Vec<JsonWord>,
}

#[derive(Serialize)]
struct JsonWord {
    text: String,
    start_ms: i64,
    end_ms: i64,
    confidence: f32,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

// ── DOCX ──────────────────────────────────────────────────────────────────

const DOCX_CONTENT_TYPES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
</Types>";

const DOCX_RELS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
</Relationships>";

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn docx_paragraph(text: &str) -> String {
    format!(
        "<w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
        xml_escape(text)
    )
}

/// Build a minimal but valid .docx (OOXML) for non-technical review: the
/// project name as a heading, then one paragraph per caption (optional
/// "Speaker:" prefix). Returns the zip bytes. Pure — writes to an in-memory
/// buffer — so it's testable offline.
pub fn build_docx(project: &Project, opts: TxtOptions) -> AppResult<Vec<u8>> {
    use std::io::Write;

    let speakers = speakers_by_id(&project.speakers);
    let mut body = docx_paragraph(&project.name);
    for c in &project.captions {
        if opts.strip_empty && c.words.is_empty() {
            continue;
        }
        let text = if opts.include_speakers {
            format_with_speaker(c, &speakers)
        } else {
            c.text()
        };
        body.push_str(&docx_paragraph(&text));
    }

    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body>{body}<w:sectPr/></w:body></w:document>"
    );

    let map_zip = |e: zip::result::ZipError| AppError::Internal(format!("docx zip: {e}"));
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opt = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    zw.start_file("[Content_Types].xml", opt).map_err(map_zip)?;
    zw.write_all(DOCX_CONTENT_TYPES.as_bytes())?;
    zw.start_file("_rels/.rels", opt).map_err(map_zip)?;
    zw.write_all(DOCX_RELS.as_bytes())?;
    zw.start_file("word/document.xml", opt).map_err(map_zip)?;
    zw.write_all(document.as_bytes())?;

    Ok(zw.finish().map_err(map_zip)?.into_inner())
}

fn speakers_by_id(speakers: &[Speaker]) -> std::collections::HashMap<String, String> {
    speakers
        .iter()
        .map(|s| (s.id.clone(), s.display_name.clone()))
        .collect()
}

fn format_with_speaker(c: &Caption, map: &std::collections::HashMap<String, String>) -> String {
    if let Some(id) = &c.speaker_id {
        if let Some(name) = map.get(id) {
            return format!("{}: {}", name, c.text());
        }
    }
    c.text()
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Caption, Project, Speaker, Style, Word};

    fn p() -> Project {
        Project {
            id: "p".into(),
            name: "test.mp4".into(),
            video_path: "/x.mp4".into(),
            video_content_hash: "h".into(),
            video_duration_ms: 60_000,
            video_width: 1920,
            video_height: 1080,
            video_fps: 30.0,
            audio_wav_path: None,
            language: "en".into(),
            default_style: Style::broadcast_news(),
            context_description: None,
            captions: vec![
                Caption {
                    id: "c1".into(),
                    start_ms: 1500,
                    end_ms: 3750,
                    words: vec![
                        Word::new("Hello", 1500, 1900, 95.0),
                        Word::new("world", 2000, 3700, 80.0),
                    ],
                    speaker_id: Some("s1".into()),
                    style_id: None,
                    notes: None,
                    ai_generated: true,
                    last_edited_at: 0,
                    track_id: None,
                },
                Caption {
                    id: "c2".into(),
                    start_ms: 4000,
                    end_ms: 7250,
                    words: vec![
                        Word::new("This", 4000, 4300, 90.0),
                        Word::new("is", 4300, 4400, 90.0),
                        Word::new("two", 4400, 7000, 80.0),
                    ],
                    speaker_id: Some("s2".into()),
                    style_id: None,
                    notes: None,
                    ai_generated: true,
                    last_edited_at: 0,
                    track_id: None,
                },
            ],
            speakers: vec![
                Speaker {
                    id: "s1".into(),
                    display_name: "Pastor Lars".into(),
                    color_hex: None,
                },
                Speaker {
                    id: "s2".into(),
                    display_name: "Maria".into(),
                    color_hex: None,
                },
            ],
            glossary: vec![],
            clips: vec![],
            talk_summary: None,
            export_config: crate::model::ExportConfig::default(),
            project_meta: crate::model::ProjectMeta::default(),
            created_at: 0,
            updated_at: 0,
            media: vec![],
            tracks: vec![],
            timeline_items: vec![],
        }
    }

    // ── Clip ASS ───────────────────────────────────────────────────────────
    #[test]
    fn clip_ass_offsets_captions_and_adds_title() {
        use crate::model::Clip;
        let clip = Clip {
            id: "clip:0".into(),
            title: "Grace".into(),
            hook: "h".into(),
            caption_ids: vec!["c1".into(), "c2".into()],
            start_ms: 1500,
            end_ms: 7250,
        };
        let ass = write_clip_ass(&p(), &clip, &Style::title_overlay(), 1080, 1920);
        assert!(ass.contains("Style: Default,"));
        assert!(ass.contains("Style: Title,"));
        assert!(ass.contains("PlayResX: 1080"));
        assert!(ass.contains("PlayResY: 1920"));
        // c1 (1500ms) becomes clip-relative 0; ends 3750-1500=2250ms = .25.
        assert!(ass.contains("Dialogue: 0,0:00:00.00,0:00:02.25,Default,,0,0,0,,Hello world"));
        // c2 starts 4000-1500=2500ms = 0:00:02.50.
        assert!(ass.contains("0:00:02.50"));
        // title overlay on layer 1 spans the whole 5750ms clip.
        assert!(ass.contains("Dialogue: 1,0:00:00.00,0:00:05.75,Title,,0,0,0,,Grace"));
    }

    #[test]
    fn clip_ass_excludes_out_of_range_captions() {
        use crate::model::Clip;
        let clip = Clip {
            id: "x".into(),
            title: "T".into(),
            hook: "".into(),
            caption_ids: vec!["c2".into()],
            start_ms: 4000,
            end_ms: 7250,
        };
        let ass = write_clip_ass(&p(), &clip, &Style::title_overlay(), 1080, 1920);
        assert!(!ass.contains("Hello world")); // c1 is before the clip
        assert!(ass.contains("This is two"));
    }

    // ── SRT ────────────────────────────────────────────────────────────────
    #[test]
    fn srt_basic_shape() {
        let out = write_srt(&p(), SrtOptions::default());
        assert!(out.starts_with("1\r\n00:00:01,500 --> 00:00:03,750\r\nHello world\r\n\r\n"));
        assert!(out.contains("2\r\n00:00:04,000 --> 00:00:07,250\r\nThis is two\r\n\r\n"));
    }

    // A caption whose text contains a blank line (e.g. a multi-line value pasted
    // via find/replace) must not break SRT/VTT cue framing. In SRT a blank line
    // ('\n\n') terminates a cue, so an embedded one truncates the cue and
    // desynchronises every following index — corrupting the whole file. The
    // writer must neutralise embedded blank lines (and bare CRs) so cue
    // boundaries stay intact.
    #[test]
    fn srt_caption_with_embedded_blank_line_does_not_break_cue_framing() {
        let mut proj = p();
        // One caption, single word, whose text holds a blank line.
        proj.captions = vec![Caption {
            id: "c".into(),
            start_ms: 0,
            end_ms: 1000,
            words: vec![Word::new("a\n\nb", 0, 1000, 90.0)],
            speaker_id: None,
            style_id: None,
            notes: None,
            ai_generated: true,
            last_edited_at: 0,
            track_id: None,
        }];
        let out = write_srt(&proj, SrtOptions::default());
        // SRT parsers split cues on a blank line REGARDLESS of CR, so normalise
        // CRLF→LF and count blank-line separators: the only one must be the cue
        // terminator at the end. An embedded one splits this single cue in two
        // and desynchronises all later indices.
        let normalised = out.replace("\r\n", "\n");
        let blank_separators = normalised.matches("\n\n").count();
        assert_eq!(
            blank_separators, 1,
            "embedded blank line corrupted SRT cue framing: {out:?}"
        );
    }

    #[test]
    fn vtt_caption_with_embedded_blank_line_does_not_break_cue_framing() {
        let mut proj = p();
        proj.captions = vec![Caption {
            id: "c".into(),
            start_ms: 0,
            end_ms: 1000,
            words: vec![Word::new("a\n\nb", 0, 1000, 90.0)],
            speaker_id: None,
            style_id: None,
            notes: None,
            ai_generated: true,
            last_edited_at: 0,
            track_id: None,
        }];
        let out = write_vtt(&proj, VttOptions::default());
        // After the "WEBVTT\n\n" header there must be exactly one cue, so exactly
        // one trailing blank-line separator. An embedded blank line would make
        // the cue text look like a second (timestamp-less) cue.
        let body = out.strip_prefix("WEBVTT\n\n").unwrap_or(&out);
        let blank_separators = body.matches("\n\n").count();
        assert_eq!(
            blank_separators, 1,
            "embedded blank line corrupted VTT cue framing: {out:?}"
        );
    }

    #[test]
    fn vtt_cue_ids_stay_contiguous_when_strip_empty_drops_a_cue() {
        // A wordless caption between two real ones must not leave a gap in the
        // VTT cue numbering (1, 2 — not 1, 3) when strip_empty removes it.
        let mut proj = p();
        proj.captions.insert(
            1,
            Caption {
                id: "empty".into(),
                start_ms: 3800,
                end_ms: 3900,
                words: vec![],
                speaker_id: None,
                style_id: None,
                notes: None,
                ai_generated: true,
                last_edited_at: 0,
                track_id: None,
            },
        );
        let out = write_vtt(
            &proj,
            VttOptions {
                include_speakers: false,
                strip_empty: true,
            },
        );
        let body = out.strip_prefix("WEBVTT\n\n").unwrap_or(&out);
        // Two cues emitted, numbered 1 then 2 (the dropped empty cue leaves no gap).
        assert!(
            body.starts_with("1\n"),
            "first cue id should be 1: {body:?}"
        );
        assert!(
            body.contains("\n2\n"),
            "second cue id should be 2: {body:?}"
        );
        assert!(
            !body.contains("\n3\n"),
            "no gap from the dropped cue: {body:?}"
        );
    }

    #[test]
    fn json_is_valid_and_preserves_words() {
        let out = write_json(&p(), JsonOptions::default());
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(v["format"], "sundayedit-captions");
        assert_eq!(v["version"], 1);
        assert_eq!(v["language"], "en");
        assert_eq!(v["captions"].as_array().unwrap().len(), 2);
        let first = &v["captions"][0];
        assert_eq!(first["text"], "Hello world");
        assert_eq!(first["speaker_id"], "s1");
        // Per-word timing + confidence are preserved (killer feature data).
        assert_eq!(first["words"][0]["text"], "Hello");
        assert_eq!(first["words"][0]["start_ms"], 1500);
        assert_eq!(first["words"][1]["confidence"], 80.0);
        // Speakers carried for cross-reference.
        assert_eq!(v["speakers"][0]["name"], "Pastor Lars");
    }

    #[test]
    fn docx_is_a_valid_zip_with_required_parts_and_text() {
        use std::io::Read;
        let bytes = build_docx(&p(), TxtOptions::default()).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        assert!(zip.by_name("[Content_Types].xml").is_ok());
        assert!(zip.by_name("_rels/.rels").is_ok());
        let mut doc = String::new();
        zip.by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut doc)
            .unwrap();
        assert!(doc.contains("Hello world"));
        assert!(doc.contains("This is two"));
        assert!(doc.contains("w:document"));
    }

    #[test]
    fn docx_escapes_xml_special_chars() {
        use std::io::Read;
        let mut proj = p();
        proj.captions = vec![Caption {
            id: "c".into(),
            start_ms: 0,
            end_ms: 1000,
            words: vec![
                Word::new("a", 0, 300, 90.0),
                Word::new("&", 300, 600, 90.0),
                Word::new("<b>", 600, 1000, 90.0),
            ],
            speaker_id: None,
            style_id: None,
            notes: None,
            ai_generated: true,
            last_edited_at: 0,
            track_id: None,
        }];
        let bytes = build_docx(&proj, TxtOptions::default()).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut doc = String::new();
        zip.by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut doc)
            .unwrap();
        assert!(doc.contains("&amp;"));
        assert!(doc.contains("&lt;b&gt;"));
        assert!(!doc.contains("<b>")); // raw tag must not leak into the body text
    }

    #[test]
    fn json_strip_empty_drops_wordless_captions() {
        let mut proj = p();
        proj.captions.push(Caption {
            id: "empty".into(),
            start_ms: 8000,
            end_ms: 9000,
            words: vec![],
            speaker_id: None,
            style_id: None,
            notes: None,
            ai_generated: true,
            last_edited_at: 0,
            track_id: None,
        });
        let out = write_json(&proj, JsonOptions { strip_empty: true });
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["captions"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn srt_with_speakers() {
        let out = write_srt(
            &p(),
            SrtOptions {
                include_speakers: true,
                strip_empty: false,
            },
        );
        assert!(out.contains("Pastor Lars: Hello world"));
        assert!(out.contains("Maria: This is two"));
    }

    #[test]
    fn srt_time_format_zero_pads() {
        assert_eq!(fmt_srt_time(0), "00:00:00,000");
        assert_eq!(fmt_srt_time(1), "00:00:00,001");
        assert_eq!(fmt_srt_time(1_000), "00:00:01,000");
        assert_eq!(fmt_srt_time(61_500), "00:01:01,500");
        assert_eq!(fmt_srt_time(3_600_000), "01:00:00,000");
    }

    // ── Property: timecode formatters are exactly reversible ─────────────────
    //
    // SRT/VTT timestamps are the load-bearing output: a player reads them back
    // by parsing HH:MM:SS,mmm / HH:MM:SS.mmm into milliseconds. The formatter is
    // only correct if a faithful parser recovers the exact same non-negative ms
    // (no rounding, truncation, or field-overflow drift). We parse the formatted
    // string back with an independent reference parser and assert equality across
    // a fixed adversarial table plus a fixed-seed PRNG sweep capped at 500.
    fn parse_hms_millis(s: &str, sep: char) -> i64 {
        // "HH:MM:SS<sep>mmm" — independent reference parser for the round-trip.
        let (hms, millis) = s.split_once(sep).expect("missing millis separator");
        let mut parts = hms.split(':');
        let h: i64 = parts.next().unwrap().parse().unwrap();
        let m: i64 = parts.next().unwrap().parse().unwrap();
        let sec: i64 = parts.next().unwrap().parse().unwrap();
        assert!(parts.next().is_none(), "too many ':' fields in {s:?}");
        assert!(m < 60 && sec < 60, "fields not normalised in {s:?}");
        assert_eq!(millis.len(), 3, "millis must be zero-padded to 3 in {s:?}");
        let ms: i64 = millis.parse().unwrap();
        ((h * 60 + m) * 60 + sec) * 1000 + ms
    }

    #[test]
    fn timecode_formatters_round_trip_to_exact_ms() {
        // Fixed adversarial table: boundaries that commonly break field math.
        let table: [i64; 12] = [
            0,
            1,
            999,
            1_000,
            59_999,
            60_000,
            3_599_999,
            3_600_000,
            3_661_001,
            86_399_999, // 23:59:59,999
            86_400_000, // 24:00:00,000 — hours overflow past two digits is fine
            359_999_999,
        ];
        for &ms in &table {
            assert_eq!(parse_hms_millis(&fmt_srt_time(ms), ','), ms, "srt ms={ms}");
            assert_eq!(parse_hms_millis(&fmt_vtt_time(ms), '.'), ms, "vtt ms={ms}");
        }

        // Fixed-seed PRNG sweep, ≤500 iterations, over the realistic 0..100h range.
        let mut state: u64 = 0xC0FF_EE12_3456_789B;
        let mut next = || {
            // xorshift64*
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };
        for _ in 0..500 {
            let ms = (next() % 360_000_000) as i64; // 0..100h
            assert_eq!(parse_hms_millis(&fmt_srt_time(ms), ','), ms, "srt ms={ms}");
            assert_eq!(parse_hms_millis(&fmt_vtt_time(ms), '.'), ms, "vtt ms={ms}");
        }
    }

    // ── VTT ────────────────────────────────────────────────────────────────
    #[test]
    fn vtt_header_present() {
        let out = write_vtt(&p(), VttOptions::default());
        assert!(out.starts_with("WEBVTT\n\n"));
    }

    #[test]
    fn vtt_time_format_dot_not_comma() {
        let out = write_vtt(&p(), VttOptions::default());
        assert!(out.contains("00:00:01.500 --> 00:00:03.750"));
        assert!(!out.contains(","));
    }

    #[test]
    fn vtt_speakers_use_voice_tag() {
        let out = write_vtt(
            &p(),
            VttOptions {
                include_speakers: true,
                strip_empty: false,
            },
        );
        assert!(out.contains("<v Pastor Lars>Hello world</v>"));
        assert!(out.contains("<v Maria>This is two</v>"));
    }

    #[test]
    fn vtt_escapes_html_chars() {
        let mut proj = p();
        proj.captions[0].words[0].text = "<test>".into();
        let out = write_vtt(&proj, VttOptions::default());
        assert!(out.contains("&lt;test&gt;"));
    }

    // ── ASS ────────────────────────────────────────────────────────────────
    #[test]
    fn ass_has_required_sections() {
        let out = write_ass(&p());
        assert!(out.contains("[Script Info]"));
        assert!(out.contains("[V4+ Styles]"));
        assert!(out.contains("[Events]"));
        assert!(out.contains("Style: Default,Helvetica Neue"));
    }

    #[test]
    fn ass_includes_dialogue_events() {
        let out = write_ass(&p());
        // Centisecond format: 1500ms → 0:00:01.50
        assert!(out
            .contains("Dialogue: 0,0:00:01.50,0:00:03.75,Default,Pastor Lars,0,0,0,,Hello world"));
    }

    // ── ASS karaoke (E4a) ──────────────────────────────────────────────────
    //
    // The flagship rule: turning karaoke ON must be the ONLY thing that changes
    // the output. Everything below either pins the OFF output byte-for-byte or
    // asserts a property of the ON output.

    use crate::services::karaoke::{karaoke_words, KaraokeOptions, KaraokeStyle};

    /// The complete pre-E4a `.ass` for the `p()` fixture, captured before the
    /// karaoke work landed. Any diff here is a regression in the flagship
    /// export path — including whitespace and field order.
    const GOLDEN_ASS: &str = "\
[Script Info]
Title: test.mp4
ScriptType: v4.00+
PlayResX: 1920
PlayResY: 1080
WrapStyle: 0
ScaledBorderAndShadow: yes
YCbCr Matrix: TV.709

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Helvetica Neue,42,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,-1,0,0,0,100,100,0,0,1,3,6,2,10,10,20,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.50,0:00:03.75,Default,Pastor Lars,0,0,0,,Hello world
Dialogue: 0,0:00:04.00,0:00:07.25,Default,Maria,0,0,0,,This is two
";

    fn karaoke_on(style: KaraokeStyle) -> KaraokeOptions {
        KaraokeOptions {
            enabled: true,
            style,
            ..Default::default()
        }
    }

    /// Strip every `{...}` override block — what's left must be the plain text.
    fn strip_ass_overrides(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut depth = 0usize;
        for ch in s.chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                _ if depth == 0 => out.push(ch),
                _ => {}
            }
        }
        out
    }

    fn dialogue_texts(ass: &str) -> Vec<String> {
        ass.lines()
            .filter(|l| l.starts_with("Dialogue:"))
            .map(|l| {
                // Text is the 10th (trailing) field — everything after the 9th comma.
                l.splitn(10, ',').nth(9).unwrap_or("").to_string()
            })
            .collect()
    }

    #[test]
    fn ass_output_is_byte_identical_when_karaoke_off() {
        // Default project (no karaoke persisted) — the pre-E4a bytes exactly.
        assert_eq!(write_ass(&p()), GOLDEN_ASS);
        // An explicitly-disabled option object must be identical too.
        assert_eq!(
            write_ass_with(&p(), &KaraokeOptions::disabled()),
            GOLDEN_ASS
        );
        // …and so must a project that persisted a fully-configured but DISABLED
        // karaoke block (pending colour / tint settings must stay inert).
        let mut proj = p();
        proj.export_config.karaoke = Some(KaraokeOptions {
            enabled: false,
            style: KaraokeStyle::Sweep,
            pending_color: "#123456".into(),
            confidence_tint: true,
            confidence_threshold: 99.0,
            low_confidence_color: "#FF0000".into(),
        });
        assert_eq!(
            write_ass(&proj),
            GOLDEN_ASS,
            "a disabled karaoke block must not leak ANY byte into the output"
        );
    }

    #[test]
    fn karaoke_emits_one_k_tag_per_word_in_order() {
        let ass = write_ass_with(&p(), &karaoke_on(KaraokeStyle::Highlight));
        let texts = dialogue_texts(&ass);
        assert_eq!(texts.len(), 2);
        // c1: 1500..3750, words Hello(1500..1900) world(2000..3700).
        // Cut at word 2's start (2000) → 50 cs, then the rest → 175 cs.
        assert_eq!(texts[0], "{\\k50}Hello {\\k175}world");
        // c2: 4000..7250, cuts at 4300 and 4400 → 30, 10, 285.
        assert_eq!(texts[1], "{\\k30}This {\\k10}is {\\k285}two");
    }

    #[test]
    fn karaoke_sweep_uses_kf() {
        let ass = write_ass_with(&p(), &karaoke_on(KaraokeStyle::Sweep));
        let texts = dialogue_texts(&ass);
        assert_eq!(texts[0], "{\\kf50}Hello {\\kf175}world");
        assert!(!ass.contains("{\\k5"), "sweep must not emit plain \\k tags");
    }

    /// `\k` durations are CUMULATIVE in libass. If they don't sum to the
    /// Dialogue span, every word after the first rounding error drifts — the
    /// exact failure this whole module exists to prevent.
    #[test]
    fn karaoke_tag_durations_sum_to_the_dialogue_span() {
        let ass = write_ass_with(&p(), &karaoke_on(KaraokeStyle::Highlight));
        for (line, c) in ass
            .lines()
            .filter(|l| l.starts_with("Dialogue:"))
            .zip(p().captions.iter())
        {
            let text = line.splitn(10, ',').nth(9).unwrap();
            let sum: i64 = text
                .split("{\\k")
                .skip(1)
                .map(|chunk| {
                    chunk
                        .split('}')
                        .next()
                        .unwrap()
                        .parse::<i64>()
                        .expect("a \\k tag carries an integer centisecond count")
                })
                .sum();
            let span_cs = c.end_ms / 10 - c.start_ms / 10;
            assert_eq!(sum, span_cs, "\\k ladder must close on the Dialogue span");
        }
    }

    #[test]
    fn karaoke_text_with_tags_stripped_equals_the_plain_text() {
        // The rendered WORDS must be untouched by karaoke — only the timing
        // overrides are added. This is the "captions never regress" guard.
        let plain = dialogue_texts(&write_ass(&p()));
        let kara = dialogue_texts(&write_ass_with(&p(), &karaoke_on(KaraokeStyle::Highlight)));
        for (a, b) in plain.iter().zip(kara.iter()) {
            assert_eq!(*a, strip_ass_overrides(b));
        }
    }

    #[test]
    fn karaoke_sets_secondary_colour_to_the_pending_colour() {
        let opts = KaraokeOptions {
            enabled: true,
            pending_color: "#102030".into(),
            ..Default::default()
        };
        let ass = write_ass_with(&p(), &opts);
        // SecondaryColour is the 5th Style field; ASS is BGR → 30,20,10.
        assert!(
            ass.contains("&H00FFFFFF,&H00302010,"),
            "pending colour must land in SecondaryColour: {ass}"
        );
        assert!(
            !ass.contains("&H000000FF"),
            "the inert placeholder secondary must be gone once karaoke is on"
        );
    }

    #[test]
    fn confidence_tint_is_off_by_default() {
        let ass = write_ass_with(&p(), &karaoke_on(KaraokeStyle::Highlight));
        assert!(
            !ass.contains("\\c&H"),
            "no inline colour overrides unless the user opts in: {ass}"
        );
    }

    #[test]
    fn confidence_tint_marks_low_confidence_words_and_restates_the_normal_colour() {
        let mut proj = p();
        // "world" at 80 stays trusted; drop it below the tier-2 floor.
        proj.captions[0].words[1].confidence = 30.0;
        let opts = KaraokeOptions {
            enabled: true,
            confidence_tint: true,
            low_confidence_color: "#FF0000".into(),
            ..Default::default()
        };
        let texts = dialogue_texts(&write_ass_with(&proj, &opts));
        // Style fg is #FFFFFF → &HFFFFFF&; low colour #FF0000 → &H0000FF&.
        assert_eq!(
            texts[0], "{\\k50\\c&HFFFFFF&}Hello {\\k175\\c&H0000FF&}world",
            "every run restates its colour so a tint cannot bleed forward"
        );
    }

    #[test]
    fn confidence_tint_respects_locked_and_edited_words() {
        let mut proj = p();
        proj.captions[0].words[1].confidence = 5.0;
        proj.captions[0].words[1].locked = true;
        let opts = KaraokeOptions {
            enabled: true,
            confidence_tint: true,
            low_confidence_color: "#FF0000".into(),
            ..Default::default()
        };
        let texts = dialogue_texts(&write_ass_with(&proj, &opts));
        assert!(
            !texts[0].contains("&H0000FF&"),
            "a user-locked word is trusted and must not be flagged: {}",
            texts[0]
        );
    }

    #[test]
    fn karaoke_escapes_braces_in_word_text() {
        // A word containing `{` must not be mistaken for an override block —
        // otherwise a stray brace swallows the following karaoke tags.
        let mut proj = p();
        proj.captions[0].words[0].text = "{oops}".into();
        let ass = write_ass_with(&proj, &karaoke_on(KaraokeStyle::Highlight));
        let texts = dialogue_texts(&ass);
        assert_eq!(texts[0], "{\\k50}\\{oops\\} {\\k175}world");
    }

    #[test]
    fn karaoke_handles_a_wordless_caption() {
        let mut proj = p();
        proj.captions[0].words.clear();
        let ass = write_ass_with(&proj, &karaoke_on(KaraokeStyle::Highlight));
        let texts = dialogue_texts(&ass);
        // One span covering the whole caption, with empty text.
        assert_eq!(texts[0], "{\\k225}");
    }

    #[test]
    fn karaoke_writer_and_timing_module_agree_word_for_word() {
        // The seam this module exists to close: the ASS writer must never
        // re-derive a duration of its own.
        let opts = karaoke_on(KaraokeStyle::Highlight);
        let texts = dialogue_texts(&write_ass_with(&p(), &opts));
        for (text, c) in texts.iter().zip(p().captions.iter()) {
            let expected: String = karaoke_words(c)
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    let sep = if i == 0 { "" } else { " " };
                    format!("{sep}{{\\k{}}}{}", w.duration_cs, w.text)
                })
                .collect();
            assert_eq!(*text, expected);
        }
    }

    #[test]
    fn project_karaoke_defaults_to_disabled_for_pre_e4a_projects() {
        assert!(!project_karaoke(&p()).enabled);
    }

    #[test]
    fn write_ass_reads_the_persisted_project_setting() {
        let mut proj = p();
        proj.export_config.karaoke = Some(karaoke_on(KaraokeStyle::Sweep));
        // burnin.rs and all three compose paths call plain `write_ass` — this
        // is what makes preview-proxy and final export agree.
        assert!(write_ass(&proj).contains("{\\kf"));
    }

    #[test]
    fn clip_ass_karaoke_uses_clip_relative_timings() {
        use crate::model::Clip;
        let mut proj = p();
        proj.export_config.karaoke = Some(karaoke_on(KaraokeStyle::Highlight));
        let clip = Clip {
            id: "clip:0".into(),
            title: "Grace".into(),
            hook: "h".into(),
            caption_ids: vec!["c1".into()],
            start_ms: 1500,
            end_ms: 7250,
        };
        let ass = write_clip_ass(&proj, &clip, &Style::title_overlay(), 1080, 1920);
        let texts = dialogue_texts(&ass);
        // c1 becomes 0..2250 clip-relative; the cut at word 2 (2000 → 500) gives
        // 50 cs then 175 cs — the same ladder, rebased, still closing exactly.
        assert_eq!(texts[0], "{\\k50}Hello {\\k175}world");
        // The Title overlay is NOT karaoke'd.
        assert!(
            texts.iter().any(|t| t == "Grace"),
            "title stays plain: {texts:?}"
        );
    }

    #[test]
    fn hex_to_ass_inline_color_is_six_digits_and_terminated() {
        assert_eq!(hex_to_ass_inline_color("#FF0000"), "&H0000FF&");
        assert_eq!(hex_to_ass_inline_color("#102030"), "&H302010&");
        assert_eq!(hex_to_ass_inline_color("bogus"), "&HFFFFFF&");
    }

    #[test]
    fn export_config_without_karaoke_deserializes() {
        // Pre-E4a persisted JSON must still load — and load as OFF.
        let cfg: crate::model::ExportConfig = serde_json::from_str(
            r#"{"format":"srt","burn_in":false,"caption_size_px":24,
                "caption_color":"white","caption_background":"none",
                "max_chars_per_line":42}"#,
        )
        .expect("pre-E4a ExportConfig JSON still deserializes");
        assert!(cfg.karaoke.is_none());
    }

    // ── Text timeline items → ASS (R5-C) ───────────────────────────────────

    fn text_track(id: &str) -> crate::model::Track {
        crate::model::Track {
            id: id.into(),
            kind: crate::model::TrackKind::Overlay,
            name: "Overlay".into(),
            index: 2,
            enabled: true,
            locked: false,
            muted: false,
            solo: false,
            volume_db: 0.0,
        }
    }

    fn text_item(id: &str, start: i64, dur: i64, text: &str) -> TimelineItem {
        TimelineItem {
            id: id.into(),
            track_id: "o1".into(),
            kind: TimelineItemKind::Text,
            source_media_id: None,
            in_ms: 0,
            out_ms: dur,
            timeline_start_ms: start,
            speed: 1.0,
            gain_db: 0.0,
            fade_in_ms: 0,
            fade_out_ms: 0,
            transform: Transform::default(),
            effects: vec![],
            transition_in: None,
            text: Some(crate::model::TextSpec {
                text: text.into(),
                style_id: None,
            }),
            enabled: true,
            locked: false,
        }
    }

    /// The project shape the R5-C feature exists for: one text overlay on a
    /// visible Overlay track, no captions in the way.
    fn text_project(transform: Transform) -> Project {
        let mut proj = p();
        proj.captions.clear();
        proj.tracks = vec![text_track("o1")];
        let mut it = text_item("tx1", 1_500, 2_500, "Velkommen");
        it.transform = transform;
        proj.timeline_items = vec![it];
        proj
    }

    fn dialogue_lines(ass: &str) -> Vec<&str> {
        ass.lines().filter(|l| l.starts_with("Dialogue:")).collect()
    }

    #[test]
    fn text_item_round_trips_as_a_positioned_dialogue_line() {
        // 1920×1080 project; x .25 → 480 px, y .75 → 810 px.
        let proj = text_project(Transform {
            x: 0.25,
            y: 0.75,
            ..Transform::default()
        });
        let ass = write_ass(&proj);
        assert_eq!(
            dialogue_lines(&ass),
            vec![
                // Layer 1 (above the caption band), timeline times, Default
                // style, top-left anchored at the transform's fractions.
                "Dialogue: 1,0:00:01.50,0:00:04.00,Default,,0,0,0,,{\\an7\\pos(480,810)}Velkommen"
            ],
            "{ass}"
        );
    }

    /// Resolution independence, and the parity claim behind it: the SAME
    /// fractions land on the same relative spot in a portrait project, because
    /// `PlayResX/Y` follow the project dimensions and libass scales that space
    /// to the output. This is the mutation-check target for the position math —
    /// swapping the width/height factors, or dropping the `round`, moves these
    /// numbers.
    #[test]
    fn text_position_follows_the_projects_play_res() {
        let mut proj = text_project(Transform {
            x: 0.25,
            y: 0.75,
            ..Transform::default()
        });
        proj.video_width = 1080;
        proj.video_height = 1920;
        let ass = write_ass(&proj);
        assert!(ass.contains("PlayResX: 1080"), "{ass}");
        assert!(dialogue_lines(&ass)[0].contains("\\pos(270,1440)"), "{ass}");
    }

    /// A fraction that does not land on a whole pixel rounds — it does not
    /// truncate, and it does not leak an f32 tail into the file.
    #[test]
    fn text_position_rounds_to_whole_play_res_pixels() {
        let proj = text_project(Transform {
            x: 0.3333,
            y: 0.1,
            ..Transform::default()
        });
        // 1920 * 0.3333 = 639.936 → 640;  1080 * 0.1 = 108.
        assert!(
            dialogue_lines(&write_ass(&proj))[0].contains("\\pos(640,108)"),
            "{}",
            write_ass(&proj)
        );
    }

    /// An untouched transform emits ONLY the anchor + position: no `\fsc`, no
    /// `\frz`, no `\alpha`. (Keeps the file readable and the golden tests
    /// meaningful.)
    #[test]
    fn identity_transform_emits_only_the_anchor_and_position() {
        let ass = write_ass(&text_project(Transform::default()));
        let line = dialogue_lines(&ass)[0];
        assert!(line.contains("{\\an7\\pos(0,0)}"), "{line}");
        assert!(!line.contains("fsc"), "{line}");
        assert!(!line.contains("frz"), "{line}");
        assert!(!line.contains("alpha"), "{line}");
    }

    /// Everything the inspector can store on a text item's transform reaches
    /// the file. A value the writer ignored would be authored content the
    /// export silently drops — the exact failure R5-C removed for text itself.
    #[test]
    fn scale_rotation_and_opacity_reach_the_override_block() {
        let ass = write_ass(&text_project(Transform {
            x: 0.5,
            y: 0.5,
            scale: 0.4,
            rotation_deg: 15.0,
            opacity: 0.5,
            crop: None,
        }));
        let line = dialogue_lines(&ass)[0];
        // scale → percent; rotation NEGATED (ASS `\frz` is CCW-positive while
        // the export's `rotate=` and the Pixi preview are CW-positive);
        // opacity → transparency byte (0.5 → 128 → 80 hex).
        assert!(line.contains("\\fscx40\\fscy40"), "{line}");
        assert!(line.contains("\\frz-15"), "{line}");
        assert!(line.contains("\\alpha&H80&"), "{line}");
    }

    #[test]
    fn text_is_escaped_like_any_other_dialogue_text() {
        let mut proj = text_project(Transform::default());
        proj.timeline_items[0].text = Some(crate::model::TextSpec {
            text: "line1\nline2 {not a tag}".into(),
            style_id: None,
        });
        let line = dialogue_lines(&write_ass(&proj))[0].to_string();
        assert!(line.ends_with("line1\\Nline2 \\{not a tag\\}"), "{line}");
    }

    /// `timeline_end_ms` honours speed, and so does the Dialogue End field —
    /// the overlay ends exactly where its lane does.
    #[test]
    fn dialogue_end_follows_timeline_end_ms() {
        let mut proj = text_project(Transform::default());
        proj.timeline_items[0].speed = 2.0; // 2500 ms of item → 1250 ms of lane
        let it = &proj.timeline_items[0];
        assert_eq!(it.timeline_end_ms(), 2_750);
        assert!(
            dialogue_lines(&write_ass(&proj))[0].starts_with("Dialogue: 1,0:00:01.50,0:00:02.75,"),
            "{}",
            write_ass(&proj)
        );
    }

    // ── Named styles (closes the Phase 5.2 note) ───────────────────────────

    #[test]
    fn a_referenced_preset_gets_its_own_style_block_for_text_and_captions() {
        let mut proj = text_project(Transform::default());
        proj.timeline_items[0].text = Some(crate::model::TextSpec {
            text: "Velkommen".into(),
            style_id: Some("preset:tiktok_bold".into()),
        });
        // …and a caption referencing a DIFFERENT preset: one writer, one rule.
        proj.captions = vec![Caption {
            id: "c1".into(),
            start_ms: 0,
            end_ms: 1_000,
            words: vec![Word::new("Hei", 0, 1_000, 90.0)],
            speaker_id: None,
            style_id: Some("preset:cinema".into()),
            notes: None,
            ai_generated: false,
            last_edited_at: 0,
            track_id: None,
        }];
        let ass = write_ass(&proj);

        // One block per referenced id, named from the id (`:` is not a legal
        // ASS Name character to leave raw in a comma-delimited field).
        assert!(
            ass.contains("\nStyle: preset_tiktok_bold,Montserrat,"),
            "{ass}"
        );
        assert!(ass.contains("\nStyle: preset_cinema,"), "{ass}");
        assert!(ass.contains("\nStyle: Default,Helvetica Neue"), "{ass}");

        // Karaoke's pending colour lands on EVERY block, not just Default —
        // a caption that picked a named style still sweeps against it.
        let mut kara = proj.clone();
        kara.export_config.karaoke = Some(KaraokeOptions {
            enabled: true,
            pending_color: "#00FF00".into(),
            ..KaraokeOptions::disabled()
        });
        let ass_k = write_ass(&kara);
        let pending = hex_to_ass_bgr("#00FF00");
        for block in ["Default", "preset_cinema", "preset_tiktok_bold"] {
            let line = ass_k
                .lines()
                .find(|l| l.starts_with(&format!("Style: {block},")))
                .unwrap_or_else(|| panic!("no block for {block} in {ass_k}"));
            assert!(
                line.contains(&pending),
                "{block} must carry the karaoke pending colour: {line}"
            );
        }

        // …and each Dialogue names its own block.
        let lines = dialogue_lines(&ass);
        assert!(lines[0].contains(",preset_cinema,"), "{ass}");
        assert!(lines[1].contains(",preset_tiktok_bold,"), "{ass}");
    }

    /// A style id nothing resolves (hand-edited file, a preset we dropped) must
    /// fall back to `Default` — and must NOT name a block the file lacks, which
    /// libass would swallow silently.
    #[test]
    fn an_unresolvable_style_id_falls_back_to_default() {
        let mut proj = text_project(Transform::default());
        proj.timeline_items[0].text = Some(crate::model::TextSpec {
            text: "Velkommen".into(),
            style_id: Some("preset:does_not_exist".into()),
        });
        let ass = write_ass(&proj);
        assert!(!ass.contains("preset_does_not_exist"), "{ass}");
        assert!(dialogue_lines(&ass)[0].contains(",Default,"), "{ass}");
    }

    /// Referencing the project's OWN default style is not an extra block — it
    /// is already emitted as `Default`.
    #[test]
    fn referencing_the_default_style_emits_no_second_block() {
        let mut proj = text_project(Transform::default());
        let own = proj.default_style.id.clone();
        proj.timeline_items[0].text = Some(crate::model::TextSpec {
            text: "Velkommen".into(),
            style_id: Some(own),
        });
        let ass = write_ass(&proj);
        assert_eq!(
            ass.matches("\nStyle: ").count(),
            1,
            "exactly one style block: {ass}"
        );
        assert!(dialogue_lines(&ass)[0].contains(",Default,"), "{ass}");
    }

    // ── the ass= predicate must agree with the writer ───────────────────────

    /// `compose` asks `ass_has_events` whether to hang an `ass=` node on the
    /// graph. If that predicate ever said "nothing to draw" while the writer
    /// emitted a Dialogue, the sidecar would be written and thrown away — the
    /// silent-loss failure this feature exists to remove.
    #[test]
    fn ass_predicate_agrees_with_the_writer() {
        let mut empty = p();
        empty.captions.clear();

        let mut disabled = text_project(Transform::default());
        disabled.timeline_items[0].enabled = false;

        let mut hidden = text_project(Transform::default());
        hidden.tracks[0].enabled = false;

        let mut blank = text_project(Transform::default());
        blank.timeline_items[0].text = Some(crate::model::TextSpec {
            text: "   ".into(),
            style_id: None,
        });

        let mut captions_only = p();
        captions_only.timeline_items.clear();

        // Captions AND an overlay — the writer emits both kinds of Dialogue.
        let mut both = p();
        both.tracks = vec![text_track("o1")];
        both.timeline_items = vec![text_item("tx1", 0, 1_000, "Velkommen")];

        for (name, proj) in [
            ("nothing at all", empty),
            ("disabled overlay", disabled),
            ("overlay on a hidden track", hidden),
            ("blank text", blank),
            ("captions only", captions_only),
            ("text only", text_project(Transform::default())),
            ("captions + overlay", both),
        ] {
            let writer_draws = !dialogue_lines(&write_ass(&proj)).is_empty();
            assert_eq!(
                ass_has_events(&proj),
                writer_draws,
                "predicate disagrees with the writer for: {name}"
            );
        }
    }

    #[test]
    fn ass_style_name_sanitizes_and_cannot_collide_with_default() {
        assert_eq!(ass_style_name("preset:tiktok_bold"), "preset_tiktok_bold");
        assert_eq!(ass_style_name("a,b"), "a_b");
        assert_eq!(ass_style_name(""), "Default");
    }

    #[test]
    fn ass_numbers_are_trimmed() {
        assert_eq!(fmt_ass_num(40.0), "40");
        assert_eq!(fmt_ass_num(37.5), "37.5");
        assert_eq!(fmt_ass_num(-0.0), "0");
        assert_eq!(fmt_ass_num(0.4 * 100.0), "40");
    }

    #[test]
    fn ass_time_format_centiseconds() {
        assert_eq!(fmt_ass_time(0), "0:00:00.00");
        assert_eq!(fmt_ass_time(50), "0:00:00.05");
        assert_eq!(fmt_ass_time(99), "0:00:00.09");
        assert_eq!(fmt_ass_time(1_500), "0:00:01.50");
        assert_eq!(fmt_ass_time(3_600_000), "1:00:00.00");
    }

    #[test]
    fn ass_escapes_braces_and_newlines() {
        assert_eq!(ass_escape("plain"), "plain");
        assert_eq!(ass_escape("{override}"), "\\{override\\}");
        assert_eq!(ass_escape("line1\nline2"), "line1\\Nline2");
    }

    #[test]
    fn hex_to_ass_bgr_swaps_channels() {
        assert_eq!(hex_to_ass_bgr("#FF0000"), "&H000000FF"); // red → BGR
        assert_eq!(hex_to_ass_bgr("#00FF00"), "&H0000FF00");
        assert_eq!(hex_to_ass_bgr("#0000FF"), "&H00FF0000");
        assert_eq!(hex_to_ass_bgr("#FFFFFF"), "&H00FFFFFF");
    }

    #[test]
    fn hex_to_ass_bgr_falls_back_on_bad_input() {
        assert_eq!(hex_to_ass_bgr("#fff"), "&H00FFFFFF"); // too short
        assert_eq!(hex_to_ass_bgr("#zzzzzz"), "&H00FFFFFF"); // non-hex
                                                             // Multibyte char inside the first 6 bytes must NOT panic on a slice.
        assert_eq!(hex_to_ass_bgr("#café12"), "&H00FFFFFF");
    }

    #[test]
    fn ass_field_neutralizes_commas() {
        // A comma in a delimited field (speaker Name / Fontname) would shift every
        // following field — it must be neutralized; the Text field keeps commas.
        assert_eq!(ass_field("Smith, Jr."), "Smith  Jr.");
        assert_eq!(ass_field("a,b"), "a b");
        assert_eq!(ass_field("Arial"), "Arial");
        assert!(!ass_field("{x},y").contains(','));
    }

    // ── TXT ────────────────────────────────────────────────────────────────
    #[test]
    fn txt_concatenates_captions() {
        let out = write_txt(&p(), TxtOptions::default());
        assert_eq!(out, "Hello world This is two");
    }

    #[test]
    fn txt_with_speakers_groups_by_speaker() {
        let out = write_txt(
            &p(),
            TxtOptions {
                include_speakers: true,
                strip_empty: false,
            },
        );
        assert!(out.contains("Pastor Lars:\nHello world"));
        assert!(out.contains("Maria:\nThis is two"));
    }
}
