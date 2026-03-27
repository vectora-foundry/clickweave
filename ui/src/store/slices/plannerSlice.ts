import type { StateCreator } from "zustand";
import { commands } from "../../bindings";
import { errorMessage } from "../../utils/commandError";
import type { StoreState } from "./types";

export interface PlannerToolCall {
  toolName: string;
  args: Record<string, unknown>;
  result?: string;
}

export interface PlannerConfirmation {
  sessionId: string;
  message: string;
  toolName: string;
}

export interface PlannerSlice {
  plannerToolCalls: PlannerToolCall[];
  plannerConfirmation: PlannerConfirmation | null;
  plannerSessionActive: boolean;

  pushPlannerToolCall: (call: PlannerToolCall) => void;
  setPlannerConfirmation: (confirmation: PlannerConfirmation | null) => void;
  setPlannerSessionActive: (active: boolean) => void;
  respondToPlannerConfirmation: (approved: boolean) => Promise<void>;
  clearPlannerState: () => void;
}

export const createPlannerSlice: StateCreator<StoreState, [], [], PlannerSlice> = (set, get) => ({
  plannerToolCalls: [],
  plannerConfirmation: null,
  plannerSessionActive: false,

  pushPlannerToolCall: (call) =>
    set((s) => ({ plannerToolCalls: [...s.plannerToolCalls, call] })),

  setPlannerConfirmation: (confirmation) =>
    set({ plannerConfirmation: confirmation }),

  setPlannerSessionActive: (active) =>
    set({ plannerSessionActive: active }),

  respondToPlannerConfirmation: async (approved) => {
    set({ plannerConfirmation: null });
    const result = await commands.plannerConfirmationRespond(approved);
    if (result.status === "error") {
      get().pushLog(`Planner confirmation failed: ${errorMessage(result.error)}`);
    }
  },

  clearPlannerState: () =>
    set({
      plannerToolCalls: [],
      plannerConfirmation: null,
      plannerSessionActive: false,
    }),
});
