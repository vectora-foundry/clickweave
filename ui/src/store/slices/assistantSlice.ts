import type { StateCreator } from "zustand";
import { isWalkthroughActive } from "./walkthroughSlice";
import type { StoreState } from "./types";
import { commands } from "../../bindings";
import type { BoundaryKind, Phase, TaskState, WorldModelDiff } from "../../bindings";
import { saveAgentChat } from "../agentChatPersistence";
import { autoDissolveGroups } from "../useWorkflowMutations";

// Flag consumed by `saveAgentChat` in `agentChatPersistence.ts` to
// short-circuit writes after Clear begins but before the file is
// removed. Module-scoped so fire-and-forget `void saveAgentChat(...)`
// callers see the latest value when their promise runs.
let conversationWipeInProgress = false;
export function isConversationWipeInProgress(): boolean {
  return conversationWipeInProgress;
}

export interface AssistantMessage {
  role: "user" | "assistant" | "system";
  content: string;
  timestamp: string;
  /**
   * Run-generation ID this message belongs to. Present for user and
   * assistant messages that bracket a single agent turn; `undefined`
   * for system annotations (e.g., deletion notes).
   */
  runId?: string;
}

export type AgentPhase = Phase;

export interface TraceStep {
  stepIndex: number;
  toolName: string;
  phase: AgentPhase;
  body: string;
  failed: boolean;
}

export interface WorldModelDelta {
  stepIndex: number;
  changedFields: string[];
}

export type MilestoneKind = Extract<
  BoundaryKind,
  "subgoal_completed" | "recovery_succeeded"
>;

export interface TraceMilestone {
  stepIndex: number;
  kind: MilestoneKind;
  text: string;
}

export interface TerminalFrame {
  kind: "complete" | "stopped" | "error" | "disagreement_cancelled";
  detail: string;
}

export interface RunTrace {
  runId: string;
  phase: AgentPhase;
  activeSubgoal: string;
  steps: TraceStep[];
  worldModelDeltas: WorldModelDelta[];
  milestones: TraceMilestone[];
  terminalFrame: TerminalFrame | null;
}

export interface AssistantSlice {
  messages: AssistantMessage[];
  assistantError: string | null;
  runTraces: Record<string, RunTrace>;

  /**
   * D21 — `setAssistantOpen` / `toggleAssistant` no longer mutate a bare
   * boolean. They now drive `assistantSurface` (lives on `uiSlice`):
   *  - On Overview both actions are no-ops (the embedded card is always
   *    live, so toggling has no surface to act on).
   *  - On Canvas they flip `assistantSurface` between `"drawer"` and
   *    `null`.
   * The walkthrough cancellation side-effect (Recording / Paused) is
   * preserved on Canvas only.
   */
  setAssistantOpen: (open: boolean) => void;
  toggleAssistant: () => void;
  setAssistantError: (error: string | null) => void;
  pushAssistantMessage: (
    role: "user" | "assistant",
    content: string,
    runId?: string,
  ) => void;
  /** Append a centered, muted system annotation (deletion notes). */
  pushSystemAnnotation: (content: string) => void;
  /** Wipe all messages in memory. Used by Clear conversation. */
  clearConversation: () => void;
  /** Replace the full messages array (used by agent_chat.json hydrate). */
  setMessages: (messages: AssistantMessage[]) => void;
  /**
   * Update any user/assistant message whose `runId` is in `runIds`.
   * The callback receives the existing message and returns a new one
   * (used for redacting partial-turn summaries). System messages are
   * never touched.
   */
  mapMessagesByRunIds: (
    runIds: Set<string>,
    fn: (msg: AssistantMessage) => AssistantMessage,
  ) => void;
  /** Drop user/assistant messages whose runId is in `runIds`. System annotations survive. */
  dropTurnsByRunIds: (runIds: Set<string>) => void;
  applyTaskStateUpdate: (runId: string, taskState: TaskState) => void;
  applyWorldModelDelta: (runId: string, diff: WorldModelDiff) => void;
  applyBoundary: (
    runId: string,
    kind: BoundaryKind,
    stepIndex: number,
    milestoneText: string | null,
  ) => void;
  pushTraceStep: (runId: string, step: TraceStep) => void;
  setTerminalFrame: (runId: string, frame: TerminalFrame) => void;
  clearTrace: (runId: string) => void;
  /**
   * Drop a fully-built trace (hydrated from disk) into `runTraces` and
   * point `agentRunId` at it. Only applied when no live run is active
   * — a hydrated trace must never override a fresh `agent://started`.
   */
  hydrateRunTrace: (trace: RunTrace) => void;
  /**
   * Full Clear-conversation flow (D1.C1): delete every agent-built
   * node, wipe the cache + variant-index + transcript files via the
   * Tauri command, and empty the local messages array. Not undoable.
   */
  clearConversationFlow: () => Promise<void>;
}

const defaultPhase: AgentPhase = "exploring";

