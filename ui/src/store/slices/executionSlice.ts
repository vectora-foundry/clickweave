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
}

export const createExecutionSlice: StateCreator<StoreState, [], [], ExecutionSlice> = (set, get) => ({
  executorState: "idle",
  executionMode: "Test",
  supervisionPause: null,
  lastRunStatus: null,

  setExecutorState: (state) => set({ executorState: state }),
  setLastRunStatus: (status) => set({ lastRunStatus: status }),
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

  runWorkflow: async () => {
    const { workflow, projectPath, agentConfig, vlmConfig, vlmEnabled, plannerConfig, mcpCommand, executionMode, pushLog } = get();

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
      vlm: vlmEnabled ? toEndpoint(vlmConfig) : null,
      planner: toEndpoint(plannerConfig),
      mcp_command: mcpCommand,
      execution_mode: executionMode,
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
