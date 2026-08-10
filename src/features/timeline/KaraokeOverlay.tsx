/**
 * Karaoke caption overlay — the E4a live preview
 * (docs/OSS-INTEGRATION-PROGRAM.md §E4). Renders the caption active at the
 * playhead with per-word karaoke states (Pending/Active/Done), driven by the
 * SAME pure timing math (`./karaoke.ts`, mirroring
 * `src-tauri/src/services/karaoke.rs`) that drives the ASS `\k`/`\kf`
 * burn-in — sharing the derivation is what keeps preview and export from
 * quietly disagreeing. Reads `project.export_config.karaoke`
 * (`KaraokeOptions`) — the SAME persisted settings `write_ass` reads — via
 * `effectiveKaraokeOptions`, so a colour or threshold change here is a
 * colour or threshold change in the real export too.
 *
 * Colour mapping mirrors `export::ass_dialogue_text` exactly:
 *   - Pending  → `karaoke.pending_color` (ASS SecondaryColour) — untinted,
 *     even when confidence tinting is on: the ASS `\c` override only ever
 *     touches the PRIMARY colour, never SecondaryColour.
 *   - Done, and Active in "highlight" mode → the project's Style
 *     foreground, or `karaoke.low_confidence_color` for a word `\c`-tinted
 *     by `uncertainFlags` (mirrors `uncertain_flags`, index-aligned).
 *   - Active in "sweep" mode → a partial fill from Pending's colour to the
 *     Done colour, proportional to `wordProgress` (approximates `\kf`'s
 *     left-to-right wipe; libass renders the real thing at burn-in).
 *
 * Subscribes to the shared playhead store directly (`usePlayheadMs`,
 * `./playhead.ts`) instead of taking it as a prop: that store exists
 * precisely to scope 60 fps churn to the components that actually need it,
 * and per frame this component only does a handful of array scans over one
 * caption's words — cheap enough that no further memoization is needed.
 *
 * Absolutely-positioned DOM, not canvas: the caption box already has a CSS
 * rendering path (`styleToCss`, built for the Style editor's WYSIWYG
 * preview) that this reuses for font/position/outline/shadow/background, so
 * there is a single source of truth for "what does this Style look like"
 * instead of a second, canvas-only text-layout implementation. Perfect
 * libass parity is explicitly out of scope here (that's E4b/jassub,
 * deliberately deferred) — the goal is a faithful, useful preview.
 */

import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
} from "react";

import type { KaraokeOptions } from "@/lib/bindings/KaraokeOptions";
import type { Project } from "@/lib/bindings/Project";
import { effectiveKaraokeOptions } from "@/features/export/karaokeOptions";
import { styleToCss } from "@/lib/styleToCss";
import {
  captionAt,
  karaokeWords,
  uncertainFlags,
  wordProgress,
  wordStatesAt,
  type KaraokeWord,
  type WordState,
} from "./karaoke";
import { usePlayheadMs } from "./playhead";

interface Props {
  project: Project | undefined;
}

