import type { StateCreator } from "zustand";
import { commands } from "../../bindings";
import type { CdpAppConfig, WalkthroughAction, WalkthroughAnnotations } from "../../bindings";
import type { CdpSetupProgress } from "../../components/CdpAppSelectModal";
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

/** Returns true when live capture is active — Processing intentionally excluded
 * so the drain-phase events emitted after Stop don't bump visible counters. */
export function isWalkthroughCapturing(status: WalkthroughStatus): boolean {
  return status === "Recording" || status === "Paused";
}

/** Opaque captured event from the backend (serialized WalkthroughEvent). */
export type WalkthroughCapturedEvent = Record<string, unknown>;

/** Maps a walkthrough action to its corresponding workflow node. */
export interface ActionNodeEntry {
  action_id: string;
  node_id: string;
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
  walkthroughSessionId: string | null;
  walkthroughEvents: WalkthroughCapturedEvent[];
  walkthroughActions: WalkthroughAction[];
  walkthroughWarnings: string[];
  walkthroughAnnotations: WalkthroughAnnotations;
  /** Whether the WalkthroughSaveSheet overlay is visible. */
  walkthroughSaveSheetOpen: boolean;
  walkthroughCdpModalOpen: boolean;
  walkthroughCdpProgress: CdpSetupProgress[];

