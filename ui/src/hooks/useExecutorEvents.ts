import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import type { ChatEntry, WorkflowPatch } from "../bindings";
import { commands } from "../bindings";
import type { ResolutionProposal } from "../store/slices/executionSlice";
import { useStore } from "../store/useAppStore";
import type { NodeVerdict } from "../store/slices/verdictSlice";
import type { WalkthroughStatus } from "../store/slices/walkthroughSlice";

/**
 * Subscribe to all Tauri `executor://`, `walkthrough://`, `menu://`,
 * `assistant://`, and `recording-bar://` backend events, dispatching
 * them into the Zustand store via `getState()` to avoid stale closures.
 *
 * Runs once on mount and tears down all listeners on unmount.
 */
export function useExecutorEvents() {
  useEffect(() => {
    const subscriptions = Promise.all([
      listen<{ message: string }>("executor://log", (e) => {
        useStore.getState().pushLog(e.payload.message);
      }),
      listen<{ state: string }>("executor://state", (e) => {
        const s = e.payload.state as "idle" | "running";
        useStore.getState().setExecutorState(s);
        if (s === "idle") useStore.getState().setActiveNode(null);
        if (s === "running") {
          useStore.getState().clearVerdicts();
          useStore.getState().setLastRunStatus(null);
        }
      }),
      listen<{ node_id: string }>("executor://node_started", (e) => {
        useStore.getState().setActiveNode(e.payload.node_id);
        useStore.getState().pushLog(`Node started: ${e.payload.node_id}`);
      }),
      listen<{ node_id: string }>("executor://node_completed", (e) => {
        useStore.getState().setActiveNode(null);
        useStore.getState().pushLog(`Node completed: ${e.payload.node_id}`);
      }),
      listen<{ node_id: string; error: string }>("executor://node_failed", (e) => {
        useStore.getState().setActiveNode(null);
        useStore.getState().pushLog(`Node failed: ${e.payload.node_id} - ${e.payload.error}`);
        useStore.getState().setLastRunStatus("failed");
      }),
      listen<NodeVerdict[]>(
        "executor://checks_completed",
        (e) => {
          useStore.getState().setVerdicts(e.payload);
        },
      ),
      listen("executor://workflow_completed", () => {
        useStore.getState().pushLog("Workflow completed");
        useStore.getState().setExecutorState("idle");
        useStore.getState().setActiveNode(null);
        if (useStore.getState().lastRunStatus !== "failed") {
          useStore.getState().setLastRunStatus("completed");
        }
        useStore.getState().openVerdictModal();
      }),
      listen<{ node_id: string; node_name: string; summary: string }>(
        "executor://supervision_passed",
        (e) => {
          useStore.getState().pushLog(`Verified: ${e.payload.node_name} — ${e.payload.summary}`);
        },
      ),
      listen<{ node_id: string; node_name: string; finding: string; screenshot: string | null }>(
        "executor://supervision_paused",
        (e) => {
          useStore.getState().setSupervisionPause({
            nodeId: e.payload.node_id,
            nodeName: e.payload.node_name,
            finding: e.payload.finding,
            screenshot: e.payload.screenshot ?? null,
          });
        },
      ),
      listen("menu://new", () => useStore.getState().newProject()),
      listen("menu://open", () => useStore.getState().openProject()),
      listen("menu://save", () => useStore.getState().saveProject()),
      listen("menu://toggle-sidebar", () => useStore.getState().toggleSidebar()),
      listen("menu://toggle-logs", () => useStore.getState().toggleLogsDrawer()),
      listen("menu://run-workflow", () => useStore.getState().runWorkflow()),
      listen("menu://stop-workflow", () => useStore.getState().stopWorkflow()),
      listen("menu://toggle-assistant", () => useStore.getState().toggleAssistant()),
      listen("assistant://repairing", () => {
        useStore.setState({ assistantRetrying: true });
      }),
      listen<{ session_id: string; entry: ChatEntry }>("assistant://message", (e) => {
        useStore.getState().appendAssistantMessage(e.payload.session_id, e.payload.entry);
      }),
      listen<{ session_id: string }>("assistant://session_started", (e) => {
        useStore.getState().setExpectedSessionId(e.payload.session_id);
      }),
      listen<ResolutionProposal>("executor://resolution_proposed", (e) => {
        useStore.setState({ resolutionProposal: e.payload });
      }),
      listen("executor://resolution_dismissed", () => {
        useStore.setState({ resolutionProposal: null });
      }),
      listen<{ patch: WorkflowPatch }>("executor://patch_applied", (e) => {
        // Merge the approved runtime patch into the frontend workflow,
        // reusing the same merge logic as applyApprovedPatch.
        useStore.getState().applyRuntimePatch(e.payload.patch);
        useStore.setState({ resolutionProposal: null });
      }),
      listen<{ status: string }>("walkthrough://state", (e) => {
        useStore.getState().setWalkthroughStatus(
          e.payload.status as WalkthroughStatus,
        );
      }),
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      listen<{ event: any }>("walkthrough://event", (e) => {
        useStore.getState().pushWalkthroughEvent(e.payload.event);
      }),
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      listen<{ actions: any[]; draft: any; warnings: string[]; action_node_map: any[] }>("walkthrough://draft_ready", (e) => {
        useStore.getState().setWalkthroughDraft({
          actions: e.payload.actions,
          draft: e.payload.draft,
          warnings: e.payload.warnings,
          action_node_map: e.payload.action_node_map ?? [],
        });
      }),
      // CdpSetupProgress type is referenced from bindings; using inline type
      // to match the existing pattern until the binding is regenerated.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      listen<any>("walkthrough://cdp-setup", (e) => {
        useStore.getState().pushCdpProgress(e.payload);
      }),
      listen<{ action: string }>("recording-bar://action", (e) => {
        const s = useStore.getState();
        switch (e.payload.action) {
          case "pause": s.pauseWalkthrough(); break;
          case "resume": s.resumeWalkthrough(); break;
          case "stop": s.stopWalkthrough(); break;
          case "cancel": s.cancelWalkthrough(); break;
        }
      }),
    ]).catch((err) => {
      console.error("Failed to subscribe to Tauri events:", err);
      useStore.getState().pushLog(`Critical: event listeners failed to initialize: ${err}`);
      return [] as (() => void)[];
    });

    // Check MCP sidecar status on mount.
    commands.getMcpStatus().then((result) => {
      if (result.status === "ok") {
        useStore.getState().pushLog(`MCP sidecar ready: ${result.data}`);
      } else {
        useStore.getState().pushLog(`⚠ MCP sidecar not found: ${result.error}. Workflow execution, planning, and walkthroughs will fail.`);
      }
    });

    return () => {
      subscriptions.then((unlisteners) => unlisteners.forEach((u) => u()));
    };
  }, []);
}
