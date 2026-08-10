/**
 * GPU preview compositor — a PixiJS 8 stage fed by the hidden `<video>`
 * (programme stage E6; ADR-010 chose this over WebCodecs, ADR-013 is the
 * wiring decision).
 *
 * ── What it is for ──────────────────────────────────────────────────────────
 * The `<video>` preview shows the active clip's *pixels* and nothing else: it
 * cannot apply the clip's `Transform` and it cannot apply its effects, so a
 * scaled/rotated/faded PiP or a graded clip looks like an ungraded full-frame
 * clip until you export. This component draws the same element as a texture on
 * a Pixi stage and applies the transform and the curated colour matrix, which
 * is the first time the preview and `compose.rs` describe the same picture.
 *
 * ── What it is NOT ──────────────────────────────────────────────────────────
 * It is not the export. `filter_complex` remains the truth (ADR-009), the
 * colour matrices are an RGB approximation of `vf_eq`'s YUV model (see
 * `effects/registry.ts`), crop and stacked layers are not modelled (see
 * `scene.ts`), and libass caption rendering stays with the ffmpeg proxy.
 *
 * ── Ownership seam ──────────────────────────────────────────────────────────
 * MediaPlayer keeps owning the element and the reconcile loop: this component
 * NEVER seeks, plays, pauses, loads or mutes it. Pixi's `VideoSource` would
 * happily do all of those (`autoPlay`/`autoLoad` default to true and `load()`
 * calls `source.load()`), which is exactly the kind of two-owners-one-element
 * seam bug this codebase keeps finding — so both are switched off and the
 * texture is refreshed explicitly, once per rendered frame.
 *
 * ── Failure is a fallback, never an error ───────────────────────────────────
 * Probe fails, `pixi.js` fails to load, `app.init()` throws, the WebGL context
 * is lost: every one of those calls `reportUnavailable`, unmounts the canvas,
 * and hands the frame back to the `<video>` path for the rest of the session.
 * `pixi.js` is imported dynamically so the 157 kB renderer is never fetched by
 * a user who has not turned the flag on.
 */

import { useEffect, useRef, useState } from "react";

import type { ColorMatrix } from "pixi.js";

import type { Project } from "@/lib/bindings/Project";
import { useT } from "@/lib/i18n";
import { probeCompositorCapability } from "./capability";
import { useCompositorFlag } from "./flag";
import {
  approximationNotice,
  describeScene,
  type CompositorScene,
} from "./scene";
import { getPlayheadMs } from "../playhead";

interface Props {
  /** The element MediaPlayer drives — our texture source, never our to command. */
  videoRef: React.RefObject<HTMLVideoElement | null>;
  project: Project | undefined;
  /**
   * Raised when the compositor starts drawing (true) or stops (false), so the
   * host can hide/show the `<video>`. Never called with `true` unless a frame
   * has actually been rendered.
   */
  onActiveChange: (active: boolean) => void;
}

/** Minimal structural types for the Pixi objects we touch. */
interface PixiHandles {
  destroy: () => void;
  render: (scene: CompositorScene) => void;
  canvas: HTMLCanvasElement;
}

