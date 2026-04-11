import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import type { Node, Edge } from "../../bindings";
import { useStore } from "../../store/useAppStore";

interface AgentStepPayload {
  summary: string;
  tool_name: string;
  step_number: number;
}

interface AgentPlanPayload {
  horizon: string[];
}

interface CdpConnectedPayload {
  app_name: string;
  port: number;
}

interface AgentErrorPayload {
  message: string;
}

interface AgentStoppedPayload {
  reason: string;
  steps_executed?: number;
  consecutive_errors?: number;
}

interface StepFailedPayload {
  step_number: number;
  tool_name: string;
  error: string;
}

interface ApprovalRequiredPayload {
  step_index: number;
  tool_name: string;
  arguments: unknown;
  description: string;
}

/**
 * Subscribe to agent backend events:
 * agent://step, agent://plan, agent://complete, agent://stopped,
 * agent://error, agent://node_added, agent://edge_added,
 * agent://approval_required, agent://cdp_connected, agent://step_failed.
 *
 * Dispatches into the Zustand AgentSlice via `getState()`.
 */
export function useAgentEvents() {
  useEffect(() => {
    const unlisteners: (() => void)[] = [];
    let cancelled = false;

    const sub = (p: Promise<() => void>) =>
      p
        .then((u) => {
          if (cancelled) {
            u();
            return;
          }
          unlisteners.push(u);
        })
        .catch((err) => {
          console.error("Failed to subscribe to agent event:", err);
          useStore
            .getState()
            .pushLog(`Critical: agent event listener failed: ${err}`);
        });

    sub(
      listen<AgentStepPayload>("agent://step", (e) => {
        useStore.getState().addAgentStep({
          summary: e.payload.summary,
          toolName: e.payload.tool_name,
          toolArgs: null,
          toolResult: e.payload.summary,
          pageTransitioned: false,
        });
        useStore
          .getState()
          .pushLog(
            `Agent step ${e.payload.step_number}: ${e.payload.tool_name}`,
          );
      }),
    );

    sub(
      listen<Node>("agent://node_added", (e) => {
        useStore.getState().addAgentNode(e.payload);
      }),
    );

    sub(
      listen<Edge>("agent://edge_added", (e) => {
        useStore.getState().addAgentEdge(e.payload);
      }),
    );

    sub(
      listen<ApprovalRequiredPayload>("agent://approval_required", (e) => {
        useStore.getState().setPendingApproval({
          stepIndex: e.payload.step_index,
          toolName: e.payload.tool_name,
          arguments: e.payload.arguments,
          description: e.payload.description,
        });
        useStore.getState().pushLog(
          `Agent awaiting approval: ${e.payload.tool_name}`,
        );
      }),
    );

    sub(
      listen<CdpConnectedPayload>("agent://cdp_connected", (e) => {
        useStore
          .getState()
          .pushLog(
            `CDP connected to '${e.payload.app_name}' (port ${e.payload.port})`,
          );
      }),
    );

    sub(
      listen<StepFailedPayload>("agent://step_failed", (e) => {
        useStore.getState().addAgentStep({
          summary: `Error: ${e.payload.error}`,
          toolName: e.payload.tool_name,
          toolArgs: null,
          toolResult: e.payload.error,
          pageTransitioned: false,
        });
        useStore
          .getState()
          .pushLog(
            `Agent step ${e.payload.step_number} failed: ${e.payload.tool_name} — ${e.payload.error}`,
          );
      }),
    );

    sub(
      listen<AgentPlanPayload>("agent://plan", (e) => {
        useStore.getState().setAgentPlanHorizon(e.payload.horizon);
      }),
    );

    sub(
      listen("agent://complete", () => {
        useStore.getState().setAgentStatus("complete");
        useStore.getState().pushLog("Agent completed");
      }),
    );

    sub(
      listen<AgentStoppedPayload>("agent://stopped", (e) => {
        // Only transition to "stopped" if the agent was still active.
        // If user already hit Stop, status is already "stopped" or "idle".
        const current = useStore.getState().agentStatus;
        if (current === "running" || current === "paused") {
          useStore.getState().setAgentStatus("stopped");
        }
        const detail =
          e.payload.reason === "max_steps_reached"
            ? `after ${e.payload.steps_executed} steps`
            : e.payload.reason === "max_errors_reached"
              ? `after ${e.payload.consecutive_errors} consecutive errors`
              : e.payload.reason === "approval_unavailable"
                ? "approval system unavailable"
                : e.payload.reason;
        useStore
          .getState()
          .pushLog(`Agent stopped: ${detail}`);
      }),
    );

    sub(
      listen<AgentErrorPayload>("agent://error", (e) => {
        useStore.getState().setAgentError(e.payload.message);
        useStore.getState().setAgentStatus("error");
        useStore
          .getState()
          .pushLog(`Agent error: ${e.payload.message}`);
      }),
    );

    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
    };
  }, []);
}
