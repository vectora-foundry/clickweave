import type { StateCreator } from "zustand";
import { commands } from "../../bindings";
import type { WalkthroughAction, WalkthroughAnnotations, Workflow } from "../../bindings";
import type { CdpSetupProgress } from "../../components/CdpAppSelectModal";
import { buildInitialOrder } from "../../utils/walkthroughGrouping";
import { errorMessage } from "../../utils/commandError";
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
  walkthroughNodeOrder: string[];

  setWalkthroughStatus: (status: WalkthroughStatus) => void;
  setWalkthroughPanelOpen: (open: boolean) => void;
  setWalkthroughDraft: (payload: {
    actions: WalkthroughAction[];
    draft: Workflow | null;
    warnings: string[];
    action_node_map: ActionNodeEntry[];
  }) => void;
}

export const createWalkthroughSlice: StateCreator<StoreState, [], [], WalkthroughSlice> = (set) => ({
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
});

/** Seed the decision cache with app resolution entries from the applied workflow. */
export async function seedCache(workflow: Workflow, get: () => StoreState) {
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
