import type { StateCreator } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type { Node, Edge } from "../../bindings";
import { toEndpoint } from "../settings";
import type { StoreState } from "./types";

export interface AgentStep {
  summary: string;
  toolName: string;
  toolArgs: unknown;
  toolResult: string;
  pageTransitioned: boolean;
}

export type AgentStatus = "idle" | "running" | "complete" | "stopped" | "error";

export interface PendingApproval {
  stepIndex: number;
  toolName: string;
  arguments: unknown;
  description: string;
}

/**
 * Tauri rejects with a structured `CommandError { kind, message }` for
 * typed failures (e.g. `AlreadyRunning`), but tauri-specta can also
 * surface plain strings when an error is serialized through `Display`.
 * Prefer the structured `message`, fall back to string coercion.
 */
function formatAgentError(err: unknown): string {
  if (err && typeof err === "object" && "message" in err) {
    const m = (err as { message?: unknown }).message;
    if (typeof m === "string" && m.length > 0) return m;
  }
  if (typeof err === "string") return err;
  return String(err);
}

/**
 * True when the rejection is the backend's `AlreadyRunning` refusal —
 * either the structured `{ kind: "AlreadyRunning" }` or the string
 * form `"AlreadyRunning: ..."` that `Display` produces.
 */
function isAlreadyRunningError(err: unknown): boolean {
  if (err && typeof err === "object" && "kind" in err) {
    return (err as { kind?: unknown }).kind === "AlreadyRunning";
  }
  if (typeof err === "string") {
    return err.startsWith("AlreadyRunning");
  }
  return false;
}

export interface AgentSlice {
  agentStatus: AgentStatus;
  agentGoal: string;
  agentSteps: AgentStep[];
  agentError: string | null;
  currentAgentStep: number;
  pendingApproval: PendingApproval | null;
  /** Generation ID for the active run — used to reject stale events. */
  agentRunId: string | null;
  startAgent: (goal: string) => Promise<void>;
  stopAgent: () => Promise<void>;
  addAgentStep: (step: AgentStep) => void;
  addAgentNode: (node: Node) => void;
  addAgentEdge: (edge: Edge) => void;
  setPendingApproval: (approval: PendingApproval | null) => void;
  approveAction: () => Promise<void>;
  rejectAction: () => Promise<void>;
  setAgentStatus: (status: AgentStatus) => void;
  setAgentError: (error: string | null) => void;
  setAgentRunId: (runId: string) => void;
  resetAgent: () => void;
}

export const createAgentSlice: StateCreator<StoreState, [], [], AgentSlice> = (
  set,
  get,
) => ({
  agentStatus: "idle",
  agentGoal: "",
  agentSteps: [],
  agentError: null,
  currentAgentStep: 0,
  pendingApproval: null,
  agentRunId: null,

  startAgent: async (goal) => {
    const priorState = get();
    const { pushLog, agentConfig, projectPath, workflow, agentStatus } =
      priorState;
    // If a run is already active, do not touch run-scoped state: the
    // backend will reject with AlreadyRunning and the live run's events
    // must keep routing through useAgentEvents. Otherwise optimistically
    // reset into the "running" shape before awaiting invoke, so early
    // terminal events (e.g. `agent://error` from a fast MCP-spawn
    // failure) can flip status to "error" — their handler gates on
    // `agentStatus === "running"`.
    const wasActive = agentStatus === "running";
    // Snapshot the prior run's visible state so we can restore it if
    // the backend rejects with AlreadyRunning during its async cleanup
    // window (handle still set but previous run has emitted its
    // terminal event). Without this, a restart attempt in that window
    // would wipe the terminal run's history and log it as "error".
    const snapshot = {
      agentStatus: priorState.agentStatus,
      agentGoal: priorState.agentGoal,
      agentSteps: priorState.agentSteps,
      agentError: priorState.agentError,
      currentAgentStep: priorState.currentAgentStep,
      pendingApproval: priorState.pendingApproval,
      agentRunId: priorState.agentRunId,
    };
    if (!wasActive) {
      set({
        agentStatus: "running",
        agentGoal: goal,
        agentSteps: [],
        agentError: null,
        currentAgentStep: 0,
        pendingApproval: null,
        // Clear the prior run's ID so any late in-flight events from it
        // fail `isStaleRunId` (which drops events when active is null).
        // `agent://started` from the new run will install the fresh ID;
        // setting null here — not after invoke — avoids racing with that
        // listener.
        agentRunId: null,
      });
      pushLog(`Agent started with goal: ${goal}`);
    }
    try {
      await invoke("run_agent", {
        request: {
          goal,
          agent: toEndpoint(agentConfig),
          project_path: projectPath,
          workflow_name: workflow.name,
          workflow_id: workflow.id,
        },
      });
    } catch (err) {
      const msg = formatAgentError(err);
      pushLog(`Agent start rejected: ${msg}`);
      if (wasActive) {
        // A live run was already active — its state was never touched,
        // and its events must keep routing.
        return;
      }
      if (isAlreadyRunningError(err)) {
        // Backend is still tearing down the previous run. Restore its
        // visible state so its terminal history is not lost.
        set(snapshot);
        return;
      }
      set({ agentStatus: "error", agentError: msg });
    }
  },

  stopAgent: async () => {
    const { pushLog } = get();
    set({ agentStatus: "stopped", pendingApproval: null });
    try {
      await invoke("stop_agent");
    } catch {
      /* ignore if not running */
    }
    pushLog("Agent stopped");
  },

  addAgentStep: (step) => {
    set((s) => ({
      agentSteps: [...s.agentSteps, step],
      currentAgentStep: s.agentSteps.length,
    }));
  },

  addAgentNode: (node) => {
    const { workflow, setWorkflow } = get();
    setWorkflow({
      ...workflow,
      nodes: [...workflow.nodes, node],
    });
  },

  addAgentEdge: (edge) => {
    const { workflow, setWorkflow } = get();
    setWorkflow({
      ...workflow,
      edges: [...workflow.edges, edge],
    });
  },

  setPendingApproval: (approval) => set({ pendingApproval: approval }),

  approveAction: async () => {
    const { pushLog, pendingApproval } = get();
    if (!pendingApproval) return;
    set({ pendingApproval: null });
    try {
      await invoke("approve_agent_action", { approved: true });
      pushLog(`Approved: ${pendingApproval.toolName}`);
    } catch (err) {
      pushLog(`Approval send failed: ${formatAgentError(err)}`);
    }
  },

  rejectAction: async () => {
    const { pushLog, pendingApproval } = get();
    if (!pendingApproval) return;
    set({ pendingApproval: null });
    try {
      await invoke("approve_agent_action", { approved: false });
      pushLog(`Rejected: ${pendingApproval.toolName}`);
    } catch (err) {
      pushLog(`Rejection send failed: ${formatAgentError(err)}`);
    }
  },

  setAgentStatus: (status) => set({ agentStatus: status }),

  setAgentError: (error) => set({ agentError: error }),

  setAgentRunId: (runId) => set({ agentRunId: runId }),

  resetAgent: () =>
    set({
      agentStatus: "idle",
      agentGoal: "",
      agentSteps: [],
      agentError: null,
      currentAgentStep: 0,
      pendingApproval: null,
      agentRunId: null,
    }),
});
