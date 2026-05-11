use super::*;

// ── Event forwarding seam ───────────────────────────────────────

/// Forward one `AgentEvent` to its paired `agent://*` Tauri event.
///
/// The persistence side of the forwarder (appending every event to
/// `events.jsonl`) stays at the call site so `RunStorage` lock ownership
/// is not smeared across this helper. `GoalComplete` is deliberately a
/// no-op: the terminal `agent://complete` is emitted from the main
/// run-agent task after the engine returns, and the
/// `CompletionDisagreementResolved` variant is emitted by the Tauri
/// layer itself (see `await_disagreement_resolution`), so neither
/// crosses this forwarder at runtime.
///
/// Extracted as a standalone function so the rubric-10 smoke test in
/// `run_agent_smoke_tests` can drive a scripted `AgentEvent` stream
/// against a mock `AppHandle` and assert the full (variant → topic)
/// mapping, locking the forwarder contract before Phase 3b deletes
/// `loop_runner.rs`.
pub(crate) fn forward_agent_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    run_id: &str,
    event: &AgentEvent,
) {
    let handled = match event {
        AgentEvent::StepCompleted { .. }
        | AgentEvent::GoalComplete { .. }
        | AgentEvent::Error { .. }
        | AgentEvent::Warning { .. }
        | AgentEvent::CdpConnected { .. }
        | AgentEvent::StepFailed { .. }
        | AgentEvent::SubAction { .. }
        | AgentEvent::CompletionDisagreement { .. }
        | AgentEvent::ConsecutiveDestructiveCapHit { .. }
        | AgentEvent::CompletionDisagreementResolved { .. } => {
            forward_lifecycle_agent_event(app, run_id, event)
        }
        AgentEvent::TaskStateChanged { .. }
        | AgentEvent::WorldModelChanged { .. }
        | AgentEvent::BoundaryRecordWritten { .. } => forward_state_agent_event(app, run_id, event),
        AgentEvent::EpisodesRetrieved { .. }
        | AgentEvent::EpisodeWritten { .. }
        | AgentEvent::EpisodePromoted { .. } => forward_episodic_agent_event(app, run_id, event),
        AgentEvent::SkillInvoked { .. }
        | AgentEvent::SkillExtracted { .. }
        | AgentEvent::SkillConfirmed { .. } => forward_skill_agent_event(app, run_id, event),
    };
    debug_assert!(handled, "agent event was classified but not forwarded");
}

fn emit_agent_event<R, S>(app: &tauri::AppHandle<R>, topic: &str, payload: S)
where
    R: tauri::Runtime,
    S: Serialize + Clone,
{
    let _ = app.emit(topic, payload);
}

fn forward_lifecycle_agent_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    run_id: &str,
    event: &AgentEvent,
) -> bool {
    match event {
        AgentEvent::StepCompleted {
            step_index,
            tool_name,
            summary,
        } => {
            emit_agent_event(
                app,
                "agent://step",
                AgentStepPayload {
                    run_id: run_id.to_string(),
                    summary: summary.clone(),
                    tool_name: tool_name.clone(),
                    step_number: *step_index,
                },
            );
            true
        }
        AgentEvent::GoalComplete { .. } => {
            // Terminal completion is emitted as agent://complete by the
            // main task after the agent loop finishes. This in-band
            // event is only used for durable tracing.
            true
        }
        AgentEvent::Error { message } => {
            emit_agent_event(
                app,
                "agent://error",
                serde_json::json!({ "run_id": run_id, "message": message }),
            );
            true
        }
        AgentEvent::Warning { message } => {
            emit_agent_event(
                app,
                "agent://warning",
                serde_json::json!({ "run_id": run_id, "message": message }),
            );
            true
        }
        AgentEvent::CdpConnected { app_name, port } => {
            emit_agent_event(
                app,
                "agent://cdp_connected",
                serde_json::json!({
                    "run_id": run_id,
                    "app_name": app_name,
                    "port": port,
                }),
            );
            true
        }
        AgentEvent::StepFailed {
            step_index,
            tool_name,
            error,
        } => {
            emit_agent_event(
                app,
                "agent://step_failed",
                serde_json::json!({
                    "run_id": run_id,
                    "step_number": step_index,
                    "tool_name": tool_name,
                    "error": error,
                }),
            );
            true
        }
        AgentEvent::SubAction { tool_name, summary } => {
            emit_agent_event(
                app,
                "agent://sub_action",
                serde_json::json!({
                    "run_id": run_id,
                    "tool_name": tool_name,
                    "summary": summary,
                }),
            );
            true
        }
        AgentEvent::CompletionDisagreement {
            screenshot_b64,
            vlm_reasoning,
            agent_summary,
        } => {
            emit_agent_event(
                app,
                "agent://completion_disagreement",
                serde_json::json!({
                    "run_id": run_id,
                    "screenshot_b64": screenshot_b64,
                    "vlm_reasoning": vlm_reasoning,
                    "agent_summary": agent_summary,
                }),
            );
            true
        }
        AgentEvent::ConsecutiveDestructiveCapHit {
            recent_tool_names,
            cap,
        } => {
            emit_agent_event(
                app,
                "agent://consecutive_destructive_cap_hit",
                serde_json::json!({
                    "run_id": run_id,
                    "recent_tool_names": recent_tool_names,
                    "cap": cap,
                }),
            );
            true
        }
        // `CompletionDisagreementResolved` is emitted by the Tauri layer
        // (not the engine) so the agent loop never sends it through this
        // channel. Persisting it is handled in
        // `await_disagreement_resolution`.
        AgentEvent::CompletionDisagreementResolved { .. } => true,
        _ => false,
    }
}

