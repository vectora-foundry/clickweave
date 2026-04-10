import type { StateCreator } from "zustand";
import type { StoreState } from "./types";

export interface AgentStep {
  summary: string;
  toolName: string;
  toolArgs: unknown;
  toolResult: string;
  pageTransitioned: boolean;
}

export type AgentStatus = "idle" | "running" | "paused" | "complete" | "error";

export interface AgentSlice {
  agentStatus: AgentStatus;
  agentGoal: string;
  agentSteps: AgentStep[];
  agentPlanHorizon: string[];
  agentError: string | null;
  currentAgentStep: number;
  startAgent: (goal: string) => Promise<void>;
  pauseAgent: () => Promise<void>;
  resumeAgent: () => Promise<void>;
  stopAgent: () => Promise<void>;
  addAgentStep: (step: AgentStep) => void;
  setAgentPlanHorizon: (horizon: string[]) => void;
  setAgentStatus: (status: AgentStatus) => void;
  setAgentError: (error: string | null) => void;
  resetAgent: () => void;
}

export const createAgentSlice: StateCreator<StoreState, [], [], AgentSlice> = (
  set,
  get,
) => ({
  agentStatus: "idle",
  agentGoal: "",
  agentSteps: [],
  agentPlanHorizon: [],
  agentError: null,
  currentAgentStep: 0,

  startAgent: async (goal) => {
    const { pushLog } = get();
    set({
      agentStatus: "running",
      agentGoal: goal,
      agentSteps: [],
      agentPlanHorizon: [],
      agentError: null,
      currentAgentStep: 0,
    });
    pushLog(`Agent started with goal: ${goal}`);
    // TODO: call Tauri run_agent command once bindings are regenerated
  },

  pauseAgent: async () => {
    set({ agentStatus: "paused" });
    get().pushLog("Agent paused");
    // TODO: call Tauri steer_agent with pause message once bindings are regenerated
  },

  resumeAgent: async () => {
    set({ agentStatus: "running" });
    get().pushLog("Agent resumed");
    // TODO: call Tauri steer_agent with resume message once bindings are regenerated
  },

  stopAgent: async () => {
    const { pushLog } = get();
    set({ agentStatus: "idle", agentGoal: "", agentError: null });
    pushLog("Agent stopped");
    // TODO: call Tauri stop_agent command once bindings are regenerated
  },

  addAgentStep: (step) => {
    set((s) => ({
      agentSteps: [...s.agentSteps, step],
      currentAgentStep: s.agentSteps.length,
    }));
  },

  setAgentPlanHorizon: (horizon) => set({ agentPlanHorizon: horizon }),

  setAgentStatus: (status) => set({ agentStatus: status }),

  setAgentError: (error) => set({ agentError: error }),

  resetAgent: () =>
    set({
      agentStatus: "idle",
      agentGoal: "",
      agentSteps: [],
      agentPlanHorizon: [],
      agentError: null,
      currentAgentStep: 0,
    }),
});
