/**
 * Capability probe for the GPU preview compositor (E6; ADR-009 §4 "gated
 * behind a runtime capability check", ADR-010, ADR-013).
 *
 * PixiJS 8 needs WebGL2 (its WebGPU path is not what ADR-010 measured, and the
 * WebGL1 fallback is not what we tuned). The whole point of the gate is that
 * SundayEdit must keep working where the GPU path cannot: a machine with a
 * blocklisted driver, a remote session with no hardware acceleration, a
 * jsdom test run. In every one of those cases the answer is "no", the flag
 * turns itself off, and the existing `<video>` preview renders exactly as it
 * always did.
 *
 * Pure and injectable — no React, no Pixi import — so the decision can be
 * tested without a GPU and without loading 157 kB of renderer.
 */

export interface CompositorCapability {
  supported: boolean;
  /** Machine-readable reason when unsupported (also the i18n discriminator). */
  reason: "ok" | "no-document" | "no-webgl2" | "context-lost" | "probe-threw";
  /** Renderer string when we could read one — useful in bug reports. */
  renderer?: string;
}

/**
 * Can this runtime host the Pixi compositor? Creates a throwaway 1×1 canvas,
 * asks for a WebGL2 context, and immediately releases it.
 *
 * Never throws: a probe that explodes is a probe that said "no".
 *
 * `doc` defaults to the ambient `document`; pass **null** (not `undefined` —
 * that is what selects the default) to assert the no-document branch.
 */
export function probeCompositorCapability(
  doc: Pick<Document, "createElement"> | null = typeof document !== "undefined"
    ? document
    : null,
): CompositorCapability {
  if (!doc) return { supported: false, reason: "no-document" };
  try {
    const canvas = doc.createElement("canvas") as HTMLCanvasElement;
    canvas.width = 1;
    canvas.height = 1;
    // `failIfMajorPerformanceCaveat` keeps us off the software rasteriser:
    // a SwiftShader-backed context "supports" WebGL2 and then composites
    // slower than the <video> path we would be replacing.
    const gl = canvas.getContext("webgl2", {
      failIfMajorPerformanceCaveat: true,
    }) as WebGL2RenderingContext | null;
    if (!gl) return { supported: false, reason: "no-webgl2" };
    if (gl.isContextLost()) return { supported: false, reason: "context-lost" };

    let renderer: string | undefined;
    try {
      const dbg = gl.getExtension("WEBGL_debug_renderer_info");
      if (dbg) {
        const value = gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL);
        if (typeof value === "string") renderer = value;
      }
    } catch {
      /* renderer name is a nicety, never a gate */
    }

    // Hand the context back rather than waiting for GC — browsers cap how many
    // live WebGL contexts a page may hold, and Pixi is about to want one.
    try {
      gl.getExtension("WEBGL_lose_context")?.loseContext();
    } catch {
      /* best effort */
    }

    return { supported: true, reason: "ok", renderer };
  } catch {
    return { supported: false, reason: "probe-threw" };
  }
}
