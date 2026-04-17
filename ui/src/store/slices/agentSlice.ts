import type { StateCreator } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type { Node, Edge } from "../../bindings";
import { toEndpoint } from "../settings";
import type { PermissionRule, ToolPermissions } from "../state";
import type { StoreState } from "./types";
import { buildPriorTurns } from "../../utils/priorTurns";

export interface AgentStep {
  summary: string;
  toolName: string;
  toolArgs: unknown;
  toolResult: string;
  pageTransitioned: boolean;
}

export type AgentStatus = "idle" | "running" | "complete" | "stopped" | "error";

export interface PendingApproval {
  stepIndex: number;
  toolName: string;
  arguments: unknown;
  description: string;
}

export interface CandidateRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface AmbiguityCandidateView {
  uid: string;
  snippet: string;
  rect: CandidateRect | null;
}

export interface AmbiguityResolution {
  /** Client-side id so the UI can key modals/cards without relying on
   *  backend-supplied node_id (multiple resolutions can fire per node). */
  id: string;
  nodeId: string;
  target: string;
  candidates: AmbiguityCandidateView[];
  chosenUid: string;
  reasoning: string;
  /** Viewport dimensions (CSS pixels) at capture time — rects are relative
   *  to this viewport, not to the full screenshot. `0` means unknown. */
  viewportWidth: number;
  viewportHeight: number;
  /** Path relative to the node's `artifacts/` directory. */
  screenshotPath: string;
  /** Base64-encoded PNG data. Populated from the live executor event. */
  screenshotBase64: string;
  /** Epoch ms at which the resolution was observed on the UI side. */
  createdAt: number;
}

/**
 * VLM completion verification disagreement. Raised when the agent emitted
 * `agent_done` but a post-run screenshot + VLM check rejected the
 * completion. The engine halts on the backend; the Tauri task holds the
 * run open on a per-run oneshot until the operator resolves via
 * `resolve_completion_disagreement` ({@link confirmDisagreementAsComplete}
 * / {@link cancelDisagreement}), which then writes the durable record
 * and emits the final terminal `agent://complete` or
 * `agent://stopped { reason: "user_cancelled_disagreement" }` event.
 */
export interface CompletionDisagreement {
  /** Base64 JPEG payload ready to drop into a data-URL. */
  screenshotBase64: string;
  /** Full VLM reply text, first line is typically YES/NO followed by reasoning. */
  vlmReasoning: string;
  /** Summary the agent provided with `agent_done`. */
  agentSummary: string;
}

/**
 * Payload from `agent://consecutive_destructive_cap_hit`. The run halts
 * server-side when the agent chains N destructive tool calls in a row,
 * and the UI shows a short notice in the assistant panel.
 */
export interface ConsecutiveDestructiveCapHit {
  recentToolNames: string[];
  cap: number;
}

/**
 * Map the UI's `ToolPermissions` shape into the wire form the Rust
 * backend expects. Rules and the per-tool map are both forwarded; the
 * engine merges them into one rule list before evaluating.
 */
export function toPermissionPolicyWire(perms: ToolPermissions) {
  return {
    allow_all: perms.allowAll,
    require_confirm_destructive: perms.requireConfirmDestructive,
    rules: perms.patternRules.map((r: PermissionRule) => ({
      tool_pattern: r.toolPattern,
      args_pattern: r.argsPattern ?? null,
      action: r.action,
    })),
    per_tool: perms.tools,
  };
}

/**
 * Tauri rejects with a structured `CommandError { kind, message }` for
 * typed failures (e.g. `AlreadyRunning`), but tauri-specta can also
 * surface plain strings when an error is serialized through `Display`.
 * Prefer the structured `message`, fall back to string coercion.
 */
function formatAgentError(err: unknown): string {
  if (err && typeof err === "object" && "message" in err) {
    const m = (err as { message?: unknown }).message;
    if (typeof m === "string" && m.length > 0) return m;
  }
  if (typeof err === "string") return err;
  return String(err);
}

/**
 * True when the rejection is the backend's `AlreadyRunning` refusal —
 * either the structured `{ kind: "AlreadyRunning" }` or the string
 * form `"AlreadyRunning: ..."` that `Display` produces.
 */
