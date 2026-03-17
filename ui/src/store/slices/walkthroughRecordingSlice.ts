import type { StateCreator } from "zustand";
import { commands } from "../../bindings";
import type { CdpAppConfig } from "../../bindings";
import type { CdpSetupProgress } from "../../components/CdpAppSelectModal";
import { toEndpoint } from "../settings";
import { errorMessage } from "../../utils/commandError";
import type { StoreState } from "./types";
import type { WalkthroughCapturedEvent } from "./walkthroughSlice";
import { openRecordingBarWindow, closeRecordingBarWindow } from "./walkthroughSlice";

export interface WalkthroughRecordingSlice {
  pushWalkthroughEvent: (event: WalkthroughCapturedEvent) => void;
  pushCdpProgress: (progress: CdpSetupProgress) => void;
  openCdpModal: () => void;
  closeCdpModal: () => void;
  startWalkthrough: (cdpApps?: CdpAppConfig[]) => Promise<void>;
  pauseWalkthrough: () => Promise<void>;
  resumeWalkthrough: () => Promise<void>;
  stopWalkthrough: () => Promise<void>;
  cancelWalkthrough: () => Promise<void>;
}

const emptyAnnotations = {
  deleted_node_ids: [] as string[],
  renamed_nodes: [] as { node_id: string; new_name: string }[],
  target_overrides: [] as { node_id: string; chosen_candidate_index: number }[],
  variable_promotions: [] as { node_id: string; variable_name: string }[],
};

export const createWalkthroughRecordingSlice: StateCreator<StoreState, [], [], WalkthroughRecordingSlice> = (_set, get) => ({
  pushWalkthroughEvent: (event) => _set((s) => ({
    walkthroughEvents: [...s.walkthroughEvents, event],
  })),

  pushCdpProgress: (progress) => {
    if (typeof progress.status === "string" && progress.status === "Done") {
      _set({ walkthroughCdpModalOpen: false });
      return;
    }
    _set((s) => ({
      walkthroughCdpProgress: [...s.walkthroughCdpProgress, progress],
    }));
  },

  openCdpModal: () => _set({ walkthroughCdpModalOpen: true, walkthroughCdpProgress: [] }),
  closeCdpModal: () => _set({ walkthroughCdpModalOpen: false }),

  startWalkthrough: async (cdpApps: CdpAppConfig[] = []) => {
    const { workflow, mcpCommand, projectPath, pushLog, plannerConfig } = get();
    _set({
      walkthroughError: null,
      walkthroughEvents: [],
      walkthroughAnnotations: { ...emptyAnnotations },
      walkthroughExpandedAction: null,
      walkthroughActionNodeMap: [],
      walkthroughCdpProgress: [],
      walkthroughNodeOrder: [],

      assistantOpen: false,
    });
    const planner = plannerConfig.baseUrl && plannerConfig.model
      ? toEndpoint(plannerConfig)
      : null;
    const { hoverDwellThreshold } = get();
    const result = await commands.startWalkthrough(workflow.id, mcpCommand, projectPath ?? null, planner, cdpApps, hoverDwellThreshold);
    if (result.status === "error") {
      const msg = errorMessage(result.error);
      _set({ walkthroughError: msg, walkthroughCdpModalOpen: false });
      pushLog(`Walkthrough start failed: ${msg}`);
    } else {
      openRecordingBarWindow();
    }
  },

  pauseWalkthrough: async () => {
    const { pushLog } = get();
    const result = await commands.pauseWalkthrough();
    if (result.status === "error") {
      pushLog(`Walkthrough pause failed: ${errorMessage(result.error)}`);
    }
  },

  resumeWalkthrough: async () => {
    const { pushLog } = get();
    const result = await commands.resumeWalkthrough();
    if (result.status === "error") {
      pushLog(`Walkthrough resume failed: ${errorMessage(result.error)}`);
    }
  },

  stopWalkthrough: async () => {
    const { pushLog, plannerConfig, hoverDwellThreshold } = get();
    const planner = plannerConfig.baseUrl && plannerConfig.model
      ? toEndpoint(plannerConfig)
      : null;
    const result = await commands.stopWalkthrough(planner, hoverDwellThreshold);
    if (result.status === "error") {
      pushLog(`Walkthrough stop failed: ${errorMessage(result.error)}`);
    }
  },

  cancelWalkthrough: async () => {
    const { pushLog } = get();
    closeRecordingBarWindow();
    _set({
      walkthroughEvents: [],
      walkthroughActions: [],
      walkthroughDraft: null,
      walkthroughWarnings: [],
      walkthroughAnnotations: { ...emptyAnnotations },
      walkthroughExpandedAction: null,
      walkthroughActionNodeMap: [],
      walkthroughNodeOrder: [],

      walkthroughPanelOpen: false,
    });
    const result = await commands.cancelWalkthrough();
    if (result.status === "error") {
      pushLog(`Walkthrough cancel failed: ${errorMessage(result.error)}`);
    }
  },
});