function emptyRunTrace(runId: string): RunTrace {
  return {
    runId,
    phase: defaultPhase,
    activeSubgoal: "",
    steps: [],
    worldModelDeltas: [],
    milestones: [],
    terminalFrame: null,
  };
}

function traceWith(runTraces: Record<string, RunTrace>, runId: string): RunTrace {
  return runTraces[runId] ?? emptyRunTrace(runId);
}

function nextTraceStepIndex(trace: RunTrace): number {
  if (trace.steps.length === 0) return 0;
  return Math.max(...trace.steps.map((step) => step.stepIndex)) + 1;
}

function boundaryMilestone(
  kind: BoundaryKind,
  stepIndex: number,
  milestoneText: string | null,
): TraceMilestone | null {
  if (kind === "subgoal_completed") {
    return {
      stepIndex,
      kind,
      text: milestoneText?.trim() || "Subgoal completed",
    };
  }
  if (kind === "recovery_succeeded") {
    return {
      stepIndex,
      kind,
      text: milestoneText?.trim() || "Recovery succeeded",
    };
  }
  return null;
}

export const createAssistantSlice: StateCreator<
  StoreState,
  [],
  [],
  AssistantSlice
> = (set, get) => {
  // Helper: persist the current transcript to `agent_chat.json` via
  // the Tauri command. Fire-and-forget; the command short-circuits
  // on `storeTraces === false` (D1.M4). Only invoked by mutations
  // that changed the messages array.
  const persist = () => {
    const s = get();
    void saveAgentChat(
      {
        projectPath: s.projectPath,
        projectName: s.workflow.name,
        projectId: s.workflow.id,
        storeTraces: s.storeTraces,
      },
      s.messages,
    );
  };

  return {
  messages: [],
  assistantError: null,
  runTraces: {},

  setAssistantOpen: (open) => {
    // D21 — the legacy boolean is now derived from `assistantSurface`.
    // Setting "open" maps to opening the **drawer** surface; the
    // Overview embedded card has its own surface and is not toggled
    // by this action.
    if (get().currentView === "overview") {
      // No drawer to toggle on Overview — the embedded card is always live.
      return;
    }
    if (open && isWalkthroughActive(get().walkthroughStatus)) {
      const status = get().walkthroughStatus;
      if (status === "Recording" || status === "Paused") {
        get().cancelWalkthrough();
      }
      // Review/Processing: don't discard — just hide the walkthrough panel
      // while the assistant is open. Closing the assistant restores it.
    }
    get().setAssistantSurface(open ? "drawer" : null);
  },
  toggleAssistant: () => {
    if (get().currentView === "overview") return;
    const opening = get().assistantSurface !== "drawer";
    if (opening && isWalkthroughActive(get().walkthroughStatus)) {
      const status = get().walkthroughStatus;
      if (status === "Recording" || status === "Paused") {
        get().cancelWalkthrough();
      }
      // Review/Processing: don't discard — just hide the walkthrough panel
      // while the assistant is open. Closing the assistant restores it.
    }
    get().setAssistantSurface(opening ? "drawer" : null);
  },

  setAssistantError: (error) => set({ assistantError: error }),

  pushAssistantMessage: (role, content, runId) => {
    const trimmed = content.trim();
    if (!trimmed) return;
    set((s) => ({
      messages: [
        ...s.messages,
        {
          role,
          content: trimmed,
          timestamp: new Date().toISOString(),
          runId,
        },
      ],
    }));
    persist();
  },

  pushSystemAnnotation: (content) => {
    const trimmed = content.trim();
    if (!trimmed) return;
    set((s) => ({
      messages: [
        ...s.messages,
        {
          role: "system",
          content: trimmed,
          timestamp: new Date().toISOString(),
        },
      ],
    }));
    persist();
  },

  clearConversation: () => {
    set({ messages: [] });
    persist();
  },

  setMessages: (messages) => set({ messages }),

  mapMessagesByRunIds: (runIds, fn) => {
    set((s) => ({
      messages: s.messages.map((m) =>
        m.role !== "system" && m.runId && runIds.has(m.runId) ? fn(m) : m,
      ),
    }));
    persist();
  },

  dropTurnsByRunIds: (runIds) => {
    set((s) => ({
      messages: s.messages.filter(
        (m) => m.role === "system" || !m.runId || !runIds.has(m.runId),
      ),
    }));
    persist();
  },

  applyTaskStateUpdate: (runId, taskState) => {
    set((s) => {
      const current = traceWith(s.runTraces, runId);
      const activeSubgoal = taskState.subgoal_stack.at(-1)?.text ?? "";
      return {
        runTraces: {
          ...s.runTraces,
          [runId]: {
            ...current,
            phase: taskState.phase,
            activeSubgoal,
          },
        },
      };
    });
  },

  applyWorldModelDelta: (runId, diff) => {
    set((s) => {
      const current = traceWith(s.runTraces, runId);
      const delta: WorldModelDelta = {
        stepIndex: nextTraceStepIndex(current),
        changedFields: diff.changed_fields,
      };
      return {
        runTraces: {
          ...s.runTraces,
          [runId]: {
            ...current,
            worldModelDeltas: [...current.worldModelDeltas, delta],
          },
        },
      };
    });
  },

  applyBoundary: (runId, kind, stepIndex, milestoneText) => {
    const milestone = boundaryMilestone(kind, stepIndex, milestoneText);
    if (!milestone) return;
    set((s) => {
      const current = traceWith(s.runTraces, runId);
      return {
        runTraces: {
          ...s.runTraces,
          [runId]: {
            ...current,
            milestones: [...current.milestones, milestone],
          },
        },
      };
    });
  },

  pushTraceStep: (runId, step) => {
    set((s) => {
      const current = traceWith(s.runTraces, runId);
      return {
        runTraces: {
          ...s.runTraces,
          [runId]: {
            ...current,
            steps: [...current.steps, step],
          },
        },
      };
    });
  },

  setTerminalFrame: (runId, frame) => {
    set((s) => {
      const current = traceWith(s.runTraces, runId);
      return {
        runTraces: {
          ...s.runTraces,
          [runId]: {
            ...current,
            terminalFrame: frame,
          },
        },
      };
    });
  },

  clearTrace: (runId) => {
    set((s) => {
      if (!s.runTraces[runId]) return {};
      const { [runId]: _removed, ...remaining } = s.runTraces;
      return { runTraces: remaining };
    });
  },

  hydrateRunTrace: (trace) => {
    if (get().agentRunId !== null) return;
    set((s) => ({
      runTraces: { ...s.runTraces, [trace.runId]: trace },
      agentRunId: trace.runId,
    }));
  },

  clearConversationFlow: async () => {
    const state = get();
    const activeRunId = state.agentRunId;
    const agentNodeIds = state.workflow.nodes
      .filter((n) => n.source_run_id != null)
      .map((n) => n.id);

    // (1) Remove agent nodes from the workflow WITHOUT a history push
    //     (D1.C1 — Clear is not undoable; writing a history entry here
    //     would resurrect deleted nodes via Cmd+Z while the cache/
    //     variant/transcript files stay wiped). Also strip deleted
    //     ids from any user groups and auto-dissolve groups that
    //     drop below their minimum membership — otherwise user-group
    //     metadata keeps referencing nodes that no longer exist and
    //     the canvas renders ghost/empty group containers.
    if (agentNodeIds.length > 0) {
      const idSet = new Set(agentNodeIds);
      const updatedGroups = (state.workflow.groups ?? []).map((g) => ({
        ...g,
        node_ids: g.node_ids.filter((id) => !idSet.has(id)),
      }));
      state.setWorkflow({
        ...state.workflow,
        nodes: state.workflow.nodes.filter((n) => !idSet.has(n.id)),
        edges: state.workflow.edges.filter(
          (e) => !idSet.has(e.from) && !idSet.has(e.to),
        ),
        groups: autoDissolveGroups(updatedGroups),
      });
      // Also clear history stacks so Cmd+Z cannot partial-undo the
      // graph mutation we just performed.
      state.clearHistory();
    }

    // (2) Wipe messages in memory before the file wipe so any
    //     concurrent saveAgentChat has nothing to replay. The flag
    //     short-circuits any in-flight save that races this call.
    conversationWipeInProgress = true;
    set({ messages: [] });
    // D24 — Clear is one of the two zeroing points for the elapsed
    // timestamps (the other is the next `startAgent`). Without this
    // reset the Live Runtime card would keep showing the prior run's
    // frozen Elapsed value after a Clear, which contradicts the
    // freshly-empty conversation surface.
    // Also clear the destructive-cap notice so the AssistantThread
    // run-halted card does not persist after the conversation is wiped.
    set({
      agentRunStartedAt: null,
      agentRunFinishedAt: null,
      consecutiveDestructiveCapHit: null,
    });

    // (3) Wipe files on disk. Respects `store_traces` privacy flag
    //     inside the command body (D1.M4).
    try {
      await commands.clearAgentConversation({
        project_path: state.projectPath,
        project_name: state.workflow.name,
        project_id: state.workflow.id,
        store_traces: state.storeTraces,
      });
    } catch (e) {
      state.setAssistantError(`Failed to clear conversation: ${String(e)}`);
    } finally {
      conversationWipeInProgress = false;
    }

    if (activeRunId) {
      state.dropRunBuffer(activeRunId);
      state.clearTrace(activeRunId);
      // Null the run ID so LiveRuntimeCard shows "No active run."
      // rather than the "Agent running..." fallback from RunTraceView
      // (which triggers when agentRunId is non-null but the trace was
      // just dropped).
      set({ agentRunId: null });
    }
  },
  };
};
