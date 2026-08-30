/**
 * Browser-side Tauri backend mock for the E2E suite.
 *
 * The Playwright specs run against `vite preview` in a plain Chromium, where
 * `window.__TAURI_INTERNALS__` does not exist — so any `invoke()` (every
 * caption op + every exporter) would throw `reading 'invoke'`. The smoke +
 * onboarding specs sidestep this by never triggering an op; the editor +
 * export workflows can't.
 *
 * Rather than re-implement the whole Rust surface, this installs a *faithful*
 * mock of the handful of commands those workflows exercise. The op + exporter
 * implementations mirror `src-tauri/src/services/{operations,export}.rs`
 * exactly (split boundary = first right-word start, merge concatenates words +
 * spans first→last, edit marks `edited`; SRT `\r\n` + `,` ms, VTT `\n` + `.`
 * ms, ASS centiseconds, JSON `sundayedit-captions` v1). The point of the E2E
 * layer is the *wiring*: React → ipc.ts (camelCase args) → invoke → render the
 * result back into the DOM. A drift in caption ids, argument names, or the
 * render round-trip fails these specs even though the math is mocked.
 *
 * When real-IPC E2E lands (tauri-driver, see playwright.config.ts), these specs
 * point at the driver and this mock is dropped.
 */

import type { Page } from "@playwright/test";

/**
 * The whole mock backend, serialised into the page as one init script. It is
 * stringified by Playwright and run before any app code, so it must be a
 * self-contained function with no outer references.
 */
