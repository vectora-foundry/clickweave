import type { StateCreator } from "zustand";
import type { DetailTab } from "../state";
import type { NodeTypeInfo } from "../../bindings";
import { commands } from "../../bindings";
import type { StoreState } from "./types";

export interface UiSlice {
  selectedNode: string | null;
  activeNode: string | null;
  detailTab: DetailTab;
  sidebarCollapsed: boolean;
  logsDrawerOpen: boolean;
  nodeSearch: string;
  showSettings: boolean;
  allowAiTransforms: boolean;
  allowAgentSteps: boolean;
  nodeTypes: NodeTypeInfo[];
  _nodeTypesLoaded: boolean;
  /**
   * True when the canvas has a selection (one or more nodes, including group
   * containers) that is NOT represented by `selectedNode`. `selectedNode`
   * only tracks a single-workflow-node selection — everything else (groups,
   * or 2+ nodes) lives purely in React Flow state, so this flag lets the
   * Escape handler know there is still an on-canvas selection to clear.
   */
  hasCanvasSelection: boolean;
  /**
   * Incrementing tick that `useNodeSync` watches to deselect every RF node
   * without threading an imperative handle up to `useEscapeKey`.
   */
  canvasSelectionResetTick: number;

  selectNode: (id: string | null) => void;
  setActiveNode: (id: string | null) => void;
  setDetailTab: (tab: DetailTab) => void;
  toggleSidebar: () => void;
  toggleLogsDrawer: () => void;
  setNodeSearch: (s: string) => void;
  setShowSettings: (show: boolean) => void;
  setAllowAiTransforms: (allow: boolean) => void;
  setAllowAgentSteps: (allow: boolean) => void;
  loadNodeTypes: () => void;
  setHasCanvasSelection: (has: boolean) => void;
  clearCanvasSelection: () => void;
}

export const createUiSlice: StateCreator<StoreState, [], [], UiSlice> = (set, get) => ({
  selectedNode: null,
  activeNode: null,
  detailTab: "setup" as DetailTab,
  sidebarCollapsed: false,
  logsDrawerOpen: false,
  nodeSearch: "",
  showSettings: false,
  allowAiTransforms: true,
  allowAgentSteps: false,
  nodeTypes: [],
  _nodeTypesLoaded: false,
  hasCanvasSelection: false,
  canvasSelectionResetTick: 0,

  selectNode: (id) => set({ selectedNode: id }),
  setActiveNode: (id) => set({ activeNode: id }),
  setDetailTab: (tab) => set({ detailTab: tab }),
  toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
  toggleLogsDrawer: () => set((s) => ({ logsDrawerOpen: !s.logsDrawerOpen })),
  setNodeSearch: (s) => set({ nodeSearch: s }),
  setShowSettings: (show) => set({ showSettings: show }),
  setAllowAiTransforms: (allow) => set({ allowAiTransforms: allow }),
  setAllowAgentSteps: (allow) => set({ allowAgentSteps: allow }),

  loadNodeTypes: () => {
    if (get()._nodeTypesLoaded) return;
    set({ _nodeTypesLoaded: true });
    commands
      .nodeTypeDefaults()
      .then((types) => set({ nodeTypes: types }))
      .catch((e) => console.error("Failed to load node type defaults:", e));
  },

  setHasCanvasSelection: (has) => {
    if (get().hasCanvasSelection === has) return;
    set({ hasCanvasSelection: has });
  },
  clearCanvasSelection: () =>
    set((s) => ({
      selectedNode: null,
      hasCanvasSelection: false,
      canvasSelectionResetTick: s.canvasSelectionResetTick + 1,
    })),
});
