/**
 * GPU preview compositor (E6) — public surface.
 *
 * `PixiCompositor` is intentionally NOT re-exported here: it is loaded through
 * `React.lazy` at the one mount site (MediaPlayer) so `pixi.js` stays out of
 * the default bundle for everyone who has not turned the flag on. Importing it
 * from a barrel would undo that.
 */

export {
  probeCompositorCapability,
  type CompositorCapability,
} from "./capability";
export {
  useCompositorFlag,
  selectCompositorActive,
  type CompositorFlagState,
} from "./flag";
export {
  describeScene,
  approximationNotice,
  type CompositorScene,
  type CompositorLayer,
} from "./scene";