export function PixiCompositor({ videoRef, project, onActiveChange }: Props) {
  // The canvas is appended IMPERATIVELY, so it gets a container React never
  // puts children into — mixing a hand-appended node with React-managed
  // siblings in the same parent is how "the canvas vanished when a badge
  // appeared" bugs happen.
  const hostRef = useRef<HTMLDivElement | null>(null);
  const reportUnavailable = useCompositorFlag((s) => s.reportUnavailable);
  const [mounted, setMounted] = useState(false);
  const t = useT();

  // What the CURRENT frame cannot reproduce (crop, a stacked composite). The
  // scene has always computed this; until now nothing rendered it, so the
  // preview silently drew a cropped clip uncropped and a stack as its top
  // layer — the user found out at export, which is precisely the promise this
  // product sells. The rAF loop writes it through a ref-guarded setState so a
  // steady frame costs one string compare.
  const [unsupported, setUnsupported] = useState<
    CompositorScene["unsupported"]
  >([]);
  const lastApproxKey = useRef("");

  // Read the newest project off a ref: the render loop must not restart (and
  // tear down the GL context) on every project edit.
  const projectRef = useRef(project);
  projectRef.current = project;

  // Latest callbacks, same reason.
  const onActiveRef = useRef(onActiveChange);
  onActiveRef.current = onActiveChange;
  const reportRef = useRef(reportUnavailable);
  reportRef.current = reportUnavailable;

  useEffect(() => {
    const host = hostRef.current;
    const video = videoRef.current;
    if (!host || !video) return;

    const capability = probeCompositorCapability();
    if (!capability.supported) {
      reportRef.current(capability.reason);
      return;
    }

    let disposed = false;
    let handles: PixiHandles | null = null;
    let raf = 0;

    void (async () => {
      try {
        const pixi = await import("pixi.js");
        if (disposed) return;
        handles = await createStage(pixi, video, projectRef.current);
        if (disposed || !handles) {
          handles?.destroy();
          return;
        }
        host.appendChild(handles.canvas);
        setMounted(true);
        onActiveRef.current(true);

        const tick = () => {
          if (!disposed && handles) {
            try {
              const scene = describeScene(projectRef.current, getPlayheadMs());
              handles.render(scene);
              // Tell the user when this frame is NOT what the export will
              // produce. Written to React state only when the set changes —
              // the common case is empty and costs one string compare a frame.
              const key = scene.unsupported.join(",");
              if (key !== lastApproxKey.current) {
                lastApproxKey.current = key;
                setUnsupported(scene.unsupported);
              }
            } catch {
              // A lost context (GPU reset, sleep/wake) surfaces here. Fall back
              // rather than spin on a dead renderer.
              disposed = true;
              onActiveRef.current(false);
              reportRef.current("context-lost");
              return;
            }
          }
          raf = requestAnimationFrame(tick);
        };
        raf = requestAnimationFrame(tick);
      } catch {
        if (!disposed) reportRef.current("probe-threw");
      }
    })();

    return () => {
      disposed = true;
      cancelAnimationFrame(raf);
      onActiveRef.current(false);
      setMounted(false);
      lastApproxKey.current = "";
      setUnsupported([]);
      handles?.destroy();
    };
    // Mount-scoped on purpose — the loop reads live values off refs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const notice = approximationNotice(unsupported, t);

  return (
    <div
      data-testid="gpu-compositor"
      data-mounted={mounted ? "1" : "0"}
      className="pointer-events-none relative grid h-full w-full place-items-center overflow-hidden"
    >
      <div
        ref={hostRef}
        className="grid h-full w-full place-items-center overflow-hidden"
      />
      {notice && (
        <div
          data-testid="compositor-approximate"
          className="absolute bottom-1 left-1 rounded bg-black/70 px-1.5 py-0.5 text-[var(--text-ui-xs)] text-[var(--color-fg)]"
        >
          {notice}
        </div>
      )}
    </div>
  );
}

/**
 * Build the Pixi application, the video texture and the sprite, and return a
 * per-frame `render` that applies a `CompositorScene`.
 *
 * Split out of the component (and taking the Pixi module as a parameter) so
 * the wiring is one linear function instead of a web of refs — and so a future
 * test can drive it with a fake module.
 */
async function createStage(
  pixi: typeof import("pixi.js"),
  video: HTMLVideoElement,
  project: Project | undefined,
): Promise<PixiHandles> {
  const width = project && project.video_width > 0 ? project.video_width : 1920;
  const height =
    project && project.video_height > 0 ? project.video_height : 1080;

  const app = new pixi.Application();
  await app.init({
    width,
    height,
    // The compositor replaces the picture, so the stage owns the letterbox
    // background the <video> element used to draw for us.
    background: 0x000000,
    backgroundAlpha: 1,
    antialias: false,
    // ADR-010 measured WebGL; WebGPU is a separate (unmeasured) path.
    preference: "webgl",
    autoDensity: false,
    // We drive our own rAF against the shared playhead — a second ticker would
    // render frames the playhead has not moved to.
    sharedTicker: false,
  });
  app.stop();

  // Never let Pixi command the element: MediaPlayer's reconcile loop owns
  // transport. `autoLoad:false` also keeps `VideoSource.load()` — which calls
  // `HTMLMediaElement.load()` and would reset playback — from ever running.
  const source = new pixi.VideoSource({
    resource: video,
    autoLoad: false,
    autoPlay: false,
    updateFPS: 0,
  });
  source.autoUpdate = false;

  const texture = new pixi.Texture({ source });
  const sprite = new pixi.Sprite(texture);
  // Centre anchor mirrors ffmpeg's `rotate`, which spins about the frame centre
  // and keeps the frame size; the scene's x/y stay the TOP-LEFT overlay offset,
  // so the sprite is positioned at that offset plus half its own drawn size.
  sprite.anchor.set(0.5);
  app.stage.addChild(sprite);

  const colorFilter = new pixi.ColorMatrixFilter();
  let lastMatrixKey = "";
  let lastVideoWidth = 0;
  let lastVideoHeight = 0;

  const render = (scene: CompositorScene) => {
    // Adopt the source's intrinsic size as soon as the decoder reports it —
    // with `autoLoad:false` nothing else will.
    if (
      video.videoWidth > 0 &&
      (video.videoWidth !== lastVideoWidth ||
        video.videoHeight !== lastVideoHeight)
    ) {
      lastVideoWidth = video.videoWidth;
      lastVideoHeight = video.videoHeight;
      source.resize(video.videoWidth, video.videoHeight);
    }

    if (
      app.renderer.width !== scene.width ||
      app.renderer.height !== scene.height
    ) {
      app.renderer.resize(scene.width, scene.height);
    }

    const layer = scene.layers[0];
    if (!layer || lastVideoWidth === 0) {
      sprite.visible = false;
    } else {
      sprite.visible = true;
      sprite.scale.set(layer.scale);
      sprite.rotation = layer.rotationRad;
      sprite.alpha = layer.alpha;
      sprite.position.set(
        layer.x + (lastVideoWidth * layer.scale) / 2,
        layer.y + (lastVideoHeight * layer.scale) / 2,
      );

      // Rebuild the filter list only when the matrix actually changes —
      // reassigning `filters` every frame forces Pixi to rebuild state.
      const key = layer.colorMatrix ? layer.colorMatrix.join(",") : "";
      if (key !== lastMatrixKey) {
        lastMatrixKey = key;
        if (layer.colorMatrix) {
          // Pixi types the matrix as a 20-tuple; `stackColorMatrix` always
          // returns exactly 20 numbers (asserted in registry.test.ts), so the
          // cast is a shape assertion, not a suppression.
          colorFilter.matrix = layer.colorMatrix as unknown as ColorMatrix;
          sprite.filters = [colorFilter];
        } else {
          sprite.filters = [];
        }
      }
    }

    // The element is usually PAUSED (the reconcile loop scrubs it frame by
    // frame in shuttle/step modes), and a paused video fires no frame
    // callbacks — so ask for the upload explicitly every frame instead of
    // relying on Pixi's auto-update. This is the upload ADR-010's user-agent
    // fix makes cheap: with a Safari token Pixi allocates (`texImage2D`,
    // 0.69 ms), without it, it re-uploads in place (`texSubImage2D`, 28.92 ms).
    // Guarded on a known intrinsic size: uploading a 0×0 video is a GL error.
    if (lastVideoWidth > 0) source.update();
    app.render();
  };

  return {
    canvas: app.canvas,
    render,
    destroy: () => {
      try {
        // NOTE the omitted `texture`/`textureSource` options (both default to
        // false). Destroying the texture would destroy our `VideoSource`, and
        // `VideoSource.destroy()` ends with
        //
        //     source.pause(); source.src = ""; source.load();
        //
        // on the resource — i.e. it would BLANK MediaPlayer's `<video>` on the
        // way out, and turning the flag off would leave the fallback preview
        // with no source. The element is not ours to tear down; the GL texture
        // dies with the renderer, and the JS object is garbage. (Nothing else
        // leaks either: `autoLoad:false` means `VideoSource.load()` never ran,
        // so it never attached a listener to the element.)
        app.destroy({ removeView: true }, { children: true });
      } catch {
        /* tearing down an already-dead renderer must not break unmount */
      }
    },
  };
}

export default PixiCompositor;
