import type { StateCreator } from "zustand";
import { commands } from "../../bindings";
import type { Workflow } from "../../bindings";
import { applyAnnotationsToDraft, findCandidateInsertIndex, recomputeNodePositions, synthesizeNodeForKeptCandidate } from "../../utils/walkthroughDraft";
import { computeAppGroups } from "../../utils/walkthroughGrouping";
import type { StoreState } from "./types";
import { buildActionByNodeId, seedCache, upsertAnnotation } from "./walkthroughSlice";

export interface WalkthroughReviewSlice {
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

const emptyAnnotations = {
  deleted_node_ids: [] as string[],
  renamed_nodes: [] as { node_id: string; new_name: string }[],
  target_overrides: [] as { node_id: string; chosen_candidate_index: number }[],
  variable_promotions: [] as { node_id: string; variable_name: string }[],
};

export const createWalkthroughReviewSlice: StateCreator<StoreState, [], [], WalkthroughReviewSlice> = (set, get) => ({
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
