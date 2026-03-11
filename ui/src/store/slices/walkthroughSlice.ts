import type { StateCreator } from "zustand";
import { commands } from "../../bindings";
import type { CdpAppConfig, NodeRename, TargetOverride, VariablePromotion, WalkthroughAction, WalkthroughAnnotations, Workflow } from "../../bindings";
import type { CdpSetupProgress } from "../../components/CdpAppSelectModal";
import { applyAnnotationsToDraft, findCandidateInsertIndex, recomputeNodePositions, synthesizeNodeForKeptCandidate } from "../../utils/walkthroughDraft";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { currentMonitor } from "@tauri-apps/api/window";
import { LogicalSize, LogicalPosition } from "@tauri-apps/api/dpi";
import { toEndpoint } from "../settings";
import type { StoreState } from "./types";

export type WalkthroughStatus = "Idle" | "Recording" | "Paused" | "Processing" | "Review" | "Applied" | "Cancelled";

/** Returns true when the walkthrough panel is visible and active (not in a terminal state). */
export function isWalkthroughActive(status: WalkthroughStatus): boolean {
  return status !== "Idle" && status !== "Applied" && status !== "Cancelled";
}

/** Build a lookup map from node_id → WalkthroughAction using the action-node map. */
export function buildActionByNodeId(
  actionNodeMap: ActionNodeEntry[],
  actions: WalkthroughAction[],
): Map<string, WalkthroughAction> {
  const map = new Map<string, WalkthroughAction>();
  for (const entry of actionNodeMap) {
    const action = actions.find((a) => a.id === entry.action_id);
    if (action) map.set(entry.node_id, action);
  }
  return map;
}

/** Opaque captured event from the backend (serialized WalkthroughEvent). */
export type WalkthroughCapturedEvent = Record<string, unknown>;

/** Maps a walkthrough action to its corresponding workflow node. */
export interface ActionNodeEntry {
  action_id: string;
  node_id: string;
}

/** Upsert an entry into an annotation array, matching by node_id. */
function upsertAnnotation<T extends { node_id: string }>(arr: T[], entry: T): T[] {
  const idx = arr.findIndex((item) => item.node_id === entry.node_id);
  return idx >= 0 ? arr.map((item, i) => (i === idx ? entry : item)) : [...arr, entry];
}

const RECORDING_BAR_LABEL = "recording-bar";
const BAR_WIDTH = 460;
const BAR_HEIGHT = 48;

async function openRecordingBarWindow() {
  // Don't create if already exists
  const existing = await WebviewWindow.getByLabel(RECORDING_BAR_LABEL);
  if (existing) return;

  // Center horizontally at top of the current monitor
  const monitor = await currentMonitor();
  const screenWidth = monitor?.size.width ?? 1920;
  const scaleFactor = monitor?.scaleFactor ?? 2;
  const logicalScreenWidth = screenWidth / scaleFactor;
  const x = Math.round((logicalScreenWidth - BAR_WIDTH) / 2);

  new WebviewWindow(RECORDING_BAR_LABEL, {
    url: "/?view=recording-bar",
    title: "",
    width: BAR_WIDTH,
    height: BAR_HEIGHT,
    x,
    y: 12,
    decorations: false,
    transparent: true,
    alwaysOnTop: true,
    resizable: false,
    minimizable: false,
    maximizable: false,
    closable: false,
    skipTaskbar: true,
    shadow: false,
    focus: false,
    acceptFirstMouse: true,
  });
}

async function closeRecordingBarWindow() {
  const win = await WebviewWindow.getByLabel(RECORDING_BAR_LABEL);
  if (win) await win.close();
  // Bring the main window back to front
  const main = await WebviewWindow.getByLabel("main");
  if (main) await main.setFocus();
}

const emptyAnnotations: WalkthroughAnnotations = {
  deleted_node_ids: [],
  renamed_nodes: [],
  target_overrides: [],
  variable_promotions: [],
};

