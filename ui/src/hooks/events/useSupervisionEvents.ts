import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import type { WorkflowPatch } from "../../bindings";
import type { ResolutionProposal } from "../../store/slices/executionSlice";
import { useStore } from "../../store/useAppStore";

/** Subscribe to executor supervision and resolution events. */
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

    sub(listen<{ node_id: string; node_name: string; summary: string }>(
      "executor://supervision_passed",
      (e) => {
        useStore.getState().pushLog(`Verified: ${e.payload.node_name} — ${e.payload.summary}`);
      },
    ));
    sub(listen<{ node_id: string; node_name: string; finding: string; screenshot: string | null }>(
      "executor://supervision_paused",
      (e) => {
        useStore.getState().setSupervisionPause({
          nodeId: e.payload.node_id,
          nodeName: e.payload.node_name,
          finding: e.payload.finding,
          screenshot: e.payload.screenshot ?? null,
        });
      },
    ));
    sub(listen<ResolutionProposal>("executor://resolution_proposed", (e) => {
      useStore.setState({ resolutionProposal: e.payload });
    }));
    sub(listen("executor://resolution_dismissed", () => {
      useStore.setState({ resolutionProposal: null });
    }));
    sub(listen<{ node_id: string; node_name: string; reason: string; patch: WorkflowPatch }>(
      "executor://resolution_auto_approved",
      (e) => {
        const { pushLog, incrementAutoApprovedCount } = useStore.getState();
        const p = e.payload.patch;
        const counts = [
          p.added_nodes.length > 0 ? `+${p.added_nodes.length}` : null,
          p.updated_nodes.length > 0 ? `~${p.updated_nodes.length}` : null,
          p.removed_node_ids.length > 0 ? `-${p.removed_node_ids.length}` : null,
        ].filter(Boolean).join("/");
        pushLog(`Auto-approved: ${e.payload.node_name} — ${e.payload.reason} (${counts} nodes)`);
        incrementAutoApprovedCount();
        // NOTE: do NOT call applyRuntimePatch here — patch_applied handles that
      },
    ));
    sub(listen<{ patch: WorkflowPatch }>("executor://patch_applied", (e) => {
      useStore.getState().applyRuntimePatch(e.payload.patch);
      useStore.setState({ resolutionProposal: null });
    }));

    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
    };
  }, []);
}
