import type { StateCreator } from "zustand";
import type { Workflow } from "../../bindings";
import { commands } from "../../bindings";
import { makeDefaultWorkflow } from "../state";
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
      messages: [],
      expectedSessionId: null,
      autoApprovedCount: 0,
    });
    get().clearHistory();
    await commands.clearAssistantSession().catch(() => {});

    // Load conversation messages from the backend session
    try {
      const convResult = await commands.loadConversation(filePath);
      if (convResult.status === "ok" && convResult.data) {
        get().setMessages(convResult.data);
      }
    } catch {
      // Conversation load failed — messages stay empty
    }

    pushLog(`Opened: ${filePath}`);
  },

  saveProject: async () => {
    const { projectPath, workflow, pushLog } = get();
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

    // Save conversation from backend session alongside the project
    if (savePath) {
      try {
        await commands.saveConversation(savePath);
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
      messages: [],
      expectedSessionId: null,
      pendingPatch: null,
      pendingPatchWarnings: [],
      assistantError: null,
      contextUsage: null,
      autoApprovedCount: 0,
    });
    get().clearHistory();
    pushLog("New project created");
  },

  skipIntentEntry: () => set({ isNewWorkflow: false }),
});
