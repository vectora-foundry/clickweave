import type { StateCreator } from "zustand";
import type { Workflow } from "../../bindings";
import { commands } from "../../bindings";
import { makeDefaultWorkflow, makeEmptyConversation } from "../state";
import { errorMessage } from "../../utils/commandError";
import type { StoreState } from "./types";

export interface ProjectSlice {
  workflow: Workflow;
  projectPath: string | null;
  isNewWorkflow: boolean;

  setWorkflow: (w: Workflow) => void;
  openProject: () => Promise<void>;
  saveProject: () => Promise<void>;
  newProject: () => void;
  skipIntentEntry: () => void;
}

export const createProjectSlice: StateCreator<StoreState, [], [], ProjectSlice> = (set, get) => ({
  workflow: makeDefaultWorkflow(),
  projectPath: null,
  isNewWorkflow: true,

  setWorkflow: (w) => set({ workflow: w }),

  openProject: async () => {
    if (get().executorState === "running") {
      console.warn("Cannot open project during execution");
      return;
    }
    const { pushLog } = get();
    const result = await commands.pickWorkflowFile();
    if (result.status !== "ok" || !result.data) return;
    const filePath = result.data;
    const projectResult = await commands.openProject(filePath);
    if (projectResult.status !== "ok") {
      pushLog(`Failed to open: ${errorMessage(projectResult.error)}`);
      return;
    }
    set({
      projectPath: projectResult.data.path,
      workflow: projectResult.data.workflow,
      selectedNode: null,
      isNewWorkflow: false,
      pendingPatch: null,
      pendingPatchWarnings: [],
      assistantError: null,
      contextUsage: null,
      conversation: makeEmptyConversation(),
    });
    get().clearHistory();
    await commands.clearAssistantSession().catch(() => {});

    // Load conversation
    try {
      const convResult = await commands.loadConversation(filePath);
      if (convResult.status === "ok" && convResult.data) {
        set({ conversation: convResult.data });
      } else {
        set({ conversation: makeEmptyConversation() });
      }
    } catch {
      set({ conversation: makeEmptyConversation() });
    }

    pushLog(`Opened: ${filePath}`);
  },

  saveProject: async () => {
    const { projectPath, workflow, conversation, pushLog } = get();
    let savePath = projectPath;
    if (!savePath) {
      const result = await commands.pickSaveFile();
      if (result.status !== "ok" || !result.data) return;
      savePath = result.data;
      set({ projectPath: savePath });
    }
    const saveResult = await commands.saveProject(savePath, workflow);
    if (saveResult.status !== "ok") {
      pushLog(`Failed to save: ${errorMessage(saveResult.error)}`);
      return;
    }

    // Save conversation alongside the project
    if (savePath) {
      try {
        await commands.saveConversation(savePath, conversation);
      } catch (e) {
        console.error("Failed to save conversation:", e);
      }
    }

    pushLog(projectPath ? "Saved" : `Saved to: ${savePath}`);
  },

  newProject: () => {
    if (get().executorState === "running") {
      console.warn("Cannot create new project during execution");
      return;
    }
    const { pushLog } = get();
    commands.clearAssistantSession().catch(() => {});
    set({
      workflow: makeDefaultWorkflow(),
      projectPath: null,
      selectedNode: null,
      isNewWorkflow: true,
      conversation: makeEmptyConversation(),
      pendingPatch: null,
      pendingPatchWarnings: [],
      assistantError: null,
      contextUsage: null,
    });
    get().clearHistory();
    pushLog("New project created");
  },

  skipIntentEntry: () => set({ isNewWorkflow: false }),
});
