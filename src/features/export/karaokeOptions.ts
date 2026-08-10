/**
 * Karaoke options — the frontend's read side of `ExportConfig.karaoke`
 * (`KaraokeOptions`, `src-tauri/src/services/karaoke.rs`). Persisted
 * per-project so the sidecar `.ass`, the final burn-in and this preview
 * overlay all read the SAME values and cannot disagree (see that struct's
 * doc comment).
 *
 * `karaoke` is `Option<KaraokeOptions>` on the Rust side (`#[serde(default)]`
 * so pre-E4a project files stay valid) — `undefined`/`null` here means the
 * same thing as `KaraokeOptions::disabled()`. `DEFAULT_KARAOKE_OPTIONS`
 * below must stay byte-for-byte in sync with that Rust `Default` impl.
 */

import type { ExportConfig } from "@/lib/bindings/ExportConfig";
import type { KaraokeOptions } from "@/lib/bindings/KaraokeOptions";

export const DEFAULT_KARAOKE_OPTIONS: KaraokeOptions = {
  enabled: false,
  style: "highlight",
  pending_color: "#7A7A7A",
  confidence_tint: false,
  confidence_threshold: 70,
  low_confidence_color: "#F5A524",
};

/** The karaoke options a project renders with — `export_config.karaoke` if
 *  set, else the shared default (mirrors `export::project_karaoke`). */
export function effectiveKaraokeOptions(config: ExportConfig): KaraokeOptions {
  return config.karaoke ?? DEFAULT_KARAOKE_OPTIONS;
}
