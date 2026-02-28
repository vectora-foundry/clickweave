import type { StateCreator } from "zustand";
import { commands } from "../../bindings";
import type { WalkthroughAction, Workflow } from "../../bindings";
import type { StoreState } from "./types";

export type WalkthroughStatus = "Idle" | "Recording" | "Paused" | "Processing" | "Review" | "Applied" | "Cancelled";

/** Opaque captured event from the backend (serialized WalkthroughEvent). */
export type WalkthroughCapturedEvent = Record<string, unknown>;

export interface WalkthroughSlice {
  walkthroughStatus: WalkthroughStatus;
  walkthroughError: string | null;
  walkthroughEvents: WalkthroughCapturedEvent[];
  walkthroughActions: WalkthroughAction[];
  walkthroughDraft: Workflow | null;
  walkthroughWarnings: string[];

  setWalkthroughStatus: (status: WalkthroughStatus) => void;
  pushWalkthroughEvent: (event: WalkthroughCapturedEvent) => void;
  setWalkthroughDraft: (payload: { actions: WalkthroughAction[]; draft: Workflow | null; warnings: string[] }) => void;
  fetchWalkthroughDraft: () => Promise<void>;
  startWalkthrough: () => Promise<void>;
  pauseWalkthrough: () => Promise<void>;
  resumeWalkthrough: () => Promise<void>;
  stopWalkthrough: () => Promise<void>;
  cancelWalkthrough: () => Promise<void>;
}

export const createWalkthroughSlice: StateCreator<StoreState, [], [], WalkthroughSlice> = (set, get) => ({
  walkthroughStatus: "Idle",
  walkthroughError: null,
  walkthroughEvents: [],
  walkthroughActions: [],
  walkthroughDraft: null,
  walkthroughWarnings: [],

  setWalkthroughStatus: (status) => set({ walkthroughStatus: status }),

  pushWalkthroughEvent: (event) => set((s) => ({
    walkthroughEvents: [...s.walkthroughEvents, event],
  })),

  setWalkthroughDraft: ({ actions, draft, warnings }) => set({
    walkthroughActions: actions,
    walkthroughDraft: draft,
    walkthroughWarnings: warnings,
  }),

  fetchWalkthroughDraft: async () => {
    const result = await commands.getWalkthroughDraft();
    if (result.status === "ok") {
      set({
        walkthroughActions: result.data.actions,
        walkthroughDraft: result.data.draft ?? null,
        walkthroughWarnings: result.data.warnings,
      });
    }
  },

  startWalkthrough: async () => {
    const { workflow, mcpCommand, projectPath, pushLog } = get();
    set({ walkthroughError: null, walkthroughEvents: [] });
    const result = await commands.startWalkthrough(workflow.id, mcpCommand, projectPath ?? null);
    if (result.status === "error") {
      set({ walkthroughError: result.error });
      pushLog(`Walkthrough start failed: ${result.error}`);
    }
  },

  pauseWalkthrough: async () => {
    const { pushLog } = get();
    const result = await commands.pauseWalkthrough();
    if (result.status === "error") {
      pushLog(`Walkthrough pause failed: ${result.error}`);
    }
  },

  resumeWalkthrough: async () => {
    const { pushLog } = get();
    const result = await commands.resumeWalkthrough();
    if (result.status === "error") {
      pushLog(`Walkthrough resume failed: ${result.error}`);
    }
  },

  stopWalkthrough: async () => {
    const { pushLog } = get();
    const result = await commands.stopWalkthrough();
    if (result.status === "error") {
      pushLog(`Walkthrough stop failed: ${result.error}`);
    }
  },

  cancelWalkthrough: async () => {
    const { pushLog } = get();
    set({ walkthroughEvents: [], walkthroughActions: [], walkthroughDraft: null, walkthroughWarnings: [] });
    const result = await commands.cancelWalkthrough();
    if (result.status === "error") {
      pushLog(`Walkthrough cancel failed: ${result.error}`);
    }
  },
});