function isAlreadyRunningError(err: unknown): boolean {
  if (err && typeof err === "object" && "kind" in err) {
    return (err as { kind?: unknown }).kind === "AlreadyRunning";
  }
  if (typeof err === "string") {
    return err.startsWith("AlreadyRunning");
  }
  return false;
}

export interface AgentSlice {
  agentStatus: AgentStatus;
  agentGoal: string;
  agentSteps: AgentStep[];
  agentError: string | null;
  currentAgentStep: number;
  pendingApproval: PendingApproval | null;
  /** Set when the backend emits `agent://completion_disagreement`. */
  completionDisagreement: CompletionDisagreement | null;
  /** Set when the backend emits `agent://consecutive_destructive_cap_hit`. */
  consecutiveDestructiveCapHit: ConsecutiveDestructiveCapHit | null;
  /** Generation ID for the active run — used to reject stale events. */
  agentRunId: string | null;
  /** Ambiguity resolution records, newest first. Persists across agent
   *  completion so the user can inspect past resolutions. */
  ambiguityResolutions: AmbiguityResolution[];
  /** Active modal target for the ambiguity inspector, keyed by
   *  AmbiguityResolution.id. */
  activeAmbiguityId: string | null;
  startAgent: (goal: string) => Promise<void>;
  stopAgent: () => Promise<void>;
  addAgentStep: (step: AgentStep) => void;
  addAgentNode: (node: Node) => void;
  addAgentEdge: (edge: Edge) => void;
  setPendingApproval: (approval: PendingApproval | null) => void;
  approveAction: () => Promise<void>;
  rejectAction: () => Promise<void>;
  setCompletionDisagreement: (d: CompletionDisagreement | null) => void;
  setConsecutiveDestructiveCapHit: (
    d: ConsecutiveDestructiveCapHit | null,
  ) => void;
  /**
   * User confirmed a pending VLM disagreement — the agent really did
   * complete the goal. Invokes the backend resolver so the decision is
   * written to `events.jsonl` + the variant index and the final
   * `agent://complete` terminal event fires. The optimistic UI update
   * (clears the card, flips status to `complete`) is reverted if the
   * invoke rejects because no resolver is pending.
   */
  confirmDisagreementAsComplete: () => Promise<void>;
  /**
   * User agreed with the VLM that the agent did not complete the goal.
   * Invokes the backend resolver which records `DisagreementCancelled`
   * and emits `agent://stopped { reason: user_cancelled_disagreement }`.
   * If no resolver is pending (the backend has already torn the run
   * down), falls back to the local-only stop path so the card is still
   * dismissed.
   */
  cancelDisagreement: () => Promise<void>;
  setAgentStatus: (status: AgentStatus) => void;
  setAgentError: (error: string | null) => void;
  setAgentRunId: (runId: string) => void;
  resetAgent: () => void;
  addAmbiguityResolution: (resolution: AmbiguityResolution) => void;
  openAmbiguityModal: (id: string) => void;
  closeAmbiguityModal: () => void;
  clearAmbiguityResolutions: () => void;
}

