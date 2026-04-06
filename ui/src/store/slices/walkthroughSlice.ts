import type { StateCreator } from "zustand";
import { commands } from "../../bindings";
import type { CdpAppConfig, WalkthroughAction, WalkthroughAnnotations, Workflow } from "../../bindings";
import type { CdpSetupProgress } from "../../components/CdpAppSelectModal";
import { applyAnnotationsToDraft, findCandidateInsertIndex, recomputeNodePositions, synthesizeNodeForKeptCandidate } from "../../utils/walkthroughDraft";
import { buildInitialOrder, computeAppGroups } from "../../utils/walkthroughGrouping";
import { errorMessage } from "../../utils/commandError";
import { toEndpoint } from "../settings";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { currentMonitor } from "@tauri-apps/api/window";
import type { StoreState } from "./types";

export type WalkthroughStatus = "Idle" | "Recording" | "Paused" | "Processing" | "Review" | "Applied" | "Cancelled";

/** Returns true when the walkthrough panel is visible and active (not in a terminal state). */
export function isWalkthroughActive(status: WalkthroughStatus): boolean {
  return status !== "Idle" && status !== "Applied" && status !== "Cancelled";
}

/** Returns true when the walkthrough is recording or processing (canvas should be non-interactive). */
export function isWalkthroughBusy(status: WalkthroughStatus): boolean {
  return status === "Recording" || status === "Paused" || status === "Processing";
}