export function KaraokeOverlay({ project }: Props) {
  const playheadMs = usePlayheadMs();
  const frameRef = useRef<HTMLDivElement | null>(null);
  const [frameHeight, setFrameHeight] = useState(0);

  useEffect(() => {
    const el = frameRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setFrameHeight(el.clientHeight));
    ro.observe(el);
    setFrameHeight(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  const style = project?.default_style;
  // `project?.export_config` guards fixtures elsewhere in the codebase that
  // construct a partial `Project` (e.g. MediaPlayer's NLE-mapping tests cast
  // a `{ tracks, timeline_items, media }` object to `Project`) — this
  // overlay should render nothing for those, not throw.
  const karaoke = useMemo(
    () =>
      project?.export_config
        ? effectiveKaraokeOptions(project.export_config)
        : null,
    [project],
  );

  const caption = useMemo(() => {
    if (!karaoke?.enabled || !project) return null;
    return captionAt(project.captions, playheadMs);
  }, [karaoke, project, playheadMs]);

  const words = useMemo(
    () => (caption ? karaokeWords(caption) : []),
    [caption],
  );
  const states = useMemo(
    () => wordStatesAt(words, playheadMs),
    [words, playheadMs],
  );
  const tinted = useMemo(
    () =>
      caption && karaoke?.confidence_tint
        ? uncertainFlags(caption, karaoke.confidence_threshold)
        : null,
    [caption, karaoke],
  );

  // An aspect-ratio box locks the overlay to the SAME box the <video>'s
  // intrinsic ratio resolves to under `max-h-full max-w-full` in the same
  // centered grid cell (see MediaPlayer), so anchor positions line up with
  // the actual picture, not the (possibly letterboxed) player chrome.
  const aspect =
    project && project.video_width > 0 && project.video_height > 0
      ? `${project.video_width} / ${project.video_height}`
      : undefined;

  const css = useMemo(
    () => (style ? styleToCss(style, frameHeight) : null),
    [style, frameHeight],
  );

  if (!project || !style || !karaoke?.enabled || !caption || !css) return null;

  return (
    <div
      ref={frameRef}
      data-testid="karaoke-overlay"
      className="pointer-events-none h-full max-h-full w-full max-w-full"
      style={{ aspectRatio: aspect, position: "relative" }}
    >
      <div style={css.container}>
        <span style={css.text}>
          {words.map((word, i) => (
            <span key={i}>
              <KaraokeWordSpan
                word={word}
                state={states[i]}
                options={karaoke}
                tinted={tinted?.[i] ?? false}
                fallbackColor={style.color_fg}
                playheadMs={playheadMs}
              />
              {i < words.length - 1 ? " " : ""}
            </span>
          ))}
        </span>
      </div>
    </div>
  );
}

/** Done/Active("highlight")'s colour — the Style's own foreground, unless
 *  this word is confidence-tinted, matching `ass_dialogue_text`'s `\c`
 *  override (PrimaryColour only; SecondaryColour/Pending is never tinted). */
function filledColor(
  options: KaraokeOptions,
  tinted: boolean,
  fallback: string,
): string {
  return tinted ? options.low_confidence_color : fallback;
}

function KaraokeWordSpan({
  word,
  state,
  options,
  tinted,
  fallbackColor,
  playheadMs,
}: {
  word: KaraokeWord;
  state: WordState;
  options: KaraokeOptions;
  tinted: boolean;
  fallbackColor: string;
  playheadMs: number;
}) {
  const fillColor = filledColor(options, tinted, fallbackColor);

  // Done, and (in "highlight" mode) Active too — `\k` switches a syllable
  // from Secondary to Primary for its ENTIRE duration the instant the wipe
  // reaches it, rather than filling mid-word like `\kf` does.
  if (
    state === "done" ||
    (state === "active" && options.style === "highlight")
  ) {
    return (
      <span data-state={state} style={{ color: fillColor }}>
        {word.text}
      </span>
    );
  }
  if (state === "pending") {
    return (
      <span data-state={state} style={{ color: options.pending_color }}>
        {word.text}
      </span>
    );
  }

  // Active + "sweep": a partial left-to-right fill, approximating `\kf`'s
  // wipe via a clipped duplicate layer over the pending-coloured base text.
  const pct = Math.round(wordProgress(word, playheadMs) * 100);
  const fillStyle: CSSProperties = {
    position: "absolute",
    inset: 0,
    color: fillColor,
    clipPath: `inset(0 ${100 - pct}% 0 0)`,
    whiteSpace: "pre",
  };
  return (
    <span
      data-state={state}
      data-progress={pct}
      style={{ position: "relative", display: "inline-block" }}
    >
      <span style={{ color: options.pending_color }}>{word.text}</span>
      <span aria-hidden style={fillStyle}>
        {word.text}
      </span>
    </span>
  );
}
