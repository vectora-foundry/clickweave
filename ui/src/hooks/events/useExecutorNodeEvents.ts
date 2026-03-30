import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useStore } from "../../store/useAppStore";
import type { NodeVerdict } from "../../store/slices/verdictSlice";

/**
 * Subscribe to executor node lifecycle events:
 * node_started, node_completed, node_failed, state, workflow_completed, checks_completed.
 *
 * Returns an unsubscribe cleanup function for useEffect.
 */
export function useExecutorNodeEvents() {
  useEffect(() => {
    const unlisteners: (() => void)[] = [];
    let cancelled = false;

    const sub = (p: Promise<() => void>) =>
      p.then((u) => {
        if (cancelled) { u(); return; }
        unlisteners.push(u);
      }).catch((err) => {
        console.error("Failed to subscribe to executor node event:", err);
        useStore.getState().pushLog(`Critical: executor node event listener failed: ${err}`);
      });

    sub(listen<{ message: string }>("executor://log", (e) => {
      useStore.getState().pushLog(e.payload.message);
    }));
    sub(listen<{ state: string }>("executor://state", (e) => {
      const s = e.payload.state as "idle" | "running";
      useStore.getState().setExecutorState(s);
      if (s === "idle") useStore.getState().setActiveNode(null);
      if (s === "running") {
        useStore.getState().clearVerdicts();
        useStore.getState().setLastRunStatus(null);
      }
    }));
    sub(listen<{ node_id: string }>("executor://node_started", (e) => {
      useStore.getState().setActiveNode(e.payload.node_id);
      useStore.getState().pushLog(`Node started: ${e.payload.node_id}`);
    }));
    sub(listen<{ node_id: string }>("executor://node_completed", (e) => {
      useStore.getState().setActiveNode(null);
      useStore.getState().pushLog(`Node completed: ${e.payload.node_id}`);
    }));
    sub(listen<{ node_id: string; error: string }>("executor://node_failed", (e) => {
      useStore.getState().setActiveNode(null);
      useStore.getState().pushLog(`Node failed: ${e.payload.node_id} - ${e.payload.error}`);
      useStore.getState().setLastRunStatus("failed");
    }));
    sub(listen<NodeVerdict[]>("executor://checks_completed", (e) => {
      useStore.getState().setVerdicts(e.payload);
    }));
    sub(listen("executor://workflow_completed", () => {
      useStore.getState().pushLog("Workflow completed");
      useStore.getState().setExecutorState("idle");
      useStore.getState().setActiveNode(null);
      if (useStore.getState().lastRunStatus !== "failed") {
        useStore.getState().setLastRunStatus("completed");
      }
      useStore.getState().openVerdictModal();
    }));

    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
    };
  }, []);
}