function backend(): void {
  type Alternate = { text: string; confidence: number };
  type Word = { text: string; start_ms: number; end_ms: number };
  type Caption = {
    id: string;
    start_ms: number;
    end_ms: number;
    words: (Word & {
      edited?: boolean;
      locked?: boolean;
      confidence: number;
      alternates?: Alternate[];
    })[];
    speaker_id: string | null;
  };
  // ── NLE (multi-track) entities (mirror bindings/{MediaItem,Track,TimelineItem}) ──
  type MediaItem = {
    id: string;
    path: string;
    content_hash: string;
    kind: "video" | "audio_only";
    duration_ms: number;
    width: number;
    height: number;
    fps: number;
    has_audio: boolean;
    audio_wav_path: string | null;
    original_filename: string;
    added_at: number;
  };
  type Track = {
    id: string;
    kind: "video" | "audio" | "caption" | "overlay";
    name: string;
    index: number;
    enabled: boolean;
    locked: boolean;
    muted: boolean;
    solo: boolean;
  };
  type Transform = {
    x: number;
    y: number;
    scale: number;
    rotation_deg: number;
    opacity: number;
    crop: null;
  };
  type Transition = { kind: string; duration_ms: number };
  type TextSpec = { text: string; style_id: string | null };
  type TimelineItem = {
    id: string;
    track_id: string;
    kind: "av" | "text" | "graphic";
    source_media_id: string | null;
    in_ms: number;
    out_ms: number;
    timeline_start_ms: number;
    speed: number;
    transform: Transform;
    effects: unknown[];
    transition_in: Transition | null;
    text: TextSpec | null;
    enabled: boolean;
    locked: boolean;
  };
  type Project = {
    name: string;
    language: string;
    captions: Caption[];
    speakers: { id: string; display_name: string; color_hex: string | null }[];
    // Multi-track NLE arrays — `#[serde(default)]` on the Rust side, so older
    // callers may omit them; the ops below treat a missing array as empty.
    media?: MediaItem[];
    tracks?: Track[];
    timeline_items?: TimelineItem[];
  };

  const captionText = (c: Caption) => c.words.map((w) => w.text).join(" ");

  // ── helpers (duplicated inside the page scope — see note above) ──
  const p2 = (n: number) => String(n).padStart(2, "0");
  const p3 = (n: number) => String(n).padStart(3, "0");
  const srtTime = (ms: number) => {
    const neg = ms < 0;
    const a = Math.abs(ms);
    const h = Math.floor(a / 3_600_000);
    const m = Math.floor(a / 60_000) % 60;
    const s = Math.floor(a / 1_000) % 60;
    return `${neg ? "-" : ""}${p2(h)}:${p2(m)}:${p2(s)},${p3(a % 1_000)}`;
  };
  const vttTime = (ms: number) => {
    const a = Math.max(0, ms);
    const h = Math.floor(a / 3_600_000);
    const m = Math.floor(a / 60_000) % 60;
    const s = Math.floor(a / 1_000) % 60;
    return `${p2(h)}:${p2(m)}:${p2(s)}.${p3(a % 1_000)}`;
  };
  const assTime = (ms: number) => {
    const a = Math.max(0, ms);
    const h = Math.floor(a / 3_600_000);
    const m = Math.floor(a / 60_000) % 60;
    const s = Math.floor(a / 1_000) % 60;
    return `${h}:${p2(m)}:${p2(s)}.${p2(Math.floor((a % 1_000) / 10))}`;
  };
  const vttEscape = (s: string) =>
    s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

  // ── exporters (mirror services/export.rs) ──
  function exportSrt(project: Project, stripEmpty: boolean): string {
    let out = "";
    let idx = 1;
    for (const c of project.captions) {
      if (stripEmpty && c.words.length === 0) continue;
      out += `${idx}\r\n${srtTime(c.start_ms)} --> ${srtTime(c.end_ms)}\r\n${captionText(c)}\r\n\r\n`;
      idx += 1;
    }
    return out;
  }
  function exportVtt(project: Project, stripEmpty: boolean): string {
    let out = "WEBVTT\n\n";
    project.captions.forEach((c, i) => {
      if (stripEmpty && c.words.length === 0) return;
      out += `${i + 1}\n${vttTime(c.start_ms)} --> ${vttTime(c.end_ms)}\n${vttEscape(captionText(c))}\n\n`;
    });
    return out;
  }
  function exportAss(project: Project): string {
    let out = "[Script Info]\n";
    out += `Title: ${project.name}\n`;
    out += "ScriptType: v4.00+\n";
    out += "\n[V4+ Styles]\n";
    out +=
      "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n";
    out += "Style: Default,Helvetica Neue,42,&H00FFFFFF\n";
    out += "\n[Events]\n";
    out +=
      "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";
    for (const c of project.captions) {
      out += `Dialogue: 0,${assTime(c.start_ms)},${assTime(c.end_ms)},Default,,0,0,0,,${captionText(c)}\n`;
    }
    return out;
  }
  function exportJson(project: Project, stripEmpty: boolean): string {
    const doc = {
      format: "sundayedit-captions",
      version: 1,
      project: project.name,
      language: project.language,
      speakers: project.speakers.map((s) => ({
        id: s.id,
        name: s.display_name,
        color: s.color_hex,
      })),
      captions: project.captions
        .filter((c) => !(stripEmpty && c.words.length === 0))
        .map((c) => ({
          id: c.id,
          start_ms: c.start_ms,
          end_ms: c.end_ms,
          text: captionText(c),
          speaker_id: c.speaker_id,
          words: c.words.map((w) => ({
            text: w.text,
            start_ms: w.start_ms,
            end_ms: w.end_ms,
            confidence: w.confidence,
          })),
        })),
    };
    return JSON.stringify(doc, null, 2);
  }

  // ── caption ops (mirror services/operations.rs) ──
  let nextId = 1;
  function findCaption(project: Project, id: string): number {
    const i = project.captions.findIndex((c) => c.id === id);
    if (i < 0) throw err(`caption ${id} not found`);
    return i;
  }
  function err(message: string) {
    return { code: "validation", message };
  }
  function splitCaption(
    project: Project,
    captionId: string,
    atWordIndex: number,
  ): Project {
    const ci = findCaption(project, captionId);
    const orig = project.captions[ci];
    if (atWordIndex === 0 || atWordIndex >= orig.words.length) {
      throw err(`split index ${atWordIndex} out of range`);
    }
    const left = orig.words.slice(0, atWordIndex);
    const right = orig.words.slice(atWordIndex);
    const boundary = right[0].start_ms;
    const leftCap = { ...orig, words: left, end_ms: boundary };
    const rightCap = {
      ...orig,
      id: `mock-${nextId++}`,
      words: right,
      start_ms: boundary,
    };
    const captions = project.captions.slice();
    captions.splice(ci, 1, leftCap, rightCap);
    return { ...project, captions };
  }
  function mergeCaptions(project: Project, captionIds: string[]): Project {
    if (captionIds.length < 2) throw err("merge needs at least 2 caption ids");
    const indices = captionIds.map((id) => findCaption(project, id)).sort();
    for (let i = 1; i < indices.length; i += 1) {
      if (indices[i] !== indices[i - 1] + 1) {
        throw err("captions are not contiguous");
      }
    }
    const first = indices[0];
    const last = indices[indices.length - 1];
    const words = project.captions
      .slice(first, last + 1)
      .flatMap((c) => c.words);
    const merged = {
      ...project.captions[first],
      end_ms: project.captions[last].end_ms,
      words,
    };
    const captions = project.captions.slice();
    captions.splice(first, last - first + 1, merged);
    return { ...project, captions };
  }
  function editWord(
    project: Project,
    captionId: string,
    wordIndex: number,
    newText: string,
  ): Project {
    const text = newText.trim();
    if (text.length === 0) throw err("word text cannot be empty");
    const ci = findCaption(project, captionId);
    const captions = project.captions.map((c, i) => {
      if (i !== ci) return c;
      const words = c.words.map((w, wi) =>
        wi === wordIndex ? { ...w, text, edited: true } : w,
      );
      return { ...c, words };
    });
    return { ...project, captions };
  }

  function lockWord(
    project: Project,
    captionId: string,
    wordIndex: number,
    locked: boolean,
  ): Project {
    const ci = findCaption(project, captionId);
    const captions = project.captions.map((c, i) => {
      if (i !== ci) return c;
      if (wordIndex >= c.words.length) {
        throw err(`word index ${wordIndex} out of range`);
      }
      const words = c.words.map((w, wi) =>
        wi === wordIndex ? { ...w, locked } : w,
      );
      return { ...c, words };
    });
    return { ...project, captions };
  }
  function acceptAlternate(
    project: Project,
    captionId: string,
    wordIndex: number,
    alternateIndex: number,
  ): Project {
    const ci = findCaption(project, captionId);
    const captions = project.captions.map((c, i) => {
      if (i !== ci) return c;
      if (wordIndex >= c.words.length) {
        throw err(`word index ${wordIndex} out of range`);
      }
      const alt = (c.words[wordIndex].alternates ?? [])[alternateIndex];
      if (!alt) throw err(`alternate index ${alternateIndex} out of range`);
      const words = c.words.map((w, wi) =>
        wi === wordIndex
          ? { ...w, text: alt.text, confidence: alt.confidence, edited: true }
          : w,
      );
      return { ...c, words };
    });
    return { ...project, captions };
  }
  function retimeWord(
    project: Project,
    captionId: string,
    wordIndex: number,
    newStartMs: number,
    newEndMs: number,
  ): Project {
    if (newStartMs >= newEndMs) throw err("start must be less than end");
    const ci = findCaption(project, captionId);
    const cap = project.captions[ci];
    if (wordIndex >= cap.words.length) {
      throw err(`word index ${wordIndex} out of range`);
    }
    // Bounds vs caption + neighbours (mirrors operations.rs::retime_word).
    const lower =
      wordIndex === 0 ? cap.start_ms : cap.words[wordIndex - 1].end_ms;
    const upper =
      wordIndex + 1 >= cap.words.length
        ? cap.end_ms
        : cap.words[wordIndex + 1].start_ms;
    if (newStartMs < lower || newEndMs > upper) {
      throw err(`retime (${newStartMs}, ${newEndMs}) outside bounds`);
    }
    const captions = project.captions.map((c, i) => {
      if (i !== ci) return c;
      const words = c.words.map((w, wi) =>
        wi === wordIndex ? { ...w, start_ms: newStartMs, end_ms: newEndMs } : w,
      );
      return { ...c, words };
    });
    return { ...project, captions };
  }
  function moveCaption(
    project: Project,
    captionId: string,
    deltaMs: number,
  ): Project {
    if (deltaMs === 0) return project;
    const idx = findCaption(project, captionId);
    const cap = project.captions[idx];
    const prevEnd = idx > 0 ? project.captions[idx - 1].end_ms : 0;
    const nextStart = project.captions[idx + 1]?.start_ms;
    const dur = cap.end_ms - cap.start_ms;
    const lo = Math.max(prevEnd, 0);
    const hi = nextStart === undefined ? Infinity : nextStart - dur;
    // Clamp the slide into the gap, NLE-style (mirrors operations.rs).
    const clamped =
      hi < lo
        ? cap.start_ms
        : Math.min(Math.max(cap.start_ms + deltaMs, lo), hi);
    const applied = clamped - cap.start_ms;
    if (applied === 0) return project;
    const captions = project.captions.map((c, i) => {
      if (i !== idx) return c;
      return {
        ...c,
        start_ms: c.start_ms + applied,
        end_ms: c.end_ms + applied,
        words: c.words.map((w) => ({
          ...w,
          start_ms: w.start_ms + applied,
          end_ms: w.end_ms + applied,
        })),
      };
    });
    return { ...project, captions };
  }
  function resizeCaption(
    project: Project,
    captionId: string,
    newStartMs: number,
    newEndMs: number,
  ): Project {
    if (newStartMs >= newEndMs) throw err("start must be less than end");
    const idx = findCaption(project, captionId);
    const cap = project.captions[idx];
    const prevEnd = idx > 0 ? project.captions[idx - 1].end_ms : 0;
    const nextStart = project.captions[idx + 1]?.start_ms;
    const wordsLo = cap.words[0]?.start_ms;
    const wordsHi = cap.words[cap.words.length - 1]?.end_ms;
    // Start edge: clamped to prev caption end / 0, can't pass first word start.
    let start = Math.max(newStartMs, prevEnd, 0);
    if (wordsLo !== undefined) start = Math.min(start, wordsLo);
    // End edge: clamped to next caption start, can't shrink past last word end.
    let end = newEndMs;
    if (nextStart !== undefined) end = Math.min(end, nextStart);
    if (wordsHi !== undefined) end = Math.max(end, wordsHi);
    if (start >= end) throw err("resize leaves the caption with no duration");
    const captions = project.captions.map((c, i) =>
      i === idx ? { ...c, start_ms: start, end_ms: end } : c,
    );
    return { ...project, captions };
  }
  function shiftAllCaptions(project: Project, offsetMs: number): Project {
    if (offsetMs === 0) return project;
    const captions = project.captions.map((c) => ({
      ...c,
      start_ms: Math.max(c.start_ms + offsetMs, 0),
      end_ms: Math.max(c.end_ms + offsetMs, 0),
      words: c.words.map((w) => ({
        ...w,
        start_ms: Math.max(w.start_ms + offsetMs, 0),
        end_ms: Math.max(w.end_ms + offsetMs, 0),
      })),
    }));
    return { ...project, captions };
  }

  // ── NLE timeline ops (mirror services/operations.rs timeline surface) ──
  // Faithful enough to drive the multi-lane UI: each returns a plausibly-mutated
  // Project (append/modify the relevant array), matching how ipc.timeline.* send
  // camelCase args. New entity ids are minted server-side (`nle-N`), mirroring
  // the Rust ops. The point is the wiring — command name + arg shape + the
  // store round-trip that re-renders the lanes — not the exact clamp maths.
  const nleId = () => `nle-${nextId++}`;
  const media = (p: Project): MediaItem[] => p.media ?? [];
  const tracks = (p: Project): Track[] => p.tracks ?? [];
  const items = (p: Project): TimelineItem[] => p.timeline_items ?? [];
  const basename = (path: string) => path.split(/[\\/]/).pop() || path;
  const identityTransform = (): Transform => ({
    x: 0,
    y: 0,
    scale: 1,
    rotation_deg: 0,
    opacity: 1,
    crop: null,
  });
  const findItem = (p: Project, id: string): TimelineItem => {
    const it = items(p).find((i) => i.id === id);
    if (!it) throw err(`timeline item ${id} not found`);
    return it;
  };
  /** Timeline span (ms) of a clip: start .. start + source-length / speed. */
  const itemSpan = (it: TimelineItem) => {
    const start = it.timeline_start_ms;
    const end = start + (it.out_ms - it.in_ms) / Math.max(0.01, it.speed);
    return { start, end };
  };

  function importMedia(project: Project, path: string): Project {
    const item: MediaItem = {
      id: nleId(),
      path,
      content_hash: `hash-${path}`,
      kind: /\.(mp3|wav|m4a|flac|ogg)$/i.test(path) ? "audio_only" : "video",
      duration_ms: 12_000,
      width: 1920,
      height: 1080,
      fps: 30,
      has_audio: true,
      audio_wav_path: null,
      original_filename: basename(path),
      added_at: 0,
    };
    return { ...project, media: [...media(project), item] };
  }
  function removeMedia(project: Project, mediaId: string): Project {
    if (items(project).some((i) => i.source_media_id === mediaId)) {
      throw err(`media ${mediaId} is still referenced by a timeline item`);
    }
    return {
      ...project,
      media: media(project).filter((m) => m.id !== mediaId),
    };
  }
  /** Renumber tracks densely by their current order (mirrors reorder/remove). */
  function renumber(list: Track[]): Track[] {
    return list.map((tk, i) => ({ ...tk, index: i }));
  }
  function addTrack(
    project: Project,
    kind: Track["kind"],
    name: string,
  ): Project {
    const track: Track = {
      id: nleId(),
      kind,
      name,
      index: tracks(project).length,
      enabled: true,
      locked: false,
      muted: false,
      solo: false,
    };
    return { ...project, tracks: [...tracks(project), track] };
  }
  function removeTrack(project: Project, trackId: string): Project {
    if (items(project).some((i) => i.track_id === trackId)) {
      throw err(`track ${trackId} is not empty`);
    }
    const kept = tracks(project).filter((tk) => tk.id !== trackId);
    return { ...project, tracks: renumber(kept) };
  }
  function reorderTrack(
    project: Project,
    trackId: string,
    newIndex: number,
  ): Project {
    const list = [...tracks(project)].sort((a, b) => a.index - b.index);
    const from = list.findIndex((tk) => tk.id === trackId);
    if (from < 0) throw err(`track ${trackId} not found`);
    const [moved] = list.splice(from, 1);
    const to = Math.max(0, Math.min(newIndex, list.length));
    list.splice(to, 0, moved);
    return { ...project, tracks: renumber(list) };
  }
  function setTrackFlags(
    project: Project,
    trackId: string,
    flags: {
      enabled: boolean | null;
      locked: boolean | null;
      muted: boolean | null;
      solo: boolean | null;
    },
  ): Project {
    const next = tracks(project).map((tk) =>
      tk.id === trackId
        ? {
            ...tk,
            enabled: flags.enabled ?? tk.enabled,
            locked: flags.locked ?? tk.locked,
            muted: flags.muted ?? tk.muted,
            solo: flags.solo ?? tk.solo,
          }
        : tk,
    );
    return { ...project, tracks: next };
  }
  function addTimelineItem(
    project: Project,
    trackId: string,
    sourceMediaId: string | null,
    inMs: number,
    outMs: number,
    timelineStartMs: number,
    kind: TimelineItem["kind"],
  ): Project {
    const item: TimelineItem = {
      id: nleId(),
      track_id: trackId,
      kind,
      source_media_id: sourceMediaId,
      in_ms: inMs,
      out_ms: outMs,
      timeline_start_ms: timelineStartMs,
      speed: 1,
      transform: identityTransform(),
      effects: [],
      transition_in: null,
      text: null,
      enabled: true,
      locked: false,
    };
    return { ...project, timeline_items: [...items(project), item] };
  }
  function splitTimelineItem(
    project: Project,
    itemId: string,
    atTimelineMs: number,
  ): Project {
    const orig = findItem(project, itemId);
    const { start, end } = itemSpan(orig);
    if (atTimelineMs <= start || atTimelineMs >= end) {
      throw err(`split at ${atTimelineMs} outside the clip`);
    }
    const sourceCut = orig.in_ms + (atTimelineMs - start) * orig.speed;
    const left = { ...orig, out_ms: sourceCut };
    const right = {
      ...orig,
      id: nleId(),
      in_ms: sourceCut,
      timeline_start_ms: atTimelineMs,
    };
    const arr = items(project).slice();
    const i = arr.findIndex((it) => it.id === itemId);
    arr.splice(i, 1, left, right);
    return { ...project, timeline_items: arr };
  }
  function trimTimelineItem(
    project: Project,
    itemId: string,
    edges: {
      newInMs: number | null;
      newOutMs: number | null;
      newTimelineStartMs: number | null;
    },
  ): Project {
    const next = items(project).map((it) =>
      it.id === itemId
        ? {
            ...it,
            in_ms: edges.newInMs ?? it.in_ms,
            out_ms: edges.newOutMs ?? it.out_ms,
            timeline_start_ms: edges.newTimelineStartMs ?? it.timeline_start_ms,
          }
        : it,
    );
    return { ...project, timeline_items: next };
  }
  function moveTimelineItem(
    project: Project,
    itemId: string,
    newTrackId: string,
    newTimelineStartMs: number,
  ): Project {
    const next = items(project).map((it) =>
      it.id === itemId
        ? {
            ...it,
            track_id: newTrackId,
            timeline_start_ms: Math.max(0, newTimelineStartMs),
          }
        : it,
    );
    return { ...project, timeline_items: next };
  }
  /**
   * Close every gap on a track (mirrors services::timeline_ops::pack_track):
   * clips slide back against their predecessor, left to right; a locked clip
   * is an anchor — it keeps its timecode and the clips after it pack against
   * its end instead of sliding past it.
   */
  function packTrack(project: Project, trackId: string): Project {
    const onTrack = items(project)
      .filter((it) => it.track_id === trackId)
      .sort((a, b) =>
        a.timeline_start_ms !== b.timeline_start_ms
          ? a.timeline_start_ms - b.timeline_start_ms
          : a.id.localeCompare(b.id),
      );
    let cursor = 0;
    const starts = new Map<string, number>();
    for (const it of onTrack) {
      const { start, end } = itemSpan(it);
      if (it.locked) {
        cursor = Math.max(cursor, end);
        continue;
      }
      const dur = end - start;
      const newStart = Math.min(cursor, start);
      starts.set(it.id, newStart);
      cursor = newStart + dur;
    }
    const next = items(project).map((it) =>
      starts.has(it.id) ? { ...it, timeline_start_ms: starts.get(it.id)! } : it,
    );
    return { ...project, timeline_items: next };
  }
  function rippleDeleteItem(project: Project, itemId: string): Project {
    const gone = findItem(project, itemId);
    const gap = itemSpan(gone).end - itemSpan(gone).start;
    const next = items(project)
      .filter((it) => it.id !== itemId)
      .map((it) =>
        it.track_id === gone.track_id &&
        it.timeline_start_ms > gone.timeline_start_ms
          ? { ...it, timeline_start_ms: it.timeline_start_ms - gap }
          : it,
      );
    return { ...project, timeline_items: next };
  }
  function setTransition(
    project: Project,
    itemId: string,
    kind: string,
    durationMs: number,
  ): Project {
    const next = items(project).map((it) =>
      it.id === itemId
        ? { ...it, transition_in: { kind, duration_ms: durationMs } }
        : it,
    );
    return { ...project, timeline_items: next };
  }
  function clearTransition(project: Project, itemId: string): Project {
    const next = items(project).map((it) =>
      it.id === itemId ? { ...it, transition_in: null } : it,
    );
    return { ...project, timeline_items: next };
  }
  function setTransform(
    project: Project,
    itemId: string,
    transform: Transform,
  ): Project {
    const next = items(project).map((it) =>
      it.id === itemId ? { ...it, transform } : it,
    );
    return { ...project, timeline_items: next };
  }
  /**
   * Mirror of `timeline_ops::set_effect` (E6): one entry per KIND, params
   * clamped to the curated registry's ranges, a non-curated kind rejected.
   */
  function setEffect(
    project: Project,
    itemId: string,
    kind: string,
    params: Record<string, number>,
    enabled: boolean,
  ): Project {
    // Inlined, not imported: this whole function is serialised into the page,
    // so it may not reference anything outside itself. Kept in step with
    // src/features/timeline/effects/registry.ts (and its Rust mirror).
    const curated: Array<{
      id: string;
      params: Array<{
        name: string;
        min: number;
        max: number;
        default: number;
      }>;
    }> = [
      {
        id: "brightness",
        params: [{ name: "amount", min: -1, max: 1, default: 0 }],
      },
      {
        id: "contrast",
        params: [{ name: "amount", min: 0, max: 3, default: 1 }],
      },
      {
        id: "saturation",
        params: [{ name: "amount", min: 0, max: 3, default: 1 }],
      },
      { id: "grayscale", params: [] },
    ];
    const def = curated.find((d) => d.id === kind);
    if (!def)
      throw new Error(`validation: effect kind \`${kind}\` is not curated`);
    const clean: Record<string, number> = {};
    for (const pd of def.params) {
      const raw = params?.[pd.name];
      const n =
        typeof raw === "number" && Number.isFinite(raw) ? raw : pd.default;
      clean[pd.name] = Math.min(pd.max, Math.max(pd.min, n));
    }
    const next = items(project).map((it) => {
      if (it.id !== itemId) return it;
      const others = it.effects.filter((e) => e.kind !== kind);
      return {
        ...it,
        effects: [
          ...others,
          { id: `fx-${kind}`, kind, params: clean, enabled },
        ],
      };
    });
    return { ...project, timeline_items: next };
  }
  function removeEffect(
    project: Project,
    itemId: string,
    kind: string,
  ): Project {
    const next = items(project).map((it) =>
      it.id === itemId
        ? { ...it, effects: it.effects.filter((e) => e.kind !== kind) }
        : it,
    );
    return { ...project, timeline_items: next };
  }
  function addTextItem(
    project: Project,
    trackId: string,
    timelineStartMs: number,
    durationMs: number,
    text: string,
  ): Project {
    const item: TimelineItem = {
      id: nleId(),
      track_id: trackId,
      kind: "text",
      source_media_id: null,
      in_ms: 0,
      out_ms: durationMs,
      timeline_start_ms: timelineStartMs,
      speed: 1,
      transform: identityTransform(),
      effects: [],
      transition_in: null,
      text: { text, style_id: null },
      enabled: true,
      locked: false,
    };
    return { ...project, timeline_items: [...items(project), item] };
  }

  /**
   * Every `compose_preview_proxy` call the app made, in order. Exposed on
   * `window.__mockProxyRenders` so a spec can assert that the "render
   * preview" button reached the backend with a real output path instead of
   * merely turning green.
   */
  const proxyRenders: Array<{ output: string; items: number }> = [];

  /**
   * Commands the app fires on boot (or on a surface the specs never assert)
   * whose real answer nothing in the suite depends on. They resolve empty —
   * everything else that has no case REJECTS, so a spec cannot pass by
   * exercising a command that does not exist.
   *
   * Keep this list short and justified. If a spec starts depending on one of
   * these, give it a real case instead of leaving it here.
   */
  const BOOT_NOOPS = new Set<string>([
    // Model manager: the models dir is empty in the browser build, so an empty
    // answer is the honest one. `asr_downloaded_models` has a real case below.
    "asr_model_dir",
    // Updater plugin — no release feed in `vite preview`.
    "plugin:updater|check",
    "plugin:updater|download_and_install",
    // Event bus / deep-link bridge: the specs never emit a Tauri event, so
    // registering a listener is a no-op that must not throw.
    "plugin:event|listen",
    "plugin:event|unlisten",
    "plugin:event|emit",
    // Window + process plugins the shell touches (close guard, relaunch).
    "plugin:window|create",
    "plugin:process|restart",
  ]);

  type Args = Record<string, unknown>;
  function invoke(cmd: string, args: Args): Promise<unknown> {
    const project = args.project as Project;
    switch (cmd) {
      case "op_split_caption":
        return Promise.resolve(
          splitCaption(
            project,
            args.captionId as string,
            args.atWordIndex as number,
          ),
        );
      case "op_merge_captions":
        return Promise.resolve(
          mergeCaptions(project, args.captionIds as string[]),
        );
      case "op_edit_word":
        return Promise.resolve(
          editWord(
            project,
            args.captionId as string,
            args.wordIndex as number,
            args.newText as string,
          ),
        );
      case "op_lock_word":
        return Promise.resolve(
          lockWord(
            project,
            args.captionId as string,
            args.wordIndex as number,
            args.locked as boolean,
          ),
        );
      case "op_accept_alternate":
        return Promise.resolve(
          acceptAlternate(
            project,
            args.captionId as string,
            args.wordIndex as number,
            args.alternateIndex as number,
          ),
        );
      case "op_retime_word":
        return Promise.resolve(
          retimeWord(
            project,
            args.captionId as string,
            args.wordIndex as number,
            args.newStartMs as number,
            args.newEndMs as number,
          ),
        );
      case "op_move_caption":
        return Promise.resolve(
          moveCaption(
            project,
            args.captionId as string,
            args.deltaMs as number,
          ),
        );
      case "op_resize_caption":
        return Promise.resolve(
          resizeCaption(
            project,
            args.captionId as string,
            args.newStartMs as number,
            args.newEndMs as number,
          ),
        );
      case "op_shift_all_captions":
        return Promise.resolve(
          shiftAllCaptions(project, args.offsetMs as number),
        );
      case "op_apply_glossary":
        return Promise.resolve({ project, corrections: [] });
      case "export_srt":
        return Promise.resolve(exportSrt(project, args.stripEmpty !== false));
      case "export_vtt":
        return Promise.resolve(exportVtt(project, args.stripEmpty !== false));
      case "export_ass":
        return Promise.resolve(exportAss(project));
      case "export_json":
        return Promise.resolve(exportJson(project, args.stripEmpty !== false));
      case "export_txt":
        return Promise.resolve(
          project.captions.map(captionText).join(" ").trim(),
        );
      case "export_list_presets":
        // One landscape + one vertical preset so the burn-in detail/preview
        // pane (and preset-toggle behaviour) is reachable from E2E.
        return Promise.resolve([
          {
            id: "youtube_16x9",
            name: "YouTube",
            description: "Landscape 16:9",
            aspect: "landscape",
            width: 1920,
            height: 1080,
            max_duration_sec: null,
            codec: "h264",
            bitrate_kbps: 8000,
            also_srt_sidecar: false,
          },
          {
            id: "reels_9x16",
            name: "Reels",
            description: "Vertical 9:16",
            aspect: "portrait",
            width: 1080,
            height: 1920,
            max_duration_sec: 90,
            codec: "h264",
            bitrate_kbps: 6000,
            also_srt_sidecar: true,
          },
        ]);
      case "export_validate":
        return Promise.resolve([]);

      // ── NLE timeline / clip-track ops (mirror ipc.timeline.*) ──
      case "op_import_media":
        return Promise.resolve(importMedia(project, args.path as string));
      case "op_remove_media":
        return Promise.resolve(removeMedia(project, args.mediaId as string));
      case "op_add_track":
        return Promise.resolve(
          addTrack(project, args.kind as Track["kind"], args.name as string),
        );
      case "op_remove_track":
        return Promise.resolve(removeTrack(project, args.trackId as string));
      case "op_reorder_track":
        return Promise.resolve(
          reorderTrack(
            project,
            args.trackId as string,
            args.newIndex as number,
          ),
        );
      case "op_set_track_flags":
        return Promise.resolve(
          setTrackFlags(project, args.trackId as string, {
            enabled: (args.enabled as boolean | null) ?? null,
            locked: (args.locked as boolean | null) ?? null,
            muted: (args.muted as boolean | null) ?? null,
            solo: (args.solo as boolean | null) ?? null,
          }),
        );
      case "op_add_timeline_item":
        return Promise.resolve(
          addTimelineItem(
            project,
            args.trackId as string,
            (args.sourceMediaId as string | null) ?? null,
            args.inMs as number,
            args.outMs as number,
            args.timelineStartMs as number,
            args.kind as TimelineItem["kind"],
          ),
        );
      case "op_split_timeline_item":
        return Promise.resolve(
          splitTimelineItem(
            project,
            args.itemId as string,
            args.atTimelineMs as number,
          ),
        );
      case "op_trim_timeline_item":
        return Promise.resolve(
          trimTimelineItem(project, args.itemId as string, {
            newInMs: (args.newInMs as number | null) ?? null,
            newOutMs: (args.newOutMs as number | null) ?? null,
            newTimelineStartMs:
              (args.newTimelineStartMs as number | null) ?? null,
          }),
        );
      case "op_move_timeline_item":
        return Promise.resolve(
          moveTimelineItem(
            project,
            args.itemId as string,
            args.newTrackId as string,
            args.newTimelineStartMs as number,
          ),
        );
      case "op_ripple_delete_item":
        return Promise.resolve(
          rippleDeleteItem(project, args.itemId as string),
        );
      case "op_pack_track":
        return Promise.resolve(packTrack(project, args.trackId as string));
      case "op_set_transition":
        return Promise.resolve(
          setTransition(
            project,
            args.itemId as string,
            args.kind as string,
            args.durationMs as number,
          ),
        );
      case "op_clear_transition":
        return Promise.resolve(clearTransition(project, args.itemId as string));
      case "op_set_transform":
        return Promise.resolve(
          setTransform(
            project,
            args.itemId as string,
            args.transform as Transform,
          ),
        );
      case "op_set_effect":
        return Promise.resolve(
          setEffect(
            project,
            args.itemId as string,
            args.kind as string,
            args.params as Record<string, number>,
            args.enabled as boolean,
          ),
        );
      case "op_remove_effect":
        return Promise.resolve(
          removeEffect(project, args.itemId as string, args.kind as string),
        );
      case "op_add_text_item":
        return Promise.resolve(
          addTextItem(
            project,
            args.trackId as string,
            args.timelineStartMs as number,
            args.durationMs as number,
            args.text as string,
          ),
        );

      // ── project creation (mirrors commands/project.rs::project_create_from_video
      //    + the shared Project::backfill_default_timeline) ──
      case "project_create_from_video": {
        // A fresh import lands with the multi-track shape already backfilled:
        // one media item from the probe scalars, a Video/Caption track pair,
        // and the video placed as ONE full-length clip at timeline 0.
        const path = args.path as string;
        const durationMs = 12_000; // matches the video_probe mock
        const mediaItem: MediaItem = {
          id: nleId(),
          path,
          content_hash: `hash-${path}`,
          kind: "video",
          duration_ms: durationMs,
          width: 1920,
          height: 1080,
          fps: 30,
          has_audio: true,
          audio_wav_path: null,
          original_filename: basename(path),
          added_at: 0,
        };
        const videoTrack: Track = {
          id: nleId(),
          kind: "video",
          name: "Video",
          index: 0,
          enabled: true,
          locked: false,
          muted: false,
          solo: false,
        };
        const captionTrack: Track = {
          id: nleId(),
          kind: "caption",
          name: "Captions",
          index: 1,
          enabled: true,
          locked: false,
          muted: false,
          solo: false,
        };
        const placed: TimelineItem = {
          id: nleId(),
          track_id: videoTrack.id,
          kind: "av",
          source_media_id: mediaItem.id,
          in_ms: 0,
          out_ms: durationMs,
          timeline_start_ms: 0,
          speed: 1,
          transform: identityTransform(),
          effects: [],
          transition_in: null,
          text: null,
          enabled: true,
          locked: false,
        };
        return Promise.resolve({
          // Scalar fields the app reads off a fresh project (subset of the
          // real Project — the mock's job is the wiring, not completeness).
          id: nleId(),
          name: basename(path),
          video_path: path,
          video_content_hash: mediaItem.content_hash,
          video_duration_ms: durationMs,
          video_width: 1920,
          video_height: 1080,
          video_fps: 30,
          audio_wav_path: null,
          language: "auto",
          captions: [],
          speakers: [],
          glossary: [],
          clips: [],
          talk_summary: null,
          media: [mediaItem],
          tracks: [videoTrack, captionTrack],
          timeline_items: [placed],
          created_at: 0,
          updated_at: 0,
        });
      }

      // ── missing-media detection + relink (Round: relink media) ──
      // `exists` is a pure convention over the mock's fake filesystem: any
      // path under `/missing/` is "gone", everything else (`/demo/…`,
      // freshly-picked paths, …) is "present". Real fs stats happen only in
      // the Rust backend.
      case "check_media_paths":
        return Promise.resolve(
          media(project).map((m) => ({
            media_id: m.id,
            path: m.path,
            exists: !/^\/missing\//i.test(m.path),
          })),
        );
      case "project_relink":
        // The mock has no real filesystem to search — always report "not
        // found automatically" so the renderer's dialog fallback is what
        // actually drives the relink in E2E (see `plugin:dialog|open` below,
        // which honours `window.__mockDialogPath` for exactly this).
        return Promise.resolve(null);
      case "op_relink_media": {
        const mediaId = args.mediaId as string;
        const newPath = args.newPath as string;
        const idx = media(project).findIndex((m) => m.id === mediaId);
        if (idx < 0) throw err(`media ${mediaId} not found`);
        // A path containing "short" simulates a re-probe that comes back with
        // a different (shorter) duration — the renderer's "timings may no
        // longer line up" warning path.
        const newDuration = /short/i.test(newPath) ? 4_000 : 12_000;
        const nextMedia = media(project).slice();
        nextMedia[idx] = {
          ...nextMedia[idx],
          path: newPath,
          content_hash: `hash-${newPath}`,
          original_filename: basename(newPath),
          duration_ms: newDuration,
        };
        return Promise.resolve({ ...project, media: nextMedia });
      }

      // ── media import dialog + probe ──
      case "accepted_media_extensions":
        return Promise.resolve([
          "mp4",
          "mov",
          "mkv",
          "webm",
          "mp3",
          "wav",
          "m4a",
        ]);
      case "plugin:dialog|open":
        // The media-bin Import button (and the relink flow's manual-pick
        // fallback) opens this picker. A spec can steer the returned path via
        // `window.__mockDialogPath` (reset to the default `/demo/broll.mp4`
        // import path otherwise) — set it before clicking to exercise a
        // specific relink target, e.g. a `/missing/…` sentinel or a `short`
        // one for the duration-changed warning.
        return Promise.resolve(
          (window as unknown as { __mockDialogPath?: string })
            .__mockDialogPath ?? "/demo/broll.mp4",
        );
      case "plugin:dialog|save":
        // The compose-export "save as" picker; a deterministic output path lets
        // the real button drive `compose_render` end-to-end.
        return Promise.resolve("/demo/out.mp4");
      case "extract_thumbnail":
        // Thumbnail grabs write a JPEG and return its path — echo the
        // requested outPath (or a stable fake) without any ffmpeg.
        return Promise.resolve(
          (args.outPath as string | undefined) ?? "/demo/thumb.jpg",
        );
      case "video_probe":
        return Promise.resolve({
          duration_ms: 12_000,
          width: 1920,
          height: 1080,
          fps: 30,
          video_codec: "h264",
          audio_codec: "aac",
          audio_channels: 2,
          audio_sample_rate: 48_000,
          container: "mp4",
          kind: "video",
        });

      // ── compose / render engine ──
      case "compose_render": {
        // Emit a couple of progress ticks then resolve. Nothing in-app listens
        // yet (the compose UI lands later), so the ticks are surfaced as window
        // CustomEvents — observable from a spec without reimplementing Tauri's
        // event bus.
        const emit = (fraction: number, done: boolean) =>
          window.dispatchEvent(
            new CustomEvent("compose-render-progress", {
              detail: {
                out_ms: Math.round(fraction * 12_000),
                total_ms: 12_000,
                fraction,
                frame: Math.round(fraction * 360),
                done,
              },
            }),
          );
        return new Promise<void>((resolve) => {
          setTimeout(() => emit(0.5, false), 0);
          setTimeout(() => {
            emit(1, true);
            resolve();
          }, 10);
        });
      }
      case "compose_cancel":
        return Promise.resolve(undefined);
      case "compose_preview_proxy":
        // The preview-render proxy: real ffmpeg flattens the timeline to a
        // low-res mp4 at `output`. There is nothing to encode here, but the
        // command MUST be answered — `renderPreviewProxy` resolving true is
        // what makes the timeline announce "Preview rendered".
        //
        // This case did not exist while `default:` resolved undefined, so the
        // announcement was made for a file that was never written and no spec
        // could tell. The call is recorded so a spec can assert the button
        // actually reached the backend with a real output path.
        proxyRenders.push({
          output: args.output as string,
          items: items(project).length,
        });
        return Promise.resolve(undefined);

      // ── path plugin ──
      // `appCacheDir()` / `appDataDir()` / `join()` / `dirname()`. Real
      // answers, not empty ones: the preview-proxy button and the model
      // manager BUILD paths from these, and a path of `undefined` is how the
      // proxy came to be "rendered" to nowhere.
      case "plugin:path|resolve_directory":
        return Promise.resolve("/demo/appdir");
      case "plugin:path|join":
        return Promise.resolve((args.paths as string[]).join("/"));
      case "plugin:path|resolve":
        return Promise.resolve((args.paths as string[]).join("/"));
      case "plugin:path|normalize":
        return Promise.resolve(args.path as string);
      case "plugin:path|dirname":
        return Promise.resolve(
          (args.path as string).split("/").slice(0, -1).join("/") || "/",
        );
      case "plugin:path|basename":
        return Promise.resolve(
          (args.path as string).split("/").pop() ?? (args.path as string),
        );
      case "plugin:path|extname": {
        const base = (args.path as string).split("/").pop() ?? "";
        const dot = base.lastIndexOf(".");
        return Promise.resolve(dot > 0 ? base.slice(dot + 1) : "");
      }

      // ── ASR model manager ──
      case "asr_downloaded_models":
        // The browser build has no models directory; an empty set is the
        // honest answer, and the model picker renders its "not downloaded"
        // state from it.
        return Promise.resolve([]);

      // ── cost estimates (pure, no network) ──
      // Every AI panel previews its scope + cost before spending. They are
      // pure functions of the caption count in Rust, so the mock answers the
      // same way for all of them — a shared shape, not a per-panel fiction.
      case "glossary_suggest_estimate":
      case "polish_estimate":
      case "suggest_estimate":
      case "translate_estimate":
      case "clips_estimate":
        return Promise.resolve({
          caption_count: project.captions.length,
          estimated_input_tokens: project.captions.length * 40,
          estimated_output_tokens: project.captions.length * 40,
          estimated_cost_usd: 0.0123,
          model_id: (args.model as string) ?? "haiku45",
        });

      default:
        // A command with no case is a BUG in the spec or in the mock, not
        // something to paper over.
        //
        // This used to be `return Promise.resolve(undefined)`. With 46 arms
        // against 107 registered Tauri commands, that made the E2E layer
        // structurally unable to fail on the thing it exists to check: a spec
        // driving a renamed, removed, or mistyped command got `undefined`,
        // the UI read it as a falsy-but-fine result, and the spec passed
        // green. `compose_preview_proxy` was the live instance — no case, so
        // `renderPreviewProxy` resolved true and the UI announced "Preview
        // rendered" for a file that did not exist.
        //
        // Genuine no-ops (app-boot probes the workflows don't depend on) are
        // listed explicitly instead, so adding one is a deliberate act.
        if (BOOT_NOOPS.has(cmd)) return Promise.resolve(undefined);
        console.warn("MOCK-UNHANDLED " + cmd);
        return Promise.reject(
          new Error(
            `mock-backend: unhandled command "${cmd}". Add a case to ` +
              `tests/e2e/fixtures/mock-backend.ts (or to BOOT_NOOPS if it is ` +
              `a boot-time probe the specs do not depend on).`,
          ),
        );
    }
  }

  const w = window as unknown as {
    __TAURI_INTERNALS__: unknown;
    __mockProxyRenders: Array<{ output: string; items: number }>;
  };
  w.__mockProxyRenders = proxyRenders;
  w.__TAURI_INTERNALS__ = {
    invoke,
    transformCallback: (cb: unknown) => cb,
    convertFileSrc: (path: string) => path,
    unregisterCallback: () => {},
  };
}

/**
 * Install the mock backend + a deterministic locale/onboarding state, then
 * load the app and click into the bundled demo project. Leaves the page in the
 * editor shell, ready for workflow assertions.
 */
export async function openDemoProject(
  page: Page,
  options: { tauri?: boolean } = {},
): Promise<void> {
  await page.addInitScript(backend);
  await page.addInitScript(() => {
    localStorage.setItem("sundayedit.onboarded", "1");
    localStorage.setItem("sundayedit.locale", "no");
  });
  // Opt-in: mark the window as a Tauri host so `isTauri()`-guarded surfaces
  // (compose export, preview render, native pickers) render + run. Default off,
  // so the browser-only specs keep exercising the graceful-degradation paths.
  if (options.tauri) {
    await page.addInitScript(() => {
      (window as unknown as { isTauri: boolean }).isTauri = true;
    });
  }
  await page.goto("/");
  await page.getByRole("button", { name: /utforsk demo-prosjektet/i }).click();
  // The editor heading confirms we're in the shell, not the import screen.
  await page
    .getByRole("heading", { name: "Editor" })
    .waitFor({ state: "visible" });
}
