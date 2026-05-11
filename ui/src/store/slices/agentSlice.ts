import type { StateCreator } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { commands, type Skill } from "../../bindings";
import { toEndpoint } from "../settings";
import type { PermissionRule, ToolPermissions } from "../state";
import type { StoreState } from "./types";

export interface AgentStep {
  summary: string;
  toolName: string;
  toolArgs: unknown;
  toolResult: string;
  pageTransitioned: boolean;
}

export type AgentStatus = "idle" | "running" | "complete" | "stopped" | "error";

export interface PendingApproval {
  scope: import("./executionSlice").SafetyScope | null;
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
/**
 * True when the backend agent task is still alive — either the loop
 * is actively running, or it has halted on a `CompletionDisagreement`
 * and is waiting on `resolve_completion_disagreement`. During the
 * disagreement window the Tauri task still owns the cache /
 * variant-index / events writes for the current workflow, so gates
 * that prevent cross-project corruption (D1.C1), concurrent graph
 * mutation (D1.H3), and clear-conversation file races must honor
 * this broader signal — not just `agentStatus === "running"`.
 */
export function isAgentActive(
  agentStatus: AgentStatus,
  completionDisagreement: CompletionDisagreement | null,
): boolean {
  return agentStatus === "running" || completionDisagreement != null;
}

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
  /** Epoch ms when `startAgent` flipped the slice into the running state.
   *  Used by `LiveRuntimeCard` to compute the Elapsed metric. Cleared on
   *  the next `startAgent` (or `clearConversationFlow`) — never on terminal
   *  events alone, so the freeze duration stays visible. */
  agentRunStartedAt: number | null;
  /** Epoch ms when the active run reached a terminal state (stop, complete,
   *  error, or completion-disagreement resolution). Drives the frozen
   *  Elapsed display in `LiveRuntimeCard` between runs. Cleared together
   *  with `agentRunStartedAt` on the next start. */
  agentRunFinishedAt: number | null;
  /** Ambiguity resolution records, newest first. Persists across agent
   *  completion so the user can inspect past resolutions. */
  ambiguityResolutions: AmbiguityResolution[];
  /** Active modal target for the ambiguity inspector, keyed by
   *  AmbiguityResolution.id. */
  activeAmbiguityId: string | null;
  /** Session-only collapse state for synthetic agent-run containers. */
  agentRunCollapsed: Record<string, boolean>;
  /**
   * True when the user typed their first goal from `IntentEmptyState` on a
   * project with zero skills. When set, `commitRunBuffer` stages a
   * `pendingRunSave` payload that opens the post-run `AgentRunSaveSheet`
   * review modal; the actual skill IPC fires only when the user confirms in
   * the sheet. Cleared as soon as `commitRunBuffer` stages the payload (the
   * sheet itself owns the `pendingRunSave` lifecycle from that point on).
   */
  skillCreationIntent: boolean;
  /**
   * Set by `commitRunBuffer` when a run terminates while `skillCreationIntent`
   * is true. Drives the post-run `AgentRunSaveSheet` modal: user picks the
   * skill name and which steps to include before the skill is materialised.
   * Cleared on Save or Discard.
   */
  pendingRunSave: { runId: string; summary: string } | null;
  setPendingRunSave: (v: { runId: string; summary: string } | null) => void;
  startAgent: (goal: string) => Promise<void>;
  stopAgent: () => Promise<void>;
  addAgentStep: (step: AgentStep) => void;
  /**
   * Commit the completed run. When `skillCreationIntent` is true, stages a
   * `pendingRunSave` payload (clears the intent flag at the same time) so the
   * post-run `AgentRunSaveSheet` opens and the user can review the steps
   * before the skill is materialised. Otherwise a no-op — ad-hoc runs are
   * not committed to a graph (no graph exists in the skill-only shell).
   */
  commitRunBuffer: (runId: string, summary: string) => void;
  dropRunBuffer: (runId: string) => void;
  setSkillCreationIntent: (intent: boolean) => void;
  /**
   * Explicitly save the most recently completed agent run as a new skill.
   * Privacy-gated: returns `{ ok: false, error }` when `storeTraces` or
   * `skillsEnabled` is off, or when the backend IPC returns an error.
   * Returns `{ ok: true, skill }` carrying the materialised `Skill` on a
   * confirmed write. Pass `stepIndices` to materialise only a subset of
   * `agentSteps` (used by `AgentRunSaveSheet` to skip wrong-path/correction
   * steps). On success, refreshes the skills panel so the new skill is
   * surfaced in the UI without a project reload.
   */
  saveRunAsSkill: (
    name?: string,
    stepIndices?: number[],
  ) => Promise<{ ok: true; skill: Skill } | { ok: false; error: string }>;
  /**
   * Explicitly append the most recently completed agent run to an existing
   * skill identified by `skillId`. Privacy-gated like `saveRunAsSkill`.
   */
  addRunToSkill: (skillId: string, version: number) => Promise<void>;
  toggleAgentRunCollapsed: (runId: string) => void;
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
  agentRunCollapsed: {},
  agentRunStartedAt: null,
  agentRunFinishedAt: null,
  skillCreationIntent: false,
  pendingRunSave: null,
  setPendingRunSave: (v) => set({ pendingRunSave: v }),

