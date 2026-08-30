/**
 * Commit an async, Rust-clamped edit (R2 audio: clip gain/fades, track
 * volume) onto the shared undo stack, folding a burst of slider ticks into
 * ONE undo entry instead of one per tick.
 *
 * `useProjectStore.run` — the normal way an IPC-backed op lands — always
 * pushes a fresh history entry per call and has no notion of "these ten
 * ticks are one gesture" (see its regression test
 * "commits the slider's release value even when earlier ticks are still in
 * flight" in ClipInspector.test.tsx for the *value*-safety half of that
 * story). That is correct for a discrete op (trim, split, add effect) and
 * wrong for a value dragged continuously: a 100-deep undo history would be
 * evicted by one drag.
 *
 * The store's SYNCHRONOUS `commit` already solves exactly this for
 * client-only edits (`coalesceKey`, `COALESCE_WINDOW_MS`), so this hook does
 * the Rust round-trip itself and hands the *result* to `commit` instead of
 * `run` — same undo stack, same folding behaviour, now available to an edit
 * that needs the backend's clamp.
 *
 * A per-key sequence counter discards a stale response: if tick 2 is issued
 * before tick 1's round-trip returns, only tick 2's result is ever committed
 * (whichever order they resolve in) — the same "the last dispatched value
 * always wins" guarantee `run`'s in-flight queue gives every other op.
 */

import { useCallback, useRef } from "react";
import type { Project } from "@/lib/bindings";
import { useProjectStore } from "@/lib/useProjectStore";

export type CoalescedCommit = (
  key: string,
  op: (project: Project) => Promise<Project>,
) => void;

export function useCoalescedCommit(): CoalescedCommit {
  const commit = useProjectStore((s) => s.commit);
  // Keyed, not a single counter: the gain slider and the fade fields coalesce
  // independently, same as the panel text fields do via distinct `panel:*`
  // coalesceKeys.
  const seqRef = useRef<Map<string, number>>(new Map());

  return useCallback(
    (key, op) => {
      const project = useProjectStore.getState().project;
      if (!project) return;
      const seq = (seqRef.current.get(key) ?? 0) + 1;
      seqRef.current.set(key, seq);
      void op(project)
        .then((next) => {
          // Superseded by a later tick issued while this one was in flight —
          // drop it silently, exactly like `run`'s in-flight guard.
          if (seqRef.current.get(key) !== seq) return;
          commit(next, { coalesceKey: key });
        })
        .catch(() => {
          // Clamped / rejected by the backend — leave the project untouched.
        });
    },
    [commit],
  );
}