export interface WalkthroughSlice {
  walkthroughStatus: WalkthroughStatus;
  walkthroughPanelOpen: boolean;
  walkthroughError: string | null;
  walkthroughEvents: WalkthroughCapturedEvent[];
  walkthroughActions: WalkthroughAction[];
  walkthroughDraft: Workflow | null;
  walkthroughWarnings: string[];
  walkthroughAnnotations: WalkthroughAnnotations;
  walkthroughExpandedAction: string | null;
  walkthroughActionNodeMap: ActionNodeEntry[];
  walkthroughCdpModalOpen: boolean;
  walkthroughCdpProgress: CdpSetupProgress[];

  setWalkthroughStatus: (status: WalkthroughStatus) => void;
  setWalkthroughPanelOpen: (open: boolean) => void;
  pushWalkthroughEvent: (event: WalkthroughCapturedEvent) => void;
  pushCdpProgress: (progress: CdpSetupProgress) => void;
  setWalkthroughDraft: (payload: {
    actions: WalkthroughAction[];
    draft: Workflow | null;
    warnings: string[];
    action_node_map: ActionNodeEntry[];
  }) => void;
  fetchWalkthroughDraft: () => Promise<void>;
  openCdpModal: () => void;
  closeCdpModal: () => void;
  startWalkthrough: (cdpApps?: CdpAppConfig[]) => Promise<void>;
  pauseWalkthrough: () => Promise<void>;
  resumeWalkthrough: () => Promise<void>;
  stopWalkthrough: () => Promise<void>;
  cancelWalkthrough: () => Promise<void>;

  setWalkthroughExpandedAction: (id: string | null) => void;
  keepCandidate: (actionId: string) => void;
  dismissCandidate: (actionId: string) => void;
  deleteNode: (nodeId: string) => void;
  restoreNode: (nodeId: string) => void;
  renameNode: (nodeId: string, newName: string) => void;
  overrideTarget: (nodeId: string, candidateIndex: number) => void;
  promoteToVariable: (nodeId: string, variableName: string) => void;
  removeVariablePromotion: (nodeId: string) => void;
  resetAnnotations: () => void;
  applyDraftToCanvas: () => Promise<void>;
  discardDraft: () => Promise<void>;
}

