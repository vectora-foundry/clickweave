import type { StateCreator } from "zustand";
import type { Edge, ExecutionMode, RunRequest, WorkflowPatch } from "../../bindings";
import { commands } from "../../bindings";
import { invoke } from "@tauri-apps/api/core";
import { validateSingleGraph } from "../../utils/graphValidation";
import { errorMessage } from "../../utils/commandError";
import { autoDissolveGroups } from "../useWorkflowMutations";
import { toEndpoint } from "../settings";
import type { StoreState } from "./types";

export interface SupervisionPause {
  nodeId: string;
  nodeName: string;
  finding: string;
  screenshot: string | null;
}

export interface ResolutionProposal {
  nodeId: string;
  nodeName: string;
  reason: string;
  patch: WorkflowPatch;
  screenshot?: string;
}

export interface ExecutionSlice {
  executorState: "idle" | "running";
  executionMode: ExecutionMode;
  supervisionPause: SupervisionPause | null;
  resolutionProposal: ResolutionProposal | null;
  lastRunStatus: "completed" | "failed" | null;

  setExecutorState: (state: "idle" | "running") => void;
  setExecutionMode: (mode: ExecutionMode) => void;
  setSupervisionPause: (pause: SupervisionPause | null) => void;
  clearSupervisionPause: () => void;
  supervisionRespond: (action: "retry" | "skip" | "abort") => Promise<void>;
  resolveResolution: (approved: boolean) => Promise<void>;
  applyRuntimePatch: (patch: WorkflowPatch) => void;
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
  resolutionProposal: null,
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

  resolveResolution: async (approved) => {
    set({ resolutionProposal: null });
    try {
      await invoke("resolution_respond", { approved });
    } catch (e) {
      get().pushLog(`Resolution response failed: ${e}`);
    }
  },

  applyRuntimePatch: (patch) => {
    const { workflow } = get();
    const edgeKey = (e: Edge) => `${e.from}-${e.to}`;
    const removedIds = new Set(patch.removed_node_ids);
    const removedEdgeKeys = new Set(patch.removed_edges.map(edgeKey));
    const nodes = [
      ...workflow.nodes
        .filter((n) => !removedIds.has(n.id))
        .map((n) => patch.updated_nodes.find((u) => u.id === n.id) ?? n),
      ...patch.added_nodes,
    ];
    const edges = [
      ...workflow.edges.filter((e) => !removedEdgeKeys.has(edgeKey(e))),
      ...patch.added_edges,
    ];
    const cleanedGroups = autoDissolveGroups(
      (workflow.groups ?? []).map((g) => ({
        ...g,
        node_ids: g.node_ids.filter((id: string) => !removedIds.has(id)),
      })),
    );
    const patchedCounters = { ...(workflow.next_id_counters ?? {}) } as Record<string, number>;
    for (const node of nodes) {
      if (!node.auto_id) continue;
      const idx = node.auto_id.lastIndexOf("_");
      if (idx === -1) continue;
      const base = node.auto_id.slice(0, idx);
      const num = parseInt(node.auto_id.slice(idx + 1), 10);
      if (!isNaN(num) && num > (patchedCounters[base] ?? 0)) {
        patchedCounters[base] = num;
      }
    }
    set({
      workflow: { ...workflow, nodes, edges, groups: cleanedGroups, next_id_counters: patchedCounters },
    });
  },

  runWorkflow: async () => {
    const { workflow, projectPath, agentConfig, fastConfig, fastEnabled, supervisorConfig, executionMode, supervisionDelayMs, pushLog } = get();

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