/** Build a lookup map from node_id -> WalkthroughAction using the action-node map. */
export function buildActionByNodeId(
  actionNodeMap: ActionNodeEntry[],
  actions: WalkthroughAction[],
): Map<string, WalkthroughAction> {
  const actionById = new Map(actions.map((a) => [a.id, a]));
  const map = new Map<string, WalkthroughAction>();
  for (const entry of actionNodeMap) {
    const action = actionById.get(entry.action_id);
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
export function upsertAnnotation<T extends { node_id: string }>(arr: T[], entry: T): T[] {
  const idx = arr.findIndex((item) => item.node_id === entry.node_id);
  return idx >= 0 ? arr.map((item, i) => (i === idx ? entry : item)) : [...arr, entry];
}

// ── Recording bar window management ─────────────────────────────

const RECORDING_BAR_LABEL = "recording-bar";
const BAR_WIDTH = 460;
const BAR_HEIGHT = 48;

export async function openRecordingBarWindow() {
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

export async function closeRecordingBarWindow() {
  const win = await WebviewWindow.getByLabel(RECORDING_BAR_LABEL);
  if (win) await win.close();
  // Bring the main window back to front
  const main = await WebviewWindow.getByLabel("main");
  if (main) await main.setFocus();
}

// ── Cache seeding ───────────────────────────────────────────────

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
    console.warn("Cache seeding failed:", errorMessage(result.error));
  }
}

// ── Shared empty annotations constant ───────────────────────────

const emptyAnnotations: WalkthroughAnnotations = {
  deleted_node_ids: [],
  renamed_nodes: [],
  target_overrides: [],
  variable_promotions: [],
};

// ── Consolidated slice interface ────────────────────────────────

export interface WalkthroughSlice {
  // ── State ──
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
  walkthroughNodeOrder: string[];

  // ── Core actions ──
  setWalkthroughStatus: (status: WalkthroughStatus) => void;
  setWalkthroughPanelOpen: (open: boolean) => void;
  setWalkthroughDraft: (payload: {
    actions: WalkthroughAction[];
    draft: Workflow | null;
    warnings: string[];
    action_node_map: ActionNodeEntry[];
  }) => void;

  // ── Recording actions ──
  pushWalkthroughEvent: (event: WalkthroughCapturedEvent) => void;
  pushCdpProgress: (progress: CdpSetupProgress) => void;
  openCdpModal: () => void;
  closeCdpModal: () => void;
  startWalkthrough: (cdpApps?: CdpAppConfig[]) => Promise<void>;
  pauseWalkthrough: () => Promise<void>;
  resumeWalkthrough: () => Promise<void>;
  stopWalkthrough: () => Promise<void>;
  cancelWalkthrough: () => Promise<void>;

  // ── Review actions ──
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
  reorderNode: (fromIndex: number, toIndex: number) => void;
  reorderGroup: (fromGroupIndex: number, toGroupIndex: number) => void;
  applyDraftToCanvas: () => Promise<void>;
  discardDraft: () => Promise<void>;
}

// ── Slice creator ───────────────────────────────────────────────

export const createWalkthroughSlice: StateCreator<StoreState, [], [], WalkthroughSlice> = (set, get) => ({
  // ── Initial state ──
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
  walkthroughNodeOrder: [],

  // ── Core actions ──

  setWalkthroughStatus: (status) => {
    set({ walkthroughStatus: status });
    // Close the recording bar window when leaving recording states
    if (status === "Processing" || status === "Review" || status === "Idle" || status === "Cancelled" || status === "Applied") {
      closeRecordingBarWindow();
    }
  },
  setWalkthroughPanelOpen: (open) => set({ walkthroughPanelOpen: open }),

  setWalkthroughDraft: ({ actions, draft, warnings, action_node_map }) => set({
    walkthroughActions: actions,
    walkthroughDraft: draft,
    walkthroughWarnings: warnings,
    walkthroughActionNodeMap: action_node_map,
    walkthroughStatus: "Review",
    walkthroughPanelOpen: true,
    walkthroughAnnotations: { ...emptyAnnotations },
    walkthroughNodeOrder: draft ? buildInitialOrder(actions, draft.nodes, action_node_map) : [],
  }),

  // ── Recording actions ──

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

  openCdpModal: () => set({ walkthroughCdpModalOpen: true, walkthroughCdpProgress: [] }),
  closeCdpModal: () => set({ walkthroughCdpModalOpen: false }),

  startWalkthrough: async (cdpApps: CdpAppConfig[] = []) => {
    const { workflow, projectPath, pushLog, plannerConfig } = get();
    set({
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
    const result = await commands.startWalkthrough(workflow.id, projectPath ?? null, planner, cdpApps, hoverDwellThreshold);
    if (result.status === "error") {
      const msg = errorMessage(result.error);
      set({ walkthroughError: msg, walkthroughCdpModalOpen: false });
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
    set({
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

  // ── Review actions ──

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
      walkthroughNodeOrder: (() => {
        // Replace candidate action ID with the new node ID
        const order = s.walkthroughNodeOrder.map((id) =>
          id === actionId ? nodeId : id,
        );
        // Ensure the kept node is positioned after all same-app anchors
        const appName = action.app_name;
        if (appName) {
          const ANCHOR_KINDS = new Set(["FocusWindow", "LaunchApp"]);
          const actionByNid = buildActionByNodeId(s.walkthroughActionNodeMap, updatedActions);
          const nodeIdx = order.indexOf(nodeId);
          let lastAnchorIdx = -1;
          for (let i = 0; i < order.length; i++) {
            const a = actionByNid.get(order[i]) ?? updatedActions.find((x) => x.id === order[i]);
            if (a && a.app_name === appName && ANCHOR_KINDS.has(a.kind.type)) lastAnchorIdx = i;
          }
          if (nodeIdx >= 0 && lastAnchorIdx >= 0 && nodeIdx < lastAnchorIdx) {
            order.splice(nodeIdx, 1);
            order.splice(lastAnchorIdx, 0, nodeId);
          }
        }
        return order;
      })(),
    };
  }),

  dismissCandidate: (actionId) => set((s) => ({
    walkthroughActions: s.walkthroughActions.filter((a) => a.id !== actionId),
    walkthroughNodeOrder: s.walkthroughNodeOrder.filter((id) => id !== actionId),
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

  reorderNode: (fromIndex, toIndex) => set((s) => {
    const order = [...s.walkthroughNodeOrder];
    const [moved] = order.splice(fromIndex, 1);
    order.splice(toIndex, 0, moved);
    return { walkthroughNodeOrder: order };
  }),

  reorderGroup: (fromGroupIndex, toGroupIndex) => set((s) => {
    if (!s.walkthroughDraft) return {};
    const groups = computeAppGroups(
      s.walkthroughNodeOrder, s.walkthroughDraft.nodes,
      s.walkthroughActions, s.walkthroughActionNodeMap,
    );
    if (fromGroupIndex < 0 || fromGroupIndex >= groups.length) return {};
    // toGroupIndex === groups.length means "append to end"
    if (toGroupIndex < 0 || toGroupIndex > groups.length) return {};

    // Extract the flat ID ranges for each group (includes deleted items),
    // reorder, and flatten back.
    const groupIdRanges: string[][] = groups.map((g) => g.items.map((item) => item.id));
    const [movedRange] = groupIdRanges.splice(fromGroupIndex, 1);
    // Compensate for source removal on downward moves
    const insertAt = fromGroupIndex < toGroupIndex ? toGroupIndex - 1 : toGroupIndex;
    groupIdRanges.splice(insertAt, 0, movedRange);

    return { walkthroughNodeOrder: groupIdRanges.flat() };
  }),

  applyDraftToCanvas: async () => {
    const { walkthroughDraft, walkthroughActions, walkthroughAnnotations: ann,
            walkthroughActionNodeMap } = get();
    if (!walkthroughDraft) return;

    const { nodes, edges } = applyAnnotationsToDraft(
      walkthroughDraft, ann, walkthroughActions, walkthroughActionNodeMap,
      get().walkthroughNodeOrder,
    );

    // Preserve the existing workflow's name and ID instead of clobbering
    // them with the draft's placeholder "Walkthrough Draft" title.
    const { workflow } = get();
    const modifiedDraft: Workflow = {
      ...walkthroughDraft,
      id: workflow.id,
      name: workflow.name,
      nodes,
      edges,
      auto_approve_resolutions: workflow.auto_approve_resolutions,
    };

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
      walkthroughNodeOrder: [],

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
      walkthroughNodeOrder: [],

      walkthroughError: null,
    });
  },
});
