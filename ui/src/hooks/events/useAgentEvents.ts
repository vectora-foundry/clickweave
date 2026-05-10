import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import type { BoundaryKind, Edge, Node, TaskState, WorldModelDiff } from "../../bindings";
import { useStore } from "../../store/useAppStore";
import type { AgentStatus } from "../../store/slices/agentSlice";
import type { AgentPhase, TerminalFrame } from "../../store/slices/assistantSlice";

/** All run-scoped payloads include a generation ID. */
interface RunScoped {
  run_id: string;
}

/**
 * Reject events from a stale run or during the null gap before
 * `agent://started` installs the active run ID.
 *
 * Exported as a pure helper so unit tests can verify the null-gap
 * guard without spinning up a Tauri event listener.
 */
export function isStaleRunId(
  activeRunId: string | null,
  incomingRunId: string,
): boolean {
  return activeRunId === null || incomingRunId !== activeRunId;
}

interface AgentStartedPayload extends RunScoped {}

interface AgentStepPayload extends RunScoped {
  summary: string;
  tool_name: string;
  step_number: number;
}

interface NodeAddedPayload extends RunScoped {
  node: Node;
}

interface EdgeAddedPayload extends RunScoped {
  edge: Edge;
}

interface CdpConnectedPayload extends RunScoped {
  app_name: string;
  port: number;
}

interface AgentErrorPayload extends RunScoped {
  message: string;
}

interface AgentStoppedPayload extends RunScoped {
  reason: string;
  steps_executed?: number;
  consecutive_errors?: number;
}

interface StepFailedPayload extends RunScoped {
  step_number: number;
  tool_name: string;
  error: string;
}

interface ApprovalRequiredPayload extends RunScoped {
  scope: import("../../store/slices/executionSlice").SafetyScope | null;
  tool_name: string;
  arguments: unknown;
  description: string;
}

interface CompletionDisagreementPayload extends RunScoped {
  screenshot_b64: string;
  vlm_reasoning: string;
  agent_summary: string;
}

interface ConsecutiveDestructiveCapHitPayload extends RunScoped {
  recent_tool_names: string[];
  cap: number;
}

interface TaskStateChangedPayload extends RunScoped {
  task_state: TaskState;
}

interface WorldModelChangedPayload extends RunScoped {
  diff: WorldModelDiff;
}

interface BoundaryRecordWrittenPayload extends RunScoped {
  boundary_kind: BoundaryKind;
  step_index: number;
  milestone_text: string | null;
}

/**
 * Subscribe to agent backend events:
 * agent://started, agent://step, agent://complete,
 * agent://completion_disagreement, agent://stopped, agent://error,
 * agent://warning, agent://node_added, agent://edge_added,
 * agent://approval_required, agent://cdp_connected, agent://step_failed,
 * agent://sub_action.
 *
 * All run-scoped events carry a `run_id` generation ID. Events whose
 * run_id does not match the active run are silently dropped to prevent
 * stale state from a previous run leaking into the current one.
 *
 * Dispatches into the Zustand AgentSlice via `getState()`.
 */
