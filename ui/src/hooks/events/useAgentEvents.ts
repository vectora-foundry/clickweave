import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import type { Node, Edge } from "../../bindings";
import { useStore } from "../../store/useAppStore";
import type { AgentStatus } from "../../store/slices/agentSlice";

/** All run-scoped payloads include a generation ID. */
interface RunScoped {
  run_id: string;
}

interface AgentStartedPayload extends RunScoped {}

interface AgentStepPayload extends RunScoped {
  summary: string;
  tool_name: string;
  step_number: number;
}

interface NodeAddedPayload extends RunScoped {
  node: Node;
}

interface EdgeAddedPayload extends RunScoped {
  edge: Edge;
}

interface CdpConnectedPayload extends RunScoped {
  app_name: string;
  port: number;
}

interface AgentErrorPayload extends RunScoped {
  message: string;
}

interface AgentStoppedPayload extends RunScoped {
  reason: string;
  steps_executed?: number;
  consecutive_errors?: number;
}

interface StepFailedPayload extends RunScoped {
  step_number: number;
  tool_name: string;
  error: string;
}

interface ApprovalRequiredPayload extends RunScoped {
  step_index: number;
  tool_name: string;
  arguments: unknown;
  description: string;
}

/**
 * Subscribe to agent backend events:
 * agent://started, agent://step, agent://complete, agent://stopped,
 * agent://error, agent://warning, agent://node_added, agent://edge_added,
 * agent://approval_required, agent://cdp_connected, agent://step_failed.
 *
 * All run-scoped events carry a `run_id` generation ID. Events whose
 * run_id does not match the active run are silently dropped to prevent
 * stale state from a previous run leaking into the current one.
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

    /** Reject events from a stale run or during the null gap before agent://started. */
    const isStale = (runId: string): boolean => {
      const activeRunId = useStore.getState().agentRunId;
      return activeRunId === null || runId !== activeRunId;
    };

    // ── Run lifecycle ──────────────────────────────────────────

    sub(
      listen<AgentStartedPayload>("agent://started", (e) => {
        useStore.getState().setAgentRunId(e.payload.run_id);
      }),
    );

    // ── Step events ────────────────────────────────────────────

    sub(
      listen<AgentStepPayload>("agent://step", (e) => {
        if (isStale(e.payload.run_id)) return;
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
      listen<NodeAddedPayload>("agent://node_added", (e) => {
        if (isStale(e.payload.run_id)) return;
        useStore.getState().addAgentNode(e.payload.node);
      }),
    );

    sub(
      listen<EdgeAddedPayload>("agent://edge_added", (e) => {
        if (isStale(e.payload.run_id)) return;
        useStore.getState().addAgentEdge(e.payload.edge);
      }),
    );

    sub(
      listen<ApprovalRequiredPayload>("agent://approval_required", (e) => {
        if (isStale(e.payload.run_id)) return;
        // Ignore stale approval requests that arrive after stop/cancel
        const current = useStore.getState().agentStatus;
        if (current !== "running") return;
        useStore.getState().setPendingApproval({
          stepIndex: e.payload.step_index,
          toolName: e.payload.tool_name,
          arguments: e.payload.arguments,
          description: e.payload.description,
        });
        useStore
          .getState()
          .pushLog(`Agent awaiting approval: ${e.payload.tool_name}`);
      }),
    );

    sub(
      listen<CdpConnectedPayload>("agent://cdp_connected", (e) => {
        if (isStale(e.payload.run_id)) return;
        useStore
          .getState()
          .pushLog(
            `CDP connected to '${e.payload.app_name}' (port ${e.payload.port})`,
          );
      }),
    );

    sub(
      listen<StepFailedPayload>("agent://step_failed", (e) => {
        if (isStale(e.payload.run_id)) return;
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
      listen<RunScoped & { tool_name: string; summary: string }>(
        "agent://sub_action",
        (e) => {
          if (isStale(e.payload.run_id)) return;
          useStore
            .getState()
            .pushLog(
              `Agent auto-action: ${e.payload.tool_name} — ${e.payload.summary}`,
            );
        },
      ),
    );

    // ── Terminal events ────────────────────────────────────────

    // Only transition status if the agent was still active — prevents
    // a backend event from overriding a user-initiated stop.
    const setStatusIfActive = (status: AgentStatus) => {
      const current = useStore.getState().agentStatus;
      if (current === "running") {
        useStore.getState().setAgentStatus(status);
      }
    };

    sub(
      listen<RunScoped>("agent://complete", (e) => {
        if (isStale(e.payload.run_id)) return;
        setStatusIfActive("complete");
        useStore.getState().pushLog("Agent completed");
      }),
    );

    sub(
      listen<AgentStoppedPayload>("agent://stopped", (e) => {
        if (isStale(e.payload.run_id)) return;
        setStatusIfActive("stopped");
        const detail =
          e.payload.reason === "max_steps_reached"
            ? `after ${e.payload.steps_executed} steps`
            : e.payload.reason === "max_errors_reached"
              ? `after ${e.payload.consecutive_errors} consecutive errors`
              : e.payload.reason === "approval_unavailable"
                ? "approval system unavailable"
                : e.payload.reason;
        useStore.getState().pushLog(`Agent stopped: ${detail}`);
      }),
    );

    sub(
      listen<RunScoped & { message: string }>("agent://warning", (e) => {
        if (isStale(e.payload.run_id)) return;
        useStore
          .getState()
          .pushLog(`Agent warning: ${e.payload.message}`);
      }),
    );

    sub(
      listen<AgentErrorPayload>("agent://error", (e) => {
        if (isStale(e.payload.run_id)) return;
        // Only transition to error if the agent was still active —
        // a racing error after stop should not override "stopped".
        const current = useStore.getState().agentStatus;
        if (current === "running") {
          useStore.getState().setAgentError(e.payload.message);
          useStore.getState().setAgentStatus("error");
        }
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