fn forward_state_agent_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    run_id: &str,
    event: &AgentEvent,
) -> bool {
    match event {
        AgentEvent::TaskStateChanged {
            run_id: event_run_id,
            task_state,
        } => {
            emit_agent_event(
                app,
                "agent://task_state_changed",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "task_state": task_state,
                }),
            );
            true
        }
        AgentEvent::WorldModelChanged {
            run_id: event_run_id,
            diff,
        } => {
            emit_agent_event(
                app,
                "agent://world_model_changed",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "diff": diff,
                }),
            );
            true
        }
        AgentEvent::BoundaryRecordWritten {
            run_id: event_run_id,
            boundary_kind,
            step_index,
            milestone_text,
        } => {
            emit_agent_event(
                app,
                "agent://boundary_record_written",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "boundary_kind": boundary_kind,
                    "step_index": step_index,
                    "milestone_text": milestone_text,
                }),
            );
            true
        }
        _ => false,
    }
}
fn forward_episodic_agent_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    run_id: &str,
    event: &AgentEvent,
) -> bool {
    match event {
        // Spec 2 D33: episodic-memory events. The runner emits
        // `EpisodesRetrieved` when retrieval surfaces candidates; the
        // background `EpisodicWriter` task emits `EpisodeWritten`
        // (insert/merge in the workflow-local store) and `EpisodePromoted`
        // (run-terminal promotion pass into the global store). All three
        // payloads carry the run's UUID so the frontend's stale-run
        // filter (`useAgentEvents::isStale`) drops late events from a
        // previous run.
        AgentEvent::EpisodesRetrieved {
            run_id: event_run_id,
            trigger,
            count,
            episode_ids,
            scope_breakdown,
        } => {
            emit_agent_event(
                app,
                "agent://episodes_retrieved",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "trigger": trigger,
                    "count": count,
                    "episode_ids": episode_ids,
                    "scope_breakdown": scope_breakdown,
                }),
            );
            true
        }
        AgentEvent::EpisodeWritten {
            run_id: event_run_id,
            outcome,
            episode_id,
            scope,
            occurrence_count,
        } => {
            emit_agent_event(
                app,
                "agent://episode_written",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "outcome": outcome,
                    "episode_id": episode_id,
                    "scope": scope,
                    "occurrence_count": occurrence_count,
                }),
            );
            true
        }
        AgentEvent::EpisodePromoted {
            run_id: event_run_id,
            promoted_episode_ids,
            skipped_count,
        } => {
            emit_agent_event(
                app,
                "agent://episode_promoted",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "promoted_episode_ids": promoted_episode_ids,
                    "skipped_count": skipped_count,
                }),
            );
            true
        }
        _ => false,
    }
}
fn forward_skill_agent_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    run_id: &str,
    event: &AgentEvent,
) -> bool {
    match event {
        AgentEvent::SkillInvoked {
            run_id: event_run_id,
            skill_id,
            version,
            parameter_count,
        } => {
            emit_agent_event(
                app,
                "agent://skill_invoked",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "skill_id": skill_id,
                    "version": version,
                    "parameter_count": parameter_count,
                }),
            );
            true
        }
        AgentEvent::SkillExtracted {
            run_id: event_run_id,
            skill_id,
            version,
            state,
            scope,
        } => {
            emit_agent_event(
                app,
                "agent://skill_extracted",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "skill_id": skill_id,
                    "version": version,
                    "state": state,
                    "scope": scope,
                }),
            );
            true
        }
        AgentEvent::SkillConfirmed {
            run_id: event_run_id,
            skill_id,
            version,
        } => {
            emit_agent_event(
                app,
                "agent://skill_confirmed",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "skill_id": skill_id,
                    "version": version,
                }),
            );
            true
        }
        _ => false,
    }
}
