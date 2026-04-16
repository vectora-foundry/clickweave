import type { StateCreator } from "zustand";
import type { ExecutionMode, RunRequest } from "../../bindings";
import { commands } from "../../bindings";
import { validateSingleGraph } from "../../utils/graphValidation";
import { errorMessage } from "../../utils/commandError";
import { toEndpoint } from "../settings";
import type { StoreState } from "./types";

export interface SupervisionPause {
  nodeId: string;
  nodeName: string;
  finding: string;
  screenshot: string | null;
}

export interface ExecutionSlice {
  executorState: "idle" | "running";
  executionMode: ExecutionMode;
  supervisionPause: SupervisionPause | null;
  lastRunStatus: "completed" | "failed" | null;

  setExecutorState: (state: "idle" | "running") => void;
  setExecutionMode: (mode: ExecutionMode) => void;
  setSupervisionPause: (pause: SupervisionPause | null) => void;
  clearSupervisionPause: () => void;
  supervisionRespond: (action: "retry" | "skip" | "abort") => Promise<void>;
  runWorkflow: () => Promise<void>;
  stopWorkflow: () => Promise<void>;
  setLastRunStatus: (status: "completed" | "failed" | null) => void;
  isExecutionLocked: () => boolean;
  setIntent: (intent: string | null) => void;
}

export const createExecutionSlice: StateCreator<StoreState, [], [], ExecutionSlice> = (set, get) => ({
  executorState: "idle",
  executionMode: "Test",
  supervisionPause: null,
  lastRunStatus: null,

  setExecutorState: (state) => set({ executorState: state }),
  setLastRunStatus: (status) => set({ lastRunStatus: status }),
  isExecutionLocked: () => get().executorState === "running",
  setExecutionMode: (mode) => set({ executionMode: mode }),
  setSupervisionPause: (pause) => set({ supervisionPause: pause }),
  clearSupervisionPause: () => set({ supervisionPause: null }),

  supervisionRespond: async (action) => {
    const { pushLog } = get();
    set({ supervisionPause: null });
    const result = await commands.supervisionRespond(action);
    if (result.status === "error") {
      pushLog(`Supervision response failed: ${errorMessage(result.error)}`);
    }
  },

  setIntent: (intent) => {
    const { workflow } = get();
    set({ workflow: { ...workflow, intent: intent || null } });
  },

  runWorkflow: async () => {
    const {
      workflow,
      projectPath,
      agentConfig,
      fastConfig,
      fastEnabled,
      supervisorConfig,
      executionMode,
      supervisionDelayMs,
      storeTraces,
      pushLog,
    } = get();

    const graphErrors = validateSingleGraph(workflow.nodes, workflow.edges);
    if (graphErrors.length > 0) {
      for (const err of graphErrors) {
        pushLog(`Validation error: ${err}`);
      }
      return;
    }

    const request: RunRequest = {
      workflow,
      project_path: projectPath,
      agent: toEndpoint(agentConfig),
      fast: fastEnabled ? toEndpoint(fastConfig) : null,
      supervisor: toEndpoint(supervisorConfig),
      execution_mode: executionMode,
      supervision_delay_ms: supervisionDelayMs,
      store_traces: storeTraces,
    };
    const result = await commands.runWorkflow(request);
    if (result.status === "error") {
      pushLog(`Run failed: ${errorMessage(result.error)}`);
    }
  },

  stopWorkflow: async () => {
    const { pushLog } = get();
    const result = await commands.stopWorkflow();
    if (result.status === "error") {
      pushLog(`Stop failed: ${errorMessage(result.error)}`);
    }
  },
});