export const createAgentSlice: StateCreator<StoreState, [], [], AgentSlice> = (
  set,
  get,
) => ({
  agentStatus: "idle",
  agentGoal: "",
  agentSteps: [],
  agentError: null,
  currentAgentStep: 0,
  pendingApproval: null,
  completionDisagreement: null,
  consecutiveDestructiveCapHit: null,
  agentRunId: null,
  ambiguityResolutions: [],
  activeAmbiguityId: null,

  startAgent: async (goal) => {
    const priorState = get();
    const {
      pushLog,
      agentConfig,
      projectPath,
      workflow,
      agentStatus,
      toolPermissions,
      storeTraces,
      messages,
      pushAssistantMessage,
    } = priorState;
    // If a run is already active, do not touch run-scoped state: the
    // backend will reject with AlreadyRunning and the live run's events
    // must keep routing through useAgentEvents. Otherwise optimistically
    // reset into the "running" shape before awaiting invoke, so early
    // terminal events (e.g. `agent://error` from a fast MCP-spawn
    // failure) can flip status to "error" — their handler gates on
    // `agentStatus === "running"`.
    const wasActive = agentStatus === "running";
    // Snapshot the prior run's visible state so we can restore it if
    // the backend rejects with AlreadyRunning during its async cleanup
    // window (handle still set but previous run has emitted its
    // terminal event). Without this, a restart attempt in that window
    // would wipe the terminal run's history and log it as "error".
    const snapshot = {
      agentStatus: priorState.agentStatus,
      agentGoal: priorState.agentGoal,
      agentSteps: priorState.agentSteps,
      agentError: priorState.agentError,
      currentAgentStep: priorState.currentAgentStep,
      pendingApproval: priorState.pendingApproval,
      completionDisagreement: priorState.completionDisagreement,
      consecutiveDestructiveCapHit: priorState.consecutiveDestructiveCapHit,
      agentRunId: priorState.agentRunId,
    };

    // Client-side run ID (D1.M1) so the user bubble can be tagged
    // before `agent://started` arrives and the backend can echo it.
    const runId = crypto.randomUUID();

    // Anchor = most recent workflow node with a source_run_id. Used
    // by the engine to seed `last_node_id` so the first emitted edge
    // connects from the prior chain into the new run's first node.
    let anchor: string | null = null;
    for (let i = workflow.nodes.length - 1; i >= 0; i -= 1) {
      const n = workflow.nodes[i] as {
        id: string;
        source_run_id?: string | null;
      };
      if (n.source_run_id) {
        anchor = n.id;
        break;
      }
    }

    // Build the prior-turn payload from the current chat + surviving
    // agent nodes. `buildPriorTurns` filters to pairs whose runId
    // still has live nodes on the canvas.
    const priorTurns = buildPriorTurns(messages, workflow).map((t) => ({
      goal: t.goal,
      summary: t.summary,
      run_id: t.run_id,
    }));

    if (!wasActive) {
      set({
        agentStatus: "running",
        agentGoal: goal,
        agentSteps: [],
        agentError: null,
        currentAgentStep: 0,
        pendingApproval: null,
        completionDisagreement: null,
        consecutiveDestructiveCapHit: null,
        // Install the new run's ID immediately so the user bubble
        // and any late in-flight events from the prior run (which
        // carry a different run_id) get rejected by `isStaleRunId`.
        agentRunId: runId,
      });
      // Push the user bubble stamped with the new run ID. This is
      // the single producer for the user side of the conversation —
      // App.tsx no longer pushes it separately.
      pushAssistantMessage("user", goal, runId);
      pushLog(`Agent started with goal: ${goal}`);
    }
    try {
      await invoke("run_agent", {
        request: {
          goal,
          agent: toEndpoint(agentConfig),
          project_path: projectPath,
          workflow_name: workflow.name,
          workflow_id: workflow.id,
          permissions: toPermissionPolicyWire(toolPermissions),
          consecutive_destructive_cap:
            toolPermissions.consecutiveDestructiveCap,
          store_traces: storeTraces,
          run_id: runId,
          anchor_node_id: anchor,
          prior_turns: priorTurns,
        },
      });
    } catch (err) {
      const msg = formatAgentError(err);
      pushLog(`Agent start rejected: ${msg}`);
      if (wasActive) {
        // A live run was already active — its state was never touched,
        // and its events must keep routing.
        return;
      }
      if (isAlreadyRunningError(err)) {
        // Backend is still tearing down the previous run. Restore its
        // visible state so its terminal history is not lost.
        set(snapshot);
        return;
      }
      set({ agentStatus: "error", agentError: msg });
    }
  },

  stopAgent: async () => {
    const { pushLog } = get();
    // Clear the disagreement card as well. When a CompletionDisagreement
    // is raised the backend holds the run open on a pending-disagreement
    // oneshot, and `stop_agent` → `force_stop` resolves that oneshot as
    // Cancel so the run still gets a truthful `DisagreementCancelled`
    // terminal record. We also clear the UI card locally so the Stop
    // button always dismisses it without waiting for the terminal event.
    set({
      agentStatus: "stopped",
      pendingApproval: null,
      completionDisagreement: null,
      consecutiveDestructiveCapHit: null,
    });
    try {
      await invoke("stop_agent");
    } catch {
      /* ignore if not running */
    }
    pushLog("Agent stopped");
  },

  addAgentStep: (step) => {
    set((s) => ({
      agentSteps: [...s.agentSteps, step],
      currentAgentStep: s.agentSteps.length,
    }));
  },

  addAgentNode: (node) => {
    const { workflow, setWorkflow } = get();
    setWorkflow({
      ...workflow,
      nodes: [...workflow.nodes, node],
    });
  },

  addAgentEdge: (edge) => {
    const { workflow, setWorkflow } = get();
    setWorkflow({
      ...workflow,
      edges: [...workflow.edges, edge],
    });
  },

  setPendingApproval: (approval) => set({ pendingApproval: approval }),

  approveAction: async () => {
    const { pushLog, pendingApproval } = get();
    if (!pendingApproval) return;
    set({ pendingApproval: null });
    try {
      await invoke("approve_agent_action", { approved: true });
      pushLog(`Approved: ${pendingApproval.toolName}`);
    } catch (err) {
      pushLog(`Approval send failed: ${formatAgentError(err)}`);
    }
  },

  rejectAction: async () => {
    const { pushLog, pendingApproval } = get();
    if (!pendingApproval) return;
    set({ pendingApproval: null });
    try {
      await invoke("approve_agent_action", { approved: false });
      pushLog(`Rejected: ${pendingApproval.toolName}`);
    } catch (err) {
      pushLog(`Rejection send failed: ${formatAgentError(err)}`);
    }
  },

  setCompletionDisagreement: (disagreement) =>
    set({ completionDisagreement: disagreement }),

  setConsecutiveDestructiveCapHit: (hit) =>
    set({ consecutiveDestructiveCapHit: hit }),

  /**
   * Confirm a pending VLM disagreement. The UI is updated optimistically
   * (card dismissed, status flipped to `complete`) before the invoke so
   * the buttons feel responsive; the backend then writes the durable
   * record and emits the truthful `agent://complete` terminal event.
   *
   * If the invoke rejects — typically because a concurrent stop already
   * consumed the disagreement sender — we roll status back to the
   * pre-click state (`stopped`, the state the disagreement card is
   * rendered against) so the UI is not stuck falsely showing
   * `complete` after the backend actually recorded the run as cancelled.
   * The card stays dismissed so the user isn't re-prompted.
   */
  confirmDisagreementAsComplete: async () => {
    const { pushLog, agentStatus } = get();
    const priorStatus = agentStatus;
    set({
      completionDisagreement: null,
      agentStatus: "complete",
    });
    try {
      await invoke("resolve_completion_disagreement", { action: "confirm" });
      pushLog(
        "Agent completion confirmed by user (VLM disagreed but user overrode)",
      );
    } catch (err) {
      set({ agentStatus: priorStatus });
      pushLog(
        `Completion confirm invoke rejected: ${formatAgentError(err)}`,
      );
    }
  },

  /**
   * Cancel a pending VLM disagreement. Clears the card and flips status
   * to `stopped` optimistically, then invokes the backend resolver so
   * the run trace records a `DisagreementCancelled` terminal reason.
   *
   * If the invoke rejects (e.g. the run was already torn down), we fall
   * through silently — the local state still reflects the user's choice.
   */
  cancelDisagreement: async () => {
    const { pushLog } = get();
    set({
      completionDisagreement: null,
      agentStatus: "stopped",
      pendingApproval: null,
    });
    try {
      await invoke("resolve_completion_disagreement", { action: "cancel" });
      pushLog("Agent run cancelled by user (VLM disagreement)");
    } catch (err) {
      pushLog(
        `Completion cancel invoke rejected: ${formatAgentError(err)}`,
      );
    }
  },

  setAgentStatus: (status) => set({ agentStatus: status }),

  setAgentError: (error) => set({ agentError: error }),

  setAgentRunId: (runId) => set({ agentRunId: runId }),

  resetAgent: () =>
    set({
      agentStatus: "idle",
      agentGoal: "",
      agentSteps: [],
      agentError: null,
      currentAgentStep: 0,
      pendingApproval: null,
      completionDisagreement: null,
      consecutiveDestructiveCapHit: null,
      agentRunId: null,
      // Ambiguity records are intentionally NOT cleared — they persist across
      // runs so the user can still inspect past resolutions until they
      // explicitly clear them or start a new project.
    }),

  addAmbiguityResolution: (resolution) =>
    set((s) => ({
      ambiguityResolutions: [resolution, ...s.ambiguityResolutions],
    })),

  openAmbiguityModal: (id) => set({ activeAmbiguityId: id }),

  closeAmbiguityModal: () => set({ activeAmbiguityId: null }),

  clearAmbiguityResolutions: () =>
    set({ ambiguityResolutions: [], activeAmbiguityId: null }),
});
