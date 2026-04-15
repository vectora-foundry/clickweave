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

export interface CandidateRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface AmbiguityCandidateView {
  uid: string;
  snippet: string;
  rect: CandidateRect | null;
}

export interface AmbiguityResolution {
  /** Client-side id so the UI can key modals/cards without relying on
   *  backend-supplied node_id (multiple resolutions can fire per node). */
  id: string;
  nodeId: string;
  target: string;
  candidates: AmbiguityCandidateView[];
  chosenUid: string;
  reasoning: string;
  /** Viewport dimensions (CSS pixels) at capture time — rects are relative
   *  to this viewport, not to the full screenshot. `0` means unknown. */
  viewportWidth: number;
  viewportHeight: number;
  /** Path relative to the node's `artifacts/` directory. */
  screenshotPath: string;
  /** Base64-encoded PNG data. Populated from the live executor event. */
  screenshotBase64: string;
  /** Epoch ms at which the resolution was observed on the UI side. */
  createdAt: number;
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
  /** Ambiguity resolution records, newest first. Persists across agent
   *  completion so the user can inspect past resolutions. */
  ambiguityResolutions: AmbiguityResolution[];
  /** Active modal target for the ambiguity inspector, keyed by
   *  AmbiguityResolution.id. */
  activeAmbiguityId: string | null;
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
  addAmbiguityResolution: (resolution: AmbiguityResolution) => void;
  openAmbiguityModal: (id: string) => void;
  closeAmbiguityModal: () => void;
  clearAmbiguityResolutions: () => void;
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
  ambiguityResolutions: [],
  activeAmbiguityId: null,

  startAgent: async (goal) => {
    const { pushLog, agentConfig, projectPath, workflow } = get();
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
      set({ agentStatus: "error", agentError: msg });
      pushLog(`Agent failed: ${msg}`);
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
      // Ambiguity records are intentionally NOT cleared — they persist across
      // runs so the user can still inspect past resolutions until they
      // explicitly clear them or start a new project.
    }),

  addAmbiguityResolution: (resolution) =>
    set((s) => ({
      ambiguityResolutions: [resolution, ...s.ambiguityResolutions],
    })),

  openAmbiguityModal: (id) => set({ activeAmbiguityId: id }),

  closeAmbiguityModal: () => set({ activeAmbiguityId: null }),

  clearAmbiguityResolutions: () =>
    set({ ambiguityResolutions: [], activeAmbiguityId: null }),
});
