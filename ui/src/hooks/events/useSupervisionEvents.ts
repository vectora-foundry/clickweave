import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useStore } from "../../store/useAppStore";

/** Subscribe to executor supervision events. */
export function useSupervisionEvents() {
  useEffect(() => {
    const unlisteners: (() => void)[] = [];
    let cancelled = false;

    const sub = (p: Promise<() => void>) =>
      p.then((u) => {
        if (cancelled) { u(); return; }
        unlisteners.push(u);
      }).catch((err) => {
        console.error("Failed to subscribe to supervision event:", err);
        useStore.getState().pushLog(`Critical: supervision event listener failed: ${err}`);
      });

    sub(listen<{ scope: import("../../store/slices/executionSlice").SafetyScope; summary: string }>(
      "executor://supervision_passed",
      (e) => {
        const scopeLabel = e.payload.scope.kind === "skill"
          ? e.payload.scope.step_id
          : e.payload.scope.run_id;
        useStore.getState().pushLog(`Verified: ${scopeLabel} — ${e.payload.summary}`);
      },
    ));
    sub(listen<{ scope: import("../../store/slices/executionSlice").SafetyScope; finding: string; screenshot: string | null }>(
      "executor://supervision_paused",
      (e) => {
        useStore.getState().setSupervisionPause({
          scope: e.payload.scope,
          finding: e.payload.finding,
          screenshot: e.payload.screenshot ?? null,
        });
      },
    ));

    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
    };
  }, []);
}