  // ── Core actions ──
  setWalkthroughStatus: (status: WalkthroughStatus) => void;
  setWalkthroughPanelOpen: (open: boolean) => void;
  setWalkthroughSaveSheetOpen: (open: boolean) => void;
  setWalkthroughDraft: (payload: {
    session_id: string;
    actions: WalkthroughAction[];
    warnings: string[];
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

  // ── Review actions (kept for walkthrough events wiring) ──
  deleteNode: (nodeId: string) => void;
  restoreNode: (nodeId: string) => void;
  renameNode: (nodeId: string, newName: string) => void;
  resetAnnotations: () => void;
  discardDraft: () => Promise<void>;
}

// ── Slice creator ───────────────────────────────────────────────

export const createWalkthroughSlice: StateCreator<StoreState, [], [], WalkthroughSlice> = (set, get) => ({
  // ── Initial state ──
  walkthroughStatus: "Idle",
  walkthroughPanelOpen: false,
  walkthroughError: null,
  walkthroughSessionId: null,
  walkthroughEvents: [],
  walkthroughActions: [],
  walkthroughWarnings: [],
  walkthroughAnnotations: { ...emptyAnnotations },
  walkthroughSaveSheetOpen: false,
  walkthroughCdpModalOpen: false,
  walkthroughCdpProgress: [],

  // ── Core actions ──

  setWalkthroughStatus: (status) => {
    set({ walkthroughStatus: status });
    // Close the recording bar window when leaving recording states
    if (status === "Processing" || status === "Review" || status === "Idle" || status === "Cancelled" || status === "Applied") {
      closeRecordingBarWindow();
    }
    // Open the save sheet when processing completes (entering Review)
    if (status === "Review") {
      set({ walkthroughSaveSheetOpen: true });
    }
  },
  setWalkthroughPanelOpen: (open) => set({ walkthroughPanelOpen: open }),
  setWalkthroughSaveSheetOpen: (open) => set({ walkthroughSaveSheetOpen: open }),

  setWalkthroughDraft: ({ session_id, actions, warnings }) => set({
    walkthroughSessionId: session_id,
    walkthroughActions: actions,
    walkthroughWarnings: warnings,
    walkthroughStatus: "Review",
    walkthroughAnnotations: { ...emptyAnnotations },
  }),

  // ── Recording actions ──

  pushWalkthroughEvent: (event) => set((s) => {
    // Drop events received outside active capture — the backend keeps
    // emitting hover/CDP drain events after the Processing transition and
    // they must not bump the live counter the user sees after Stop.
    if (!isWalkthroughCapturing(s.walkthroughStatus)) return {};
    return { walkthroughEvents: [...s.walkthroughEvents, event] };
  }),

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
    const { projectId, projectPath, pushLog, supervisorConfig } = get();
    // Flip to Recording optimistically so the pushWalkthroughEvent guard
    // accepts events from the backend — the capture processing task is
    // spawned before the backend's emit_state(Recording), so the first
    // captured events can land before the state transition arrives.
    set({
      walkthroughStatus: "Recording",
      walkthroughError: null,
      walkthroughSessionId: null,
      walkthroughEvents: [],
      walkthroughAnnotations: { ...emptyAnnotations },
      walkthroughSaveSheetOpen: false,
      walkthroughCdpProgress: [],

      // P2.H2 — legacy bare-boolean removed; drive the surface enum so any
      // open drawer hides while the walkthrough records.
      assistantSurface: null,
    });
    const supervisor = supervisorConfig.baseUrl && supervisorConfig.model
      ? toEndpoint(supervisorConfig)
      : null;
    const { hoverDwellThreshold } = get();
    const result = await commands.startWalkthrough(projectId, projectPath ?? null, supervisor, cdpApps, hoverDwellThreshold);
    if (result.status === "error") {
      const msg = errorMessage(result.error);
      set({ walkthroughStatus: "Idle", walkthroughError: msg, walkthroughCdpModalOpen: false });
      pushLog(`Walkthrough start failed: ${msg}`);
      return;
    }
    // If the user cancelled during the optimistic window the local status
    // will have been flipped away from "Recording"; the backend cancel call
    // issued then may have hit "No walkthrough session is active" because
    // start_walkthrough hadn't installed guard.session yet. Retry the cancel
    // now that the session is live so we don't leak a recording the user
    // already asked to abandon.
    if (get().walkthroughStatus !== "Recording") {
      const cancelResult = await commands.cancelWalkthrough();
      if (cancelResult.status === "error") {
        pushLog(`Walkthrough cancel after start failed: ${errorMessage(cancelResult.error)}`);
      }
      return;
    }
    openRecordingBarWindow();
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
    const { pushLog, supervisorConfig, hoverDwellThreshold } = get();
    const supervisor = supervisorConfig.baseUrl && supervisorConfig.model
      ? toEndpoint(supervisorConfig)
      : null;
    const result = await commands.stopWalkthrough(supervisor, hoverDwellThreshold);
    if (result.status === "error") {
      pushLog(`Walkthrough stop failed: ${errorMessage(result.error)}`);
    }
  },

  cancelWalkthrough: async () => {
    const { pushLog } = get();
    closeRecordingBarWindow();
    // Flip off capture locally so drain-phase walkthrough://event entries
    // emitted by the backend while cancel_walkthrough awaits the processing
    // task don't repopulate the freshly cleared walkthroughEvents array.
    set({
      walkthroughStatus: "Processing",
      walkthroughSessionId: null,
      walkthroughEvents: [],
      walkthroughActions: [],
      walkthroughWarnings: [],
      walkthroughAnnotations: { ...emptyAnnotations },
      walkthroughSaveSheetOpen: false,
      walkthroughPanelOpen: false,
    });
    const result = await commands.cancelWalkthrough();
    if (result.status === "error") {
      pushLog(`Walkthrough cancel failed: ${errorMessage(result.error)}`);
      // Backend won't emit a state event for an errored cancel, so settle
      // the UI back to Idle ourselves — most commonly "No walkthrough
      // session is active" when Escape races the start-command RPC.
      set({ walkthroughStatus: "Idle" });
    }
  },

  // ── Review actions ──

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

  renameNode: (nodeId, newName) => set((s) => {
    const arr = s.walkthroughAnnotations.renamed_nodes;
    const idx = arr.findIndex((item) => item.node_id === nodeId);
    const updated = idx >= 0
      ? arr.map((item, i) => (i === idx ? { node_id: nodeId, new_name: newName } : item))
      : [...arr, { node_id: nodeId, new_name: newName }];
    return {
      walkthroughAnnotations: {
        ...s.walkthroughAnnotations,
        renamed_nodes: updated,
      },
    };
  }),

  resetAnnotations: () => set({
    walkthroughAnnotations: { ...emptyAnnotations },
  }),

  discardDraft: async () => {
    // Clear the backend session before transitioning to Idle so a new
    // recording started immediately after won't race with the cancel.
    await commands.cancelWalkthrough().catch(() => {});

    set({
      walkthroughStatus: "Idle",
      walkthroughPanelOpen: false,
      walkthroughSaveSheetOpen: false,
      walkthroughSessionId: null,
      walkthroughActions: [],
      walkthroughWarnings: [],
      walkthroughAnnotations: { ...emptyAnnotations },
      walkthroughEvents: [],
      walkthroughError: null,
    });
  },
});