  startAgent: async (goal) => {
    const priorState = get();
    const {
      pushLog,
      agentConfig,
      projectPath,
      projectName,
      projectId,
      toolPermissions,
      storeTraces,
      episodicEnabled,
      retrievedEpisodesK,
      episodicGlobalParticipation,
      skillsEnabled,
      applicableSkillsK,
      skillsGlobalParticipation,
      pushAssistantMessage,
    } = priorState;
    // If a run is already active, do not touch run-scoped state: the
    // backend will reject with AlreadyRunning and the live run's events
    // must keep routing through useAgentEvents. Otherwise optimistically
    // reset into the "running" shape before awaiting invoke, so early
    // terminal events (e.g. `agent://error` from a fast MCP-spawn
    // failure) can flip status to "error" — their handler gates on
    // `agentStatus === "running"`. A pending VLM completion-disagreement
    // resolver counts as "active" too — the backend task still owns
    // the workflow's cache/variant-index writes, so a fresh start
    // would race them even though `agentStatus` has been flipped to
    // `"stopped"` for the spinner UI.
    const wasActive = isAgentActive(
      priorState.agentStatus,
      priorState.completionDisagreement,
    );
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
      // Include elapsed timestamps so AlreadyRunning rollback restores
      // the prior terminal run's frozen duration rather than keeping
      // the rejected attempt's start time and a null finish time.
      agentRunStartedAt: priorState.agentRunStartedAt,
      agentRunFinishedAt: priorState.agentRunFinishedAt,
    };

