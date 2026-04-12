import type { StateCreator } from "zustand";
import type { ChatEntry, WorkflowPatch } from "../../bindings";
import { isWalkthroughActive } from "./walkthroughSlice";
import type { StoreState } from "./types";

export interface AssistantSlice {
  /** Display-only message array, populated by assistant://message events. */
  messages: ChatEntry[];
  /** Session ID used to filter incoming assistant events. */
  expectedSessionId: string | null;
  assistantOpen: boolean;
  assistantLoading: boolean;
  assistantRetrying: boolean;
  assistantError: string | null;
  pendingPatch: WorkflowPatch | null;
  pendingPatchWarnings: string[];
  pendingIntent: string | null;
  hasPendingIntent: boolean;
  contextUsage: number | null;

  setAssistantOpen: (open: boolean) => void;
  toggleAssistant: () => void;
  sendAssistantMessage: (message: string) => Promise<void>;
  applyApprovedPatch: () => Promise<void>;
  discardPendingPatch: () => void;
  cancelAssistantChat: () => Promise<void>;
  clearConversation: () => void;
  appendAssistantMessage: (sessionId: string, entry: ChatEntry) => void;
  setExpectedSessionId: (sessionId: string) => void;
  setMessages: (messages: ChatEntry[]) => void;
}

export const createAssistantSlice: StateCreator<StoreState, [], [], AssistantSlice> = (set, get) => ({
  messages: [],
  expectedSessionId: null,
  assistantOpen: false,
  assistantLoading: false,
  assistantRetrying: false,
  assistantError: null,
  pendingPatch: null,
  pendingPatchWarnings: [],
  pendingIntent: null,
  hasPendingIntent: false,
  contextUsage: null,

  setAssistantOpen: (open) => {
    if (open && isWalkthroughActive(get().walkthroughStatus)) {
      const status = get().walkthroughStatus;
      if (status === "Recording" || status === "Paused") {
        get().cancelWalkthrough();
      }
      // Review/Processing: don't discard — just hide the walkthrough panel
      // while the assistant is open. Closing the assistant restores it.
    }
    set({ assistantOpen: open });
  },
  toggleAssistant: () => {
    const opening = !get().assistantOpen;
    if (opening && isWalkthroughActive(get().walkthroughStatus)) {
      const status = get().walkthroughStatus;
      if (status === "Recording" || status === "Paused") {
        get().cancelWalkthrough();
      }
      // Review/Processing: don't discard — just hide the walkthrough panel
      // while the assistant is open. Closing the assistant restores it.
    }
    set({ assistantOpen: opening });
  },

  sendAssistantMessage: async (_message) => {
    set({ assistantLoading: true, assistantError: null, assistantRetrying: false });
    // TODO: Assistant chat backend was removed — use agent mode instead.
    get().pushLog("Assistant not available — use agent mode");
    set({ assistantLoading: false });
  },

  applyApprovedPatch: async () => {
    // No-op: assistant patch workflow was removed with the assistant backend.
  },

  discardPendingPatch: () => {
    set({
      pendingPatch: null,
      pendingPatchWarnings: [],
      pendingIntent: null,
      hasPendingIntent: false,
      assistantError: null,
    });
  },

  cancelAssistantChat: async () => {
    set({ assistantLoading: false, assistantError: null, assistantRetrying: false });
  },

  clearConversation: () => {
    set({
      messages: [],
      expectedSessionId: null,
      pendingPatch: null,
      pendingPatchWarnings: [],
      pendingIntent: null,
      hasPendingIntent: false,
      assistantError: null,
      contextUsage: null,
    });
  },

  appendAssistantMessage: (sessionId, entry) => {
    if (get().expectedSessionId !== null && get().expectedSessionId !== sessionId) return;
    set((s) => ({ messages: [...s.messages, entry] }));
  },

  setExpectedSessionId: (sessionId) => {
    const current = get().expectedSessionId;
    if (current === sessionId) return;
    set({ expectedSessionId: sessionId, messages: current === null ? get().messages : [] });
  },

  setMessages: (messages) => {
    set({ messages });
  },
});
