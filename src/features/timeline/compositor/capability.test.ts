import { describe, it, expect, vi } from "vitest";

import { probeCompositorCapability } from "./capability";

/** A stand-in for `document` that hands back the canvas we control. */
function docWith(canvas: unknown): Pick<Document, "createElement"> {
  return { createElement: () => canvas } as unknown as Pick<
    Document,
    "createElement"
  >;
}

function fakeGl(overrides: Record<string, unknown> = {}) {
  return {
    isContextLost: () => false,
    getExtension: () => null,
    getParameter: () => undefined,
    ...overrides,
  };
}

describe("probeCompositorCapability", () => {
  it("says no without a document (node/worker)", () => {
    expect(probeCompositorCapability(null)).toEqual({
      supported: false,
      reason: "no-document",
    });
  });

  it("says no when WebGL2 is unavailable — which is the jsdom case", () => {
    const canvas = { getContext: () => null };
    expect(probeCompositorCapability(docWith(canvas))).toEqual({
      supported: false,
      reason: "no-webgl2",
    });
  });

  it("refuses a context with a major performance caveat", () => {
    // A SwiftShader-backed context reports WebGL2 support and then composites
    // slower than the <video> path we would be replacing, so the probe asks
    // the browser to fail rather than hand one over.
    const getContext = vi.fn(() => null);
    probeCompositorCapability(docWith({ getContext }));
    expect(getContext).toHaveBeenCalledWith("webgl2", {
      failIfMajorPerformanceCaveat: true,
    });
  });

  it("says no when the context arrives already lost", () => {
    const canvas = {
      getContext: () => fakeGl({ isContextLost: () => true }),
    };
    expect(probeCompositorCapability(docWith(canvas))).toEqual({
      supported: false,
      reason: "context-lost",
    });
  });

  it("says yes on a healthy WebGL2 context", () => {
    const canvas = { getContext: () => fakeGl() };
    const got = probeCompositorCapability(docWith(canvas));
    expect(got.supported).toBe(true);
    expect(got.reason).toBe("ok");
  });

  it("reports the renderer name when the debug extension offers one", () => {
    const canvas = {
      getContext: () =>
        fakeGl({
          getExtension: (name: string) =>
            name === "WEBGL_debug_renderer_info"
              ? { UNMASKED_RENDERER_WEBGL: 1 }
              : null,
          getParameter: () => "Apple GPU",
        }),
    };
    expect(probeCompositorCapability(docWith(canvas)).renderer).toBe(
      "Apple GPU",
    );
  });

  it("still says yes when reading the renderer name throws", () => {
    const canvas = {
      getContext: () =>
        fakeGl({
          getExtension: (name: string) => {
            if (name === "WEBGL_debug_renderer_info") throw new Error("nope");
            return null;
          },
        }),
    };
    const got = probeCompositorCapability(docWith(canvas));
    expect(got.supported).toBe(true);
    expect(got.renderer).toBeUndefined();
  });

  it("releases the probe context so Pixi can have one", () => {
    // Browsers cap live WebGL contexts per page; leaking the probe's would
    // starve the renderer we are about to start.
    const loseContext = vi.fn();
    const canvas = {
      getContext: () =>
        fakeGl({
          getExtension: (name: string) =>
            name === "WEBGL_lose_context" ? { loseContext } : null,
        }),
    };
    probeCompositorCapability(docWith(canvas));
    expect(loseContext).toHaveBeenCalled();
  });

  it("treats a throwing probe as an answer, not an exception", () => {
    const canvas = {
      getContext: () => {
        throw new Error("driver exploded");
      },
    };
    expect(probeCompositorCapability(docWith(canvas))).toEqual({
      supported: false,
      reason: "probe-threw",
    });
  });

  it("says no in this very test environment (jsdom has no WebGL2)", () => {
    // The acceptance bar in practice: nothing in the test suite can ever end
    // up on the GPU path by accident.
    expect(probeCompositorCapability().supported).toBe(false);
  });
});