    // Client-side run ID so the user bubble can be tagged before
    // `agent://started` arrives and the backend can echo it.
    const runId = crypto.randomUUID();

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
        // D24 — both elapsed fields zero together on every fresh
        // start. The "cleared together only on the next start" rule
        // is achieved by writing both fields here; terminal events
        // only set `agentRunFinishedAt`, never `agentRunStartedAt`.
        agentRunStartedAt: Date.now(),
        agentRunFinishedAt: null,
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
          project_name: projectName,
          project_id: projectId,
          permissions: toPermissionPolicyWire(toolPermissions),
          consecutive_destructive_cap:
            toolPermissions.consecutiveDestructiveCap,
          store_traces: storeTraces,
          run_id: runId,
          anchor_node_id: null,
          prior_turns: [],
          episodic_enabled: episodicEnabled,
          retrieved_episodes_k: retrievedEpisodesK,
          episodic_global_participation: episodicGlobalParticipation,
          skills_enabled: skillsEnabled,
          applicable_skills_k: applicableSkillsK,
          skills_global_participation: skillsGlobalParticipation,
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
    // Clear the pending approval so the agent task's oneshot receives
    // a rejection (force_stop also sends false on the approval
    // channel). Do NOT clear `completionDisagreement` here — the
    // backend task is still alive writing its final cache / variant-
    // index / events state, and the frontend `isAgentActive` gates
    // must stay armed until the terminal event handler clears the
    // card. Flipping `agentStatus` to "stopped" is safe because
    // `isAgentActive` also watches the disagreement card.
    set({
      agentStatus: "stopped",
      pendingApproval: null,
      consecutiveDestructiveCapHit: null,
      // D24 — freeze elapsed at the user-initiated stop. Terminal
      // event handlers will overwrite with their own Date.now() if
      // they fire (within microseconds), but stamping here keeps the
      // Live Runtime card frozen even if the backend never emits a
      // terminal event (e.g. force-kill path).
      agentRunFinishedAt: Date.now(),
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

  commitRunBuffer: (runId, summary) => {
    // When `skillCreationIntent` is set we defer materialising the skill
    // to `AgentRunSaveSheet` so the user can review the name + steps
    // (including filtering out wrong-path / correction tool calls) before
    // commit. Otherwise this is a no-op for ad-hoc runs.
    const state = get();
    if (state.skillCreationIntent) {
      set({
        skillCreationIntent: false,
        pendingRunSave: { runId, summary: summary || state.agentGoal || "" },
      });
    }
  },

  dropRunBuffer: (_runId) => {
    // No-op: buffers were removed with the canvas. Kept for call-site
    // compatibility until event subscribers are updated.
  },

  setSkillCreationIntent: (intent) => set({ skillCreationIntent: intent }),

  saveRunAsSkill: async (name, stepIndices) => {
    const state = get();
    if (!state.storeTraces || !state.skillsEnabled) {
      return {
        ok: false,
        error: !state.storeTraces
          ? "Trace persistence is off"
          : "Skill saving is disabled",
      };
    }
    const sourceSteps = stepIndices
      ? stepIndices
          .map((i) => state.agentSteps[i])
          .filter((s): s is AgentStep => s !== undefined)
      : state.agentSteps;
    const steps = sourceSteps.map((s) => ({
      summary: s.summary,
      tool_name: s.toolName,
      args_json: s.toolArgs ? JSON.stringify(s.toolArgs) : "",
    }));
    const result = await commands.saveRunAsSkill({
      project_path: state.projectPath ?? null,
      project_name: state.projectName,
      project_id: state.projectId,
      name: (typeof name === "string" ? name : state.agentGoal) ?? "",
      goal: state.agentGoal,
      steps,
      store_traces: state.storeTraces,
    });
    if (result.status === "error") {
      const message =
        typeof result.error === "string"
          ? result.error
          : JSON.stringify(result.error);
      console.error("saveRunAsSkill failed:", message);
      return { ok: false, error: message };
    }
    // Refresh the skills panel so the freshly written skill is surfaced
    // without requiring a project reload. `save_run_as_skill` emits
    // `agent://skill_extracted` without a `run_id`, which the staleness
    // filter on the event listener rejects, so we cannot rely on that
    // path to update the UI for this command.
    //
    // Re-check the current project before refreshing: if the user opened
    // or created a different project while the IPC was in flight, the
    // captured project fields are stale. Refreshing with them would clobber
    // the new project's panel with the old project's skill list. The new
    // project's own panel load is handled by `AppShell`'s skills-load
    // effect, so we can safely skip the refresh here.
    const currentProjectId = get().projectId;
    if (currentProjectId === state.projectId) {
      get()
        .loadSkillsForPanel({
          projectPath: state.projectPath,
          projectName: state.projectName,
          projectId: state.projectId,
          includeGlobal: state.skillsGlobalParticipation,
          storeTraces: state.storeTraces,
        })
        .catch((e) =>
          console.error("loadSkillsForPanel after saveRunAsSkill failed", e),
        );
    }
    return { ok: true, skill: result.data };
  },

  addRunToSkill: async (skillId, version) => {
    const state = get();
    if (!state.storeTraces || !state.skillsEnabled) return;
    const steps = state.agentSteps.map((s) => ({
      summary: s.summary,
      tool_name: s.toolName,
      args_json: s.toolArgs ? JSON.stringify(s.toolArgs) : "",
    }));
    const result = await commands.addRunToSkill({
      project_path: state.projectPath ?? null,
      project_name: state.projectName,
      project_id: state.projectId,
      skill_id: skillId,
      version,
      goal: state.agentGoal,
      steps,
      store_traces: state.storeTraces,
    });
    if (result.status === "error") {
      console.error("addRunToSkill failed:", result.error);
    }
  },

  toggleAgentRunCollapsed: (runId) => {
    set((state) => ({
      agentRunCollapsed: {
        ...state.agentRunCollapsed,
        [runId]: !state.agentRunCollapsed[runId],
      },
    }));
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
   * Confirm a pending VLM disagreement. Invokes the backend resolver
   * which forwards the operator's choice into the agent task. Does
   * NOT clear `completionDisagreement` locally — the terminal
   * `agent://complete` / `agent://stopped` / `agent://error` event
   * handlers clear it, so the `isAgentActive` gates (cross-project,
   * mid-run-delete, Clear) stay armed until the Tauri task has
   * actually finished its final cache + variant-index + events
   * writes. That was the real source of the post-Confirm race.
   *
   * If the resolver invoke rejects the backend run was already
   * gone — the card is dismissed explicitly so the user isn't
   * re-prompted in a never-going-to-end state.
   */
  confirmDisagreementAsComplete: async () => {
    const { pushLog } = get();
    // D24 — freeze elapsed at the moment of resolution. The terminal
    // event will overwrite this with its own Date.now() if it fires
    // (within microseconds), but the optimistic stamp ensures the
    // Live Runtime card freezes immediately even if the catch path
    // runs (resolver-rejected race) and only sets `completionDisagreement`.
    set({ agentRunFinishedAt: Date.now() });
    try {
      await invoke("resolve_completion_disagreement", { action: "confirm" });
      pushLog(
        "Agent completion confirmed by user (VLM disagreed but user overrode)",
      );
      // Card and agent status are cleared by the terminal event
      // handler in useAgentEvents when `agent://complete` arrives.
    } catch (err) {
      set({ completionDisagreement: null });
      pushLog(`Completion confirm invoke rejected: ${formatAgentError(err)}`);
    }
  },

  /**
   * Cancel a pending VLM disagreement. Same contract as
   * `confirmDisagreementAsComplete` — forwards the decision and lets
   * the terminal-event handler drop the active-run marker so the
   * frontend gates stay armed until the backend task finishes.
   */
  cancelDisagreement: async () => {
    const { pushLog, agentRunId } = get();
    if (agentRunId) {
      get().dropRunBuffer(agentRunId);
    }
    // D24 — freeze elapsed at resolution (mirrors confirm path). The
    // terminal `agent://stopped` event overwrites this if it fires;
    // stamping here guarantees the Live Runtime card freezes even on
    // the resolver-rejected catch path.
    set({ agentRunFinishedAt: Date.now() });
    try {
      await invoke("resolve_completion_disagreement", { action: "cancel" });
      pushLog("Agent run cancelled by user (VLM disagreement)");
      // Card and agent status are cleared by the terminal event
      // handler in useAgentEvents when `agent://stopped` arrives.
    } catch (err) {
      set({
        completionDisagreement: null,
        agentStatus: "stopped",
        pendingApproval: null,
        agentRunFinishedAt: Date.now(),
      });
      pushLog(`Completion cancel invoke rejected: ${formatAgentError(err)}`);
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
      agentRunCollapsed: {},
      agentRunStartedAt: null,
      agentRunFinishedAt: null,
      skillCreationIntent: false,
      pendingRunSave: null,
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