export function useAgentEvents() {
  useEffect(() => {
    const unlisteners: (() => void)[] = [];
    let cancelled = false;

    const sub = (p: Promise<() => void>) =>
      p
        .then((u) => {
          if (cancelled) {
            u();
            return;
          }
          unlisteners.push(u);
        })
        .catch((err) => {
          console.error("Failed to subscribe to agent event:", err);
          useStore
            .getState()
            .pushLog(`Critical: agent event listener failed: ${err}`);
        });

    /** Reject events from a stale run or during the null gap before agent://started. */
    const isStale = (runId: string): boolean =>
      isStaleRunId(useStore.getState().agentRunId, runId);

    const currentTracePhase = (runId: string): AgentPhase =>
      useStore.getState().runTraces[runId]?.phase ?? "exploring";

    // ── Run lifecycle ──────────────────────────────────────────

    sub(
      listen<AgentStartedPayload>("agent://started", (e) => {
        useStore.getState().setAgentRunId(e.payload.run_id);
      }),
    );

    // ── Step events ────────────────────────────────────────────

    sub(
      listen<AgentStepPayload>("agent://step", (e) => {
        if (isStale(e.payload.run_id)) return;
        const phase = currentTracePhase(e.payload.run_id);
        useStore.getState().addAgentStep({
          summary: e.payload.summary,
          toolName: e.payload.tool_name,
          toolArgs: null,
          toolResult: e.payload.summary,
          pageTransitioned: false,
        });
        useStore
          .getState()
          .pushLog(
            `Agent step ${e.payload.step_number}: ${e.payload.tool_name}`,
          );
        useStore.getState().pushTraceStep(e.payload.run_id, {
          stepIndex: e.payload.step_number,
          toolName: e.payload.tool_name,
          phase,
          body: e.payload.summary,
          failed: false,
        });
      }),
    );

    sub(
      listen<NodeAddedPayload>("agent://node_added", (e) => {
        if (isStale(e.payload.run_id)) return;
        useStore
          .getState()
          .bufferAgentNode(e.payload.run_id, e.payload.node);
      }),
    );

    sub(
      listen<EdgeAddedPayload>("agent://edge_added", (e) => {
        if (isStale(e.payload.run_id)) return;
        useStore
          .getState()
          .bufferAgentEdge(e.payload.run_id, e.payload.edge);
      }),
    );

    sub(
      listen<ApprovalRequiredPayload>("agent://approval_required", (e) => {
        if (isStale(e.payload.run_id)) return;
        // Ignore stale approval requests that arrive after stop/cancel
        const current = useStore.getState().agentStatus;
        if (current !== "running") return;
        useStore.getState().setPendingApproval({
          scope: e.payload.scope,
          toolName: e.payload.tool_name,
          arguments: e.payload.arguments,
          description: e.payload.description,
        });
        useStore
          .getState()
          .pushLog(`Agent awaiting approval: ${e.payload.tool_name}`);
      }),
    );

    sub(
      listen<CdpConnectedPayload>("agent://cdp_connected", (e) => {
        if (isStale(e.payload.run_id)) return;
        useStore
          .getState()
          .pushLog(
            `CDP connected to '${e.payload.app_name}' (port ${e.payload.port})`,
          );
      }),
    );

    sub(
      listen<StepFailedPayload>("agent://step_failed", (e) => {
        if (isStale(e.payload.run_id)) return;
        const phase = currentTracePhase(e.payload.run_id);
        useStore.getState().addAgentStep({
          summary: `Error: ${e.payload.error}`,
          toolName: e.payload.tool_name,
          toolArgs: null,
          toolResult: e.payload.error,
          pageTransitioned: false,
        });
        useStore
          .getState()
          .pushLog(
            `Agent step ${e.payload.step_number} failed: ${e.payload.tool_name} — ${e.payload.error}`,
          );
        useStore.getState().pushTraceStep(e.payload.run_id, {
          stepIndex: e.payload.step_number,
          toolName: e.payload.tool_name,
          phase,
          body: e.payload.error,
          failed: true,
        });
      }),
    );

    sub(
      listen<RunScoped & { tool_name: string; summary: string }>(
        "agent://sub_action",
        (e) => {
          if (isStale(e.payload.run_id)) return;
          useStore
            .getState()
            .pushLog(
              `Agent auto-action: ${e.payload.tool_name} — ${e.payload.summary}`,
            );
        },
      ),
    );

    // ── Terminal events ────────────────────────────────────────

    // Only transition status if the agent was still active — prevents
    // a backend event from overriding a user-initiated stop.
    const setStatusIfActive = (status: AgentStatus) => {
      const current = useStore.getState().agentStatus;
      if (current === "running") {
        useStore.getState().setAgentStatus(status);
      }
    };

    sub(
      listen<RunScoped & { summary?: string }>("agent://complete", (e) => {
        if (isStale(e.payload.run_id)) return;
        const summary = e.payload.summary?.trim();
        useStore
          .getState()
          .commitRunBuffer(e.payload.run_id, summary || "Goal completed.");
        // A confirmed disagreement demotes status to `stopped` for the
        // card UI but the backend still emits `agent://complete` on
        // resolution. Accept the promotion from either `running` or
        // a pending-disagreement `stopped` so a confirmed completion
        // isn't left stuck in the post-card status.
        const current = useStore.getState().agentStatus;
        const hadDisagreement =
          useStore.getState().completionDisagreement != null;
        if (current === "running" || hadDisagreement) {
          useStore.getState().setAgentStatus("complete");
        }
        // Terminal event: the backend task has finished all of its
        // variant-index / cache / events.jsonl writes. Drop the
        // active-run marker (disagreement card) so the `isAgentActive`
        // gates reopen — previously Confirm/Cancel cleared the card
        // optimistically and reopened the guards before the backend
        // was actually done.
        useStore.getState().setCompletionDisagreement(null);
        // D24 — freeze elapsed at the terminal moment.
        useStore.setState({ agentRunFinishedAt: Date.now() });
        useStore.getState().pushLog("Agent completed");
        useStore.getState().setTerminalFrame(e.payload.run_id, {
          kind: "complete",
          detail: summary || "Goal completed.",
        });
        useStore
          .getState()
          .pushAssistantMessage(
            "assistant",
            summary || "Goal completed.",
            e.payload.run_id,
          );
      }),
    );

    sub(
      listen<CompletionDisagreementPayload>(
        "agent://completion_disagreement",
        (e) => {
          if (isStale(e.payload.run_id)) return;
          // Mark the run as stopped so the assistant panel switches out of
          // its "running" UI while the disagreement card is displayed.
          setStatusIfActive("stopped");
          useStore.getState().setCompletionDisagreement({
            screenshotBase64: e.payload.screenshot_b64,
            vlmReasoning: e.payload.vlm_reasoning,
            agentSummary: e.payload.agent_summary,
          });
          useStore
            .getState()
            .pushLog(
              "Agent completion check disagreed — awaiting user decision",
            );
        },
      ),
    );

    sub(
      listen<ConsecutiveDestructiveCapHitPayload>(
        "agent://consecutive_destructive_cap_hit",
        (e) => {
          if (isStale(e.payload.run_id)) return;
          useStore.getState().dropRunBuffer(e.payload.run_id);
          setStatusIfActive("stopped");
          useStore
            .getState()
            .setConsecutiveDestructiveCapHit({
              recentToolNames: e.payload.recent_tool_names,
              cap: e.payload.cap,
            });
          const toolList = e.payload.recent_tool_names.join(", ");
          useStore
            .getState()
            .pushLog(
              `Run halted: reached ${e.payload.cap} consecutive destructive actions (${toolList})`,
            );
        },
      ),
    );

    sub(
      listen<AgentStoppedPayload>("agent://stopped", (e) => {
        if (isStale(e.payload.run_id)) return;
        useStore.getState().dropRunBuffer(e.payload.run_id);
        // A `stopped` for `user_cancelled_disagreement` arrives after the
        // operator's Cancel button optimistically flipped status to
        // `stopped`; the disagreement card is already dismissed. Keep
        // the status coalescer so we don't accidentally re-enter
        // `stopped` over a newly-`complete` race (not possible today,
        // but cheap future-proofing).
        setStatusIfActive("stopped");
        // Terminal event: backend task finished. Always drop the
        // disagreement card (whatever reason the run ended for) so
        // the `isAgentActive` gates reopen only after the backend
        // has actually completed its final cache/variant-index writes.
        useStore.getState().setCompletionDisagreement(null);
        // D24 — freeze elapsed at the terminal moment.
        useStore.setState({ agentRunFinishedAt: Date.now() });
        const detail =
          e.payload.reason === "max_steps_reached"
            ? `after ${e.payload.steps_executed} steps`
            : e.payload.reason === "max_errors_reached"
              ? `after ${e.payload.consecutive_errors} consecutive errors`
              : e.payload.reason === "approval_unavailable"
                ? "approval system unavailable"
                : e.payload.reason === "user_cancelled_disagreement"
                  ? "user cancelled after VLM disagreement"
                  : e.payload.reason === "loop_detected"
                    ? "the same tool call kept failing — stopped to avoid looping"
                    : e.payload.reason;
        const frameKind: TerminalFrame["kind"] =
          e.payload.reason === "user_cancelled_disagreement"
            ? "disagreement_cancelled"
            : "stopped";
        useStore.getState().setTerminalFrame(e.payload.run_id, {
          kind: frameKind,
          detail,
        });
        useStore.getState().pushLog(`Agent stopped: ${detail}`);
        useStore
          .getState()
          .pushAssistantMessage(
            "assistant",
            `Stopped: ${detail}`,
            e.payload.run_id,
          );
      }),
    );

    sub(
      listen<RunScoped & { action: "confirm" | "cancel" }>(
        "agent://completion_disagreement_resolved",
        (e) => {
          if (isStale(e.payload.run_id)) return;
          // The definitive status change rides on the terminal
          // `agent://complete` or `agent://stopped` that fires right
          // after this event. This subscriber exists so the log drawer
          // records the resolution for any run where the operator used
          // the Stop path without the assistant panel's Cancel button
          // having already logged it.
          useStore
            .getState()
            .pushLog(`Completion disagreement resolved: ${e.payload.action}`);
        },
      ),
    );

    sub(
      listen<RunScoped & { message: string }>("agent://warning", (e) => {
        if (isStale(e.payload.run_id)) return;
        useStore
          .getState()
          .pushLog(`Agent warning: ${e.payload.message}`);
      }),
    );

    sub(
      listen<AgentErrorPayload>("agent://error", (e) => {
        if (isStale(e.payload.run_id)) return;
        useStore.getState().dropRunBuffer(e.payload.run_id);
        // Only transition to error if the agent was still active —
        // a racing error after stop should not override "stopped".
        const current = useStore.getState().agentStatus;
        if (current === "running") {
          useStore.getState().setAgentError(e.payload.message);
          useStore.getState().setAgentStatus("error");
        }
        // Terminal event: clear the disagreement card so
        // `isAgentActive` drops to false now that the backend task
        // has signalled it is done.
        useStore.getState().setCompletionDisagreement(null);
        // D24 — freeze elapsed at the terminal moment. Stamped
        // outside the `if (current === "running")` branch above so
        // a racing-error-after-stop still reflects the last terminal
        // moment in the Live Runtime card.
        useStore.setState({ agentRunFinishedAt: Date.now() });
        useStore.getState().setTerminalFrame(e.payload.run_id, {
          kind: "error",
          detail: e.payload.message,
        });
        useStore
          .getState()
          .pushLog(`Agent error: ${e.payload.message}`);
        useStore
          .getState()
          .pushAssistantMessage(
            "assistant",
            `Error: ${e.payload.message}`,
            e.payload.run_id,
          );
      }),
    );

    // ── Trace events ────────────────────────────────────────────

    sub(
      listen<TaskStateChangedPayload>("agent://task_state_changed", (e) => {
        if (isStale(e.payload.run_id)) return;
        useStore
          .getState()
          .applyTaskStateUpdate(e.payload.run_id, e.payload.task_state);
      }),
    );

    sub(
      listen<WorldModelChangedPayload>("agent://world_model_changed", (e) => {
        if (isStale(e.payload.run_id)) return;
        useStore
          .getState()
          .applyWorldModelDelta(e.payload.run_id, e.payload.diff);
      }),
    );

    sub(
      listen<BoundaryRecordWrittenPayload>(
        "agent://boundary_record_written",
        (e) => {
          if (isStale(e.payload.run_id)) return;
          useStore.getState().applyBoundary(
            e.payload.run_id,
            e.payload.boundary_kind,
            e.payload.step_index,
            e.payload.milestone_text,
          );
        },
      ),
    );

    sub(
      listen<{
        run_id: string;
        event_run_id: string;
        skill_id: string;
        version: number;
        parameter_count: number;
      }>("agent://skill_invoked", (e) => {
        if (isStale(e.payload.run_id)) return;
        const suffix = e.payload.parameter_count === 1 ? "" : "s";
        useStore
          .getState()
          .pushLog(
            `Agent invoked skill ${e.payload.skill_id} v${e.payload.version} (${e.payload.parameter_count} parameter${suffix})`,
          );
      }),
    );

    sub(
      listen<{
        run_id: string;
        event_run_id: string;
        skill_id: string;
        version: number;
        state: "draft" | "confirmed" | "promoted";
        scope: "project_local" | "global";
      }>("agent://skill_extracted", (e) => {
        if (isStale(e.payload.run_id)) return;
        useStore.getState().applySkillExtracted(e.payload);
      }),
    );

    sub(
      listen<{
        run_id: string;
        event_run_id: string;
        skill_id: string;
        version: number;
      }>("agent://skill_confirmed", (e) => {
        // skill_confirmed can arrive outside an active run (user
        // confirms a skill from the panel). Don't gate on isStale.
        useStore.getState().applySkillConfirmed(e.payload);
      }),
    );

    return () => {
      cancelled = true;
      unlisteners.forEach((u) => u());
    };
  }, []);
}
