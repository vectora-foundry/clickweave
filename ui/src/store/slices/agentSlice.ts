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
    const { pushLog, agentConfig, projectPath, workflow } = get();
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
      // Reset visible run state only after the backend accepts. On
      // rejection (e.g. AlreadyRunning from a duplicate click), leaving
      // run-scoped state untouched keeps the still-live original run's
      // events routing through useAgentEvents instead of being dropped
      // as stale.
      set({
        agentStatus: "running",
        agentGoal: goal,
        agentSteps: [],
        agentError: null,
        currentAgentStep: 0,
        pendingApproval: null,
        agentRunId: null,
      });
      pushLog(`Agent started with goal: ${goal}`);
    } catch (err) {
      pushLog(`Agent start rejected: ${formatAgentError(err)}`);
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
