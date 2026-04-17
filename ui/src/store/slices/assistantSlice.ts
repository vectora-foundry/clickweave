import type { StateCreator } from "zustand";
import { isWalkthroughActive } from "./walkthroughSlice";
import type { StoreState } from "./types";

export interface AssistantMessage {
  role: "user" | "assistant";
  content: string;
  timestamp: string;
}

export interface AssistantSlice {
  messages: AssistantMessage[];
  assistantOpen: boolean;
  assistantError: string | null;

  setAssistantOpen: (open: boolean) => void;
  toggleAssistant: () => void;
  setAssistantError: (error: string | null) => void;
  pushAssistantMessage: (role: AssistantMessage["role"], content: string) => void;
}

export const createAssistantSlice: StateCreator<StoreState, [], [], AssistantSlice> = (set, get) => ({
  messages: [],
  assistantOpen: false,
  assistantError: null,

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

  setAssistantError: (error) => set({ assistantError: error }),

  pushAssistantMessage: (role, content) => {
    const trimmed = content.trim();
    if (!trimmed) return;
    set((s) => ({
      messages: [
        ...s.messages,
        { role, content: trimmed, timestamp: new Date().toISOString() },
      ],
    }));
  },
});