export const createWalkthroughSlice: StateCreator<StoreState, [], [], WalkthroughSlice> = (set, get) => ({
  walkthroughStatus: "Idle",
  walkthroughPanelOpen: false,
  walkthroughError: null,
  walkthroughEvents: [],
  walkthroughActions: [],
  walkthroughDraft: null,
  walkthroughWarnings: [],
  walkthroughAnnotations: { ...emptyAnnotations },
  walkthroughExpandedAction: null,
  walkthroughActionNodeMap: [],
  walkthroughCdpModalOpen: false,
  walkthroughCdpProgress: [],

  setWalkthroughStatus: (status) => {
    set({ walkthroughStatus: status });
    // Close the recording bar window when leaving recording states
    if (status === "Review" || status === "Idle" || status === "Cancelled" || status === "Applied") {
      closeRecordingBarWindow();
    }
  },
  setWalkthroughPanelOpen: (open) => set({ walkthroughPanelOpen: open }),

  pushWalkthroughEvent: (event) => set((s) => ({
    walkthroughEvents: [...s.walkthroughEvents, event],
  })),

  pushCdpProgress: (progress) => {
    if (typeof progress.status === "string" && progress.status === "Done") {
      set({ walkthroughCdpModalOpen: false });
      return;
    }
    set((s) => ({
      walkthroughCdpProgress: [...s.walkthroughCdpProgress, progress],
    }));
  },

  setWalkthroughDraft: ({ actions, draft, warnings, action_node_map }) => set({
    walkthroughActions: actions,
    walkthroughDraft: draft,
    walkthroughWarnings: warnings,
    walkthroughActionNodeMap: action_node_map,
    walkthroughStatus: "Review",
    walkthroughPanelOpen: true,
    walkthroughAnnotations: { ...emptyAnnotations },
  }),

  fetchWalkthroughDraft: async () => {
    const result = await commands.getWalkthroughDraft();
    if (result.status === "ok") {
      set({
        walkthroughActions: result.data.actions,
        walkthroughDraft: result.data.draft ?? null,
        walkthroughWarnings: result.data.warnings,
        walkthroughStatus: "Review",
        walkthroughPanelOpen: true,
      });
    }
  },

  openCdpModal: () => set({ walkthroughCdpModalOpen: true, walkthroughCdpProgress: [] }),
  closeCdpModal: () => set({ walkthroughCdpModalOpen: false }),

  startWalkthrough: async (cdpApps: CdpAppConfig[] = []) => {
    const { workflow, mcpCommand, projectPath, pushLog, plannerConfig } = get();
    set({
      walkthroughError: null,
      walkthroughEvents: [],
      walkthroughAnnotations: { ...emptyAnnotations },
      walkthroughExpandedAction: null,
      walkthroughActionNodeMap: [],
      walkthroughCdpProgress: [],

      assistantOpen: false,
    });
    const planner = plannerConfig.baseUrl && plannerConfig.model
      ? toEndpoint(plannerConfig)
      : null;
    const { hoverDwellThreshold } = get();
    const result = await commands.startWalkthrough(workflow.id, mcpCommand, projectPath ?? null, planner, cdpApps, hoverDwellThreshold);
    if (result.status === "error") {
      set({ walkthroughError: result.error, walkthroughCdpModalOpen: false });
      pushLog(`Walkthrough start failed: ${result.error}`);
    } else {
      openRecordingBarWindow();
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
    const { pushLog, plannerConfig, hoverDwellThreshold } = get();
    const planner = plannerConfig.baseUrl && plannerConfig.model
      ? toEndpoint(plannerConfig)
      : null;
    const result = await commands.stopWalkthrough(planner, hoverDwellThreshold);
    if (result.status === "error") {
      pushLog(`Walkthrough stop failed: ${result.error}`);
    }
  },

  cancelWalkthrough: async () => {
    const { pushLog } = get();
    closeRecordingBarWindow();
    set({
      walkthroughEvents: [],
      walkthroughActions: [],
      walkthroughDraft: null,
      walkthroughWarnings: [],
      walkthroughAnnotations: { ...emptyAnnotations },
      walkthroughExpandedAction: null,
      walkthroughActionNodeMap: [],

      walkthroughPanelOpen: false,
    });
    const result = await commands.cancelWalkthrough();
    if (result.status === "error") {
      pushLog(`Walkthrough cancel failed: ${result.error}`);
    }
  },

  setWalkthroughExpandedAction: (id) => set((s) => ({
    walkthroughExpandedAction: s.walkthroughExpandedAction === id ? null : id,
  })),

  keepCandidate: (actionId) => set((s) => {
    const updatedActions = s.walkthroughActions.map((a) =>
      a.id === actionId ? { ...a, candidate: false } : a,
    );

    const action = updatedActions.find((a) => a.id === actionId);
    if (!action || !s.walkthroughDraft || action.kind.type !== "Hover") {
      return { walkthroughActions: updatedActions };
    }

    // Synthesize a node for the kept candidate and insert it into the draft.
    const nodeId = crypto.randomUUID();
    const insertIdx = findCandidateInsertIndex(
      actionId, updatedActions, s.walkthroughActionNodeMap, s.walkthroughDraft.nodes,
    );
    const position = { x: 250, y: insertIdx * 100 };
    const node = synthesizeNodeForKeptCandidate(action, nodeId, position);

    const updatedNodes = [...s.walkthroughDraft.nodes];
    updatedNodes.splice(insertIdx, 0, node);

    return {
      walkthroughActions: updatedActions,
      walkthroughDraft: {
        ...s.walkthroughDraft,
        nodes: recomputeNodePositions(updatedNodes),
      },
      walkthroughActionNodeMap: [
        ...s.walkthroughActionNodeMap,
        { action_id: actionId, node_id: nodeId },
      ],
    };
  }),

  dismissCandidate: (actionId) => set((s) => ({
    walkthroughActions: s.walkthroughActions.filter((a) => a.id !== actionId),
  })),

  deleteNode: (nodeId) => set((s) => ({
    walkthroughAnnotations: {
      ...s.walkthroughAnnotations,
      deleted_node_ids: [...s.walkthroughAnnotations.deleted_node_ids, nodeId],
    },
  })),

  restoreNode: (nodeId) => set((s) => ({
    walkthroughAnnotations: {
      ...s.walkthroughAnnotations,
      deleted_node_ids: s.walkthroughAnnotations.deleted_node_ids.filter((id) => id !== nodeId),
    },
  })),

  renameNode: (nodeId, newName) => set((s) => ({
    walkthroughAnnotations: {
      ...s.walkthroughAnnotations,
      renamed_nodes: upsertAnnotation(s.walkthroughAnnotations.renamed_nodes, { node_id: nodeId, new_name: newName }),
    },
  })),

  overrideTarget: (nodeId, candidateIndex) => set((s) => ({
    walkthroughAnnotations: {
      ...s.walkthroughAnnotations,
      target_overrides: upsertAnnotation(s.walkthroughAnnotations.target_overrides, { node_id: nodeId, chosen_candidate_index: candidateIndex }),
    },
  })),

  promoteToVariable: (nodeId, variableName) => set((s) => ({
    walkthroughAnnotations: {
      ...s.walkthroughAnnotations,
      variable_promotions: upsertAnnotation(s.walkthroughAnnotations.variable_promotions, { node_id: nodeId, variable_name: variableName }),
    },
  })),

  removeVariablePromotion: (nodeId) => set((s) => ({
    walkthroughAnnotations: {
      ...s.walkthroughAnnotations,
      variable_promotions: s.walkthroughAnnotations.variable_promotions.filter((p) => p.node_id !== nodeId),
    },
  })),

  resetAnnotations: () => set({
    walkthroughAnnotations: { ...emptyAnnotations },
    walkthroughExpandedAction: null,
  }),

  applyDraftToCanvas: async () => {
    const { walkthroughDraft, walkthroughActions, walkthroughAnnotations: ann,
            walkthroughActionNodeMap } = get();
    if (!walkthroughDraft) return;

    const { nodes, edges } = applyAnnotationsToDraft(
      walkthroughDraft, ann, walkthroughActions, walkthroughActionNodeMap,
    );

    // Preserve the existing workflow's name and ID instead of clobbering
    // them with the draft's placeholder "Walkthrough Draft" title.
    const { workflow } = get();
    const modifiedDraft: Workflow = { ...walkthroughDraft, id: workflow.id, name: workflow.name, nodes, edges };

    get().pushHistory("Apply Walkthrough");
    get().setWorkflow(modifiedDraft);

    // Seed decision cache.
    seedCache(modifiedDraft, get);

    // Clear the backend session before transitioning to Idle so a new
    // recording started immediately after won't race with the cancel.
    await commands.cancelWalkthrough().catch(() => {});

    set({
      walkthroughStatus: "Idle",
      walkthroughPanelOpen: false,
      walkthroughActions: [],
      walkthroughDraft: null,
      walkthroughWarnings: [],
      walkthroughAnnotations: { ...emptyAnnotations },
      walkthroughExpandedAction: null,
      walkthroughEvents: [],
      walkthroughActionNodeMap: [],

      isNewWorkflow: false,
    });
  },

  discardDraft: async () => {
    // Clear the backend session before transitioning to Idle so a new
    // recording started immediately after won't race with the cancel.
    await commands.cancelWalkthrough().catch(() => {});

    set({
      walkthroughStatus: "Idle",
      walkthroughPanelOpen: false,
      walkthroughActions: [],
      walkthroughDraft: null,
      walkthroughWarnings: [],
      walkthroughAnnotations: { ...emptyAnnotations },
      walkthroughExpandedAction: null,
      walkthroughEvents: [],
      walkthroughActionNodeMap: [],

      walkthroughError: null,
    });
  },
});

/** Seed the decision cache with app resolution entries from the applied workflow. */
async function seedCache(workflow: Workflow, get: () => StoreState) {
  const entries: { node_id: string; app_name: string }[] = [];
  for (const node of workflow.nodes) {
    if (node.node_type.type === "FocusWindow" && node.node_type.value) {
      entries.push({ node_id: node.id, app_name: node.node_type.value });
    }
  }
  if (entries.length === 0) return;

  const { projectPath } = get();
  const result = await commands.seedWalkthroughCache(
    workflow.id,
    workflow.name,
    projectPath ?? null,
    entries,
  );
  if (result.status === "error") {
    console.warn("Cache seeding failed:", result.error);
  }
}
