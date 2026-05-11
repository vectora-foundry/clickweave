import type { StateCreator } from "zustand";
import type { ProjectManifest } from "../../bindings";
import { commands } from "../../bindings";
import { PROJECT_SCHEMA_VERSION } from "../state";
import { errorMessage } from "../../utils/commandError";
import type { StoreState } from "./types";
import { loadAgentChat } from "../agentChatPersistence";
import { loadLatestRunTrace } from "../runTracePersistence";
import { isAgentActive } from "./agentSlice";

export interface ProjectSlice {
  projectId: string;
  projectName: string;
  projectIntent: string | null;
  projectPath: string | null;
  isNewWorkflow: boolean;

  setProjectName: (name: string) => void;
  setProjectIntent: (intent: string | null) => void;
  openProject: () => Promise<void>;
  saveProject: () => Promise<void>;
  newProject: () => void;
  skipIntentEntry: () => void;
}

export const createProjectSlice: StateCreator<StoreState, [], [], ProjectSlice> = (set, get) => ({
  projectId: crypto.randomUUID(),
  projectName: "New Workflow",
  projectIntent: null,
  projectPath: null,
  isNewWorkflow: true,

  setProjectName: (name) => set({ projectName: name }),
  setProjectIntent: (intent) => set({ projectIntent: intent }),

  openProject: async () => {
    if (get().executorState === "running") {
      console.warn("Cannot open project during execution");
      return;
    }
    // Cross-project corruption guard: a live agent run against project A
    // would keep emitting events into project B if we swapped out from
    // under it. Also block while a VLM completion-disagreement resolver is
    // pending — the backend task still owns this project's cache +
    // variant-index writes until the operator resolves.
    if (isAgentActive(get().agentStatus, get().completionDisagreement)) {
      get().setAssistantError(
        "Stop the agent before opening another project.",
      );
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
      projectId: projectResult.data.manifest.id,
      projectName: projectResult.data.manifest.name,
      projectIntent: projectResult.data.manifest.intent ?? null,
      selectedNode: null,
      isNewWorkflow: false,
      assistantError: null,
      messages: [],
      // Clear the previous project's run context so the Overview
      // cards don't display stale trace/elapsed data for the old project.
      agentRunId: null,
      agentRunStartedAt: null,
      agentRunFinishedAt: null,
      lastRunStatus: null,
      // Also clear the terminal run notice — the destructive-cap card
      // in AssistantThread reads this directly and would otherwise keep
      // showing the previous project's run-halted message.
      consecutiveDestructiveCapHit: null,
      // Drop any staged first-run save sheet and its underlying step
      // buffer: a pendingRunSave belongs to the previous project's runId,
      // and saving it after the switch would materialise the old steps as
      // a skill in the *new* project.
      pendingRunSave: null,
      agentSteps: [],
      agentGoal: "",
      skillCreationIntent: false,
    });
    // Ambiguity resolutions are specific to the prior project's nodes.
    get().clearAmbiguityResolutions();

    // Hydrate the per-project chat transcript from disk. Best-effort
    // — missing or malformed files return an empty array.
    const rehydrated = await loadAgentChat({
      projectPath: projectResult.data.path,
      projectName: projectResult.data.manifest.name,
      projectId: projectResult.data.manifest.id,
    });
    if (rehydrated.length > 0) {
      get().setMessages(rehydrated);
    }

    const hydratedTrace = await loadLatestRunTrace({
      projectPath: projectResult.data.path,
      projectName: projectResult.data.manifest.name,
      projectId: projectResult.data.manifest.id,
      storeTraces: get().storeTraces,
    });
    if (hydratedTrace) {
      get().hydrateRunTrace(hydratedTrace);
    }

    pushLog(`Opened: ${filePath}`);
  },

  saveProject: async () => {
    const { projectPath, projectId, projectName, projectIntent, pushLog } = get();
    let savePath = projectPath;
    if (!savePath) {
      const result = await commands.pickSaveFile();
      if (result.status !== "ok" || !result.data) return;
      savePath = result.data;
      set({ projectPath: savePath });
    }
    const manifest: ProjectManifest = {
      id: projectId,
      name: projectName,
      intent: projectIntent ?? null,
      schema_version: PROJECT_SCHEMA_VERSION,
    };
    const saveResult = await commands.saveProject(savePath, manifest);
    if (saveResult.status !== "ok") {
      pushLog(`Failed to save: ${errorMessage(saveResult.error)}`);
      return;
    }

    pushLog(projectPath ? "Saved" : `Saved to: ${savePath}`);
  },

  newProject: () => {
    if (get().executorState === "running") {
      console.warn("Cannot create new project during execution");
      return;
    }
    if (isAgentActive(get().agentStatus, get().completionDisagreement)) {
      get().setAssistantError(
        "Stop the agent before creating a new project.",
      );
      return;
    }
    const { pushLog } = get();
    set({
      projectId: crypto.randomUUID(),
      projectName: "New Workflow",
      projectIntent: null,
      projectPath: null,
      selectedNode: null,
      isNewWorkflow: true,
      messages: [],
      assistantError: null,
      // Clear the previous project's run context so the Overview
      // cards don't display stale trace/elapsed data for the old project.
      agentRunId: null,
      agentRunStartedAt: null,
      agentRunFinishedAt: null,
      lastRunStatus: null,
      // Also clear the terminal run notice — the destructive-cap card
      // in AssistantThread reads this directly and would otherwise keep
      // showing the previous project's run-halted message.
      consecutiveDestructiveCapHit: null,
      // Drop any staged first-run save sheet and its underlying step
      // buffer: a pendingRunSave belongs to the previous project's runId,
      // and saving it after the switch would materialise the old steps as
      // a skill in the *new* project.
      pendingRunSave: null,
      agentSteps: [],
      agentGoal: "",
      skillCreationIntent: false,
    });
    get().clearAmbiguityResolutions();
    pushLog("New project created");
  },

  skipIntentEntry: () => set({ isNewWorkflow: false }),
});
