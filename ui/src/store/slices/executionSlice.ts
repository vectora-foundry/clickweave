import type { StateCreator } from "zustand";
import type { ExecutionMode, RunSkillRequest } from "../../bindings";
import { commands } from "../../bindings";
import { validateSingleGraph } from "../../utils/graphValidation";
import { errorMessage } from "../../utils/commandError";
import { toEndpoint } from "../settings";
import type { StoreState } from "./types";

export type SafetyScope =
  | { kind: "skill"; skill_id: string; section_id: string; step_id: string }
  | { kind: "ad_hoc"; run_id: string };

export interface SupervisionPause {
  scope: SafetyScope;
  finding: string;
  screenshot: string | null;
}

/// Skill-scoped supervision pause: overlaid inline on the SkillSectionCard.
export interface SectionApprovalPause {
  scope: Extract<SafetyScope, { kind: "skill" }>;
  finding: string;
  screenshot: string | null;
}

/// Ad-hoc supervision pause: rendered as a card anchored in AssistantThread.
export interface ChatAnchoredApprovalPause {
  scope: Extract<SafetyScope, { kind: "ad_hoc" }>;
  finding: string;
  screenshot: string | null;
}

export interface ExecutionSlice {
  executorState: "idle" | "running";
  executionMode: ExecutionMode;
  supervisionPause: SupervisionPause | null;
  sectionApproval: SectionApprovalPause | null;
  chatAnchoredApproval: ChatAnchoredApprovalPause | null;
  lastRunStatus: "completed" | "failed" | null;

  setExecutorState: (state: "idle" | "running") => void;
  setExecutionMode: (mode: ExecutionMode) => void;
  setSupervisionPause: (pause: SupervisionPause | null) => void;
  clearSupervisionPause: () => void;
  setSectionApproval: (pause: SectionApprovalPause | null) => void;
  setChatAnchoredApproval: (pause: ChatAnchoredApprovalPause | null) => void;
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
  sectionApproval: null,
  chatAnchoredApproval: null,
  lastRunStatus: null,

  setExecutorState: (state) => set({ executorState: state }),
  setLastRunStatus: (status) => set({ lastRunStatus: status }),
  isExecutionLocked: () => get().executorState === "running",
  setExecutionMode: (mode) => set({ executionMode: mode }),
  setSupervisionPause: (pause) => set({ supervisionPause: pause }),
  clearSupervisionPause: () => set({ supervisionPause: null }),
  setSectionApproval: (pause) => set({ sectionApproval: pause }),
  setChatAnchoredApproval: (pause) => set({ chatAnchoredApproval: pause }),

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

    // 1.G TOMBSTONE: graph validation runs against `workflow.nodes/edges`
    // which are deleted with the canvas in 1.G. Kept here so the existing
    // run button keeps validating until 1.F lands the SkillView-driven
    // run path.
    const graphErrors = validateSingleGraph(workflow.nodes, workflow.edges);
    if (graphErrors.length > 0) {
      for (const err of graphErrors) {
        pushLog(`Validation error: ${err}`);
      }
      return;
    }

    // 1.F WIRE-UP: today's "run" button is a temporary stub against the
    // new `run_skill` IPC. Real invocation flows through `SkillView` once
    // it lands in 1.F; until then no caller can succeed at runtime — the
    // backend stub returns `Skill not found` for the placeholder id.
    const request: RunSkillRequest = {
      project_path: projectPath,
      project_id: workflow.id,
      project_name: workflow.name,
      skill_id: "<unimplemented>",
      variables: {},
      agent: toEndpoint(agentConfig),
      fast: fastEnabled ? toEndpoint(fastConfig) : null,
      supervisor: toEndpoint(supervisorConfig),
      execution_mode: executionMode,
      supervision_delay_ms: supervisionDelayMs,
      store_traces: storeTraces,
    };
    const result = await commands.runSkill(request);
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
