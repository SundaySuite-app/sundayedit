/**
 * App-wide "some source file moved" banner — Round: relink media.
 *
 * MediaBin already flags individual rows, but the bin is just one of several
 * dock tools and is easy to never open. This banner is the surface that
 * catches the user regardless of which tool is focused: it counts every
 * missing pooled media item and offers the SAME relink flow the bin's rows
 * use (`useRelinkMedia`), run for every missing item in turn. Dismissible,
 * but re-appears if the missing SET changes afterwards (a different/further
 * file goes missing) rather than staying hidden forever.
 */
import { useEffect, useState } from "react";
import { AlertTriangle, X } from "lucide-react";

import type { Project } from "@/lib/bindings";
import { useT } from "@/lib/i18n";
import { useMediaAvailability } from "./useMediaAvailability";
import { useRelinkMedia } from "./relink";

export function MissingMediaBanner({ project }: { project: Project }) {
  const t = useT();
  const { availability } = useMediaAvailability(project);
  const { relink, statusById } = useRelinkMedia();
  const [dismissed, setDismissed] = useState(false);
  const [running, setRunning] = useState(false);

  const missing = availability.filter((a) => !a.exists);
  const missingKey = missing
    .map((a) => a.media_id)
    .sort()
    .join(",");

  // A dismissal only covers the SITUATION the user saw — if the missing set
  // changes afterwards (another file goes missing, or a relink clears one but
  // leaves others), the banner is worth showing again.
  useEffect(() => {
    setDismissed(false);
  }, [missingKey]);

  if (missing.length === 0 || dismissed) return null;

  async function relinkAll() {
    setRunning(true);
    try {
      for (const row of missing) {
        const media = project.media.find((m) => m.id === row.media_id);
        if (media) await relink(media);
      }
    } finally {
      setRunning(false);
    }
  }

  const anyError = missing.some(
    (row) => statusById[row.media_id]?.phase === "error",
  );

  return (
    <div
      role="alert"
      data-testid="missing-media-banner"
      className="flex shrink-0 items-center gap-3 border-b border-[var(--color-border)] bg-[var(--color-danger,#b3261e)]/10 px-4 py-2 text-[var(--text-ui-sm)] text-[var(--color-danger,#b3261e)]"
    >
      <AlertTriangle size={15} className="shrink-0" aria-hidden="true" />
      <span className="flex-1">
        {t("missingMediaBannerCount", { n: missing.length })}
        {anyError && ` ${t("relinkFailedGeneric")}`}
      </span>
      <button
        type="button"
        data-testid="missing-media-relink-all"
        onClick={() => void relinkAll()}
        disabled={running}
        className="shrink-0 rounded-md border border-current px-2.5 py-1 text-[var(--text-ui-xs)] font-medium hover:bg-[var(--color-danger,#b3261e)]/15 disabled:opacity-50"
      >
        {running ? t("relinkWorking") : t("mediaBinRelink")}
      </button>
      <button
        type="button"
        onClick={() => setDismissed(true)}
        title={t("actionClose")}
        aria-label={t("actionClose")}
        className="shrink-0 rounded p-1 hover:bg-[var(--color-danger,#b3261e)]/15"
      >
        <X size={13} />
      </button>
    </div>
  );
}
