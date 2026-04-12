use super::error::CommandError;
use super::types::*;
use clickweave_core::variant_index::{VariantEntry, VariantIndex};
use clickweave_engine::agent::{
    AgentCache, AgentChannels, AgentConfig, AgentEvent, ApprovalRequest,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use tokio_util::sync::CancellationToken;

// ── Request / payload types ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct AgentRunRequest {
    pub goal: String,
    pub agent: EndpointConfig,
    pub project_path: Option<String>,
    pub workflow_name: String,
    pub workflow_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentStepPayload {
    pub run_id: String,
    pub summary: String,
    pub tool_name: String,
    pub step_number: usize,
}

// ── Handle ──────────────────────────────────────────────────────

#[derive(Default)]
pub struct AgentHandle {
    cancel_token: Option<CancellationToken>,
    task_handle: Option<tauri::async_runtime::JoinHandle<()>>,
    /// Pending approval oneshot sender — set when the agent is waiting for approval.
    pending_approval_tx: Option<tokio::sync::oneshot::Sender<bool>>,
    /// Generation ID for the current run. Used to tag events and reject stale ones.
    run_id: Option<String>,
}

impl AgentHandle {
    /// Cancel the running agent task.
    /// Returns `true` if a task was actually running (or starting).
    pub fn force_stop(&mut self) -> bool {
        // Check cancel_token too — it's installed before the task handle
        // so stop_agent works even during the spawn window.
        let had_task = self.cancel_token.is_some() || self.task_handle.is_some();
        if let Some(token) = self.cancel_token.take() {
            token.cancel();
        }
        // Send explicit cancellation through the approval channel instead
        // of silently dropping the sender. This ensures the engine sees
        // `Ok(false)` (rejection/replan) rather than `Err` (channel closed),
        // which would surface as `approval_unavailable` instead of `cancelled`.
        if let Some(tx) = self.pending_approval_tx.take() {
            let _ = tx.send(false);
        }
        had_task
    }
}

// ── Commands ────────────────────────────────────────────────────

#[tauri::command]
#[specta::specta]
pub async fn run_agent(
    app: tauri::AppHandle,
    request: AgentRunRequest,
) -> Result<(), CommandError> {
    {
        let handle = app.state::<Mutex<AgentHandle>>();
        let guard = handle.lock().unwrap();
        if guard.cancel_token.is_some() || guard.task_handle.is_some() {
            return Err(CommandError::already_running());
        }
    }

    let agent_config = request.agent.into_llm_config(None);
    let mcp_binary_path =
        crate::mcp_resolve::resolve_mcp_binary().map_err(|e| CommandError::mcp(format!("{e}")))?;

    let workflow_id: uuid::Uuid = request
        .workflow_id
        .parse()
        .map_err(|_| CommandError::validation("Invalid workflow ID"))?;

    let storage = resolve_storage(
        &app,
        &request.project_path,
        &request.workflow_name,
        workflow_id,
    );
    let storage = Arc::new(Mutex::new(storage));

    // Generate a per-run generation ID so event consumers can reject
    // stale events from a previous run that drain after stop/restart.
    let run_id = uuid::Uuid::new_v4().to_string();

    let cancel_token = CancellationToken::new();
    let agent_token = cancel_token.clone();
    let forwarder_token = cancel_token.clone();
    let event_forwarder_token = cancel_token.clone();

    // Live event channel: agent runner -> Tauri event emitter
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);

    // Approval channel: agent runner sends requests, we forward to UI and store
    // the oneshot response sender in the handle for `approve_agent_action` to use.
    let (approval_tx, mut approval_rx) =
        tokio::sync::mpsc::channel::<(ApprovalRequest, tokio::sync::oneshot::Sender<bool>)>(1);

    let emit_handle = app.clone();
    let event_emit_handle = app.clone();
    let approval_emit_handle = app.clone();
    let cleanup_handle = app.clone();
    let goal = request.goal.clone();
    let task_storage = storage.clone();
    let event_storage = storage.clone();
    let task_run_id = run_id.clone();
    let event_run_id = run_id.clone();
    let approval_run_id = run_id.clone();

    // Channels used to signal cleanup when the agent task, event forwarder,
    // and approval forwarder have all finished, preventing stale event leakage.
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let (events_done_tx, events_done_rx) = tokio::sync::oneshot::channel::<()>();
    let (approval_done_tx, approval_done_rx) = tokio::sync::oneshot::channel::<()>();

    // Install cancel_token and run_id before spawning so stop_agent() works
    // even during the spawn window (before task_handle is available).
    {
        let handle = app.state::<Mutex<AgentHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.cancel_token = Some(cancel_token);
        guard.run_id = Some(run_id.clone());
    }

    // Emit agent://started so the frontend knows the run_id before any other events.
    let _ = app.emit("agent://started", serde_json::json!({ "run_id": &run_id }));

    let task_handle = tauri::async_runtime::spawn(async move {
        // Spawn MCP server — cancellation-aware so stop_agent() works
        // even during slow MCP startup / handshake.
        let mcp = tokio::select! {
            res = clickweave_mcp::McpClient::spawn(&mcp_binary_path, &[]) => {
                match res {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = emit_handle.emit(
                            "agent://error",
                            serde_json::json!({ "run_id": task_run_id, "message": format!("MCP spawn failed: {e}") }),
                        );
                        let _ = done_tx.send(());
                        return;
                    }
                }
            }
            _ = agent_token.cancelled() => {
                let _ = emit_handle.emit(
                    "agent://stopped",
                    serde_json::json!({ "run_id": task_run_id, "reason": "cancelled" }),
                );
                let _ = done_tx.send(());
                return;
            }
        };

        // Create LLM client
        let llm = clickweave_llm::LlmClient::new(agent_config);
        let config = AgentConfig::default();

        // Begin storage execution and load cross-run state under a single lock.
        // Storage init failure prevents the run from starting — durable tracing
        // must be available before executing any agent actions.
        let storage = task_storage;
        let (variant_context, cache) = {
            let mut guard = storage.lock().unwrap();
            match guard.begin_execution() {
                Ok(_) => {}
                Err(e) => {
                    let _ = emit_handle.emit(
                        "agent://error",
                        serde_json::json!({
                            "run_id": task_run_id,
                            "message": format!("Run storage init failed: {e}"),
                        }),
                    );
                    let _ = done_tx.send(());
                    return;
                }
            }
            let variant_index = VariantIndex::load(&guard.variant_index_path());
            let cache = AgentCache::load_from_path(&guard.agent_cache_path());
            (variant_index.as_context_text(), cache)
        };

        let channels = AgentChannels {
            event_tx,
            approval_tx,
        };

        // Run the agent loop
        let result = tokio::select! {
            res = clickweave_engine::agent::run_agent_workflow(
                &llm,
                config,
                goal,
                &mcp,
                if variant_context.is_empty() { None } else { Some(&variant_context) },
                Some(cache),
                Some(channels),
            ) => res,
            _ = agent_token.cancelled() => {
                let _ = emit_handle.emit(
                    "agent://stopped",
                    serde_json::json!({ "run_id": task_run_id, "reason": "cancelled" }),
                );
                let _ = done_tx.send(());
                return;
            }
        };

        match result {
            Ok((state, updated_cache)) => {
                // Persist the updated cache
                let _ = updated_cache.save_to_path(&storage.lock().unwrap().agent_cache_path());

                // Derive variant metadata from terminal reason
                use clickweave_engine::agent::TerminalReason;
                let (divergence_summary, success) = match &state.terminal_reason {
                    Some(reason) => (reason.divergence_summary(), reason.is_completed()),
                    None => ("Stopped: unknown reason".to_string(), false),
                };

                let variant_entry = VariantEntry {
                    execution_dir: storage
                        .lock()
                        .unwrap()
                        .execution_dir_name()
                        .unwrap_or("unknown")
                        .to_string(),
                    diverged_at_step: None,
                    divergence_summary,
                    success,
                };
                let _ = VariantIndex::append(
                    &storage.lock().unwrap().variant_index_path(),
                    &variant_entry,
                );

                // Emit truthful terminal event.
                match &state.terminal_reason {
                    Some(TerminalReason::Completed { summary }) => {
                        let _ = emit_handle.emit(
                            "agent://complete",
                            serde_json::json!({ "run_id": task_run_id, "summary": summary }),
                        );
                    }
                    Some(reason) => {
                        let mut payload = serde_json::to_value(reason).unwrap_or_default();
                        if let Some(obj) = payload.as_object_mut() {
                            obj.insert("run_id".to_string(), serde_json::json!(task_run_id));
                        }
                        let _ = emit_handle.emit("agent://stopped", payload);
                    }
                    None => {
                        let _ = emit_handle.emit(
                            "agent://stopped",
                            serde_json::json!({ "run_id": task_run_id, "reason": "unknown" }),
                        );
                    }
                }
            }
            Err(e) => {
                let _ = emit_handle.emit(
                    "agent://error",
                    serde_json::json!({ "run_id": task_run_id, "message": format!("{e}") }),
                );
            }
        }

        let _ = done_tx.send(());
    });

    // Spawn a task to forward live agent events to the Tauri frontend.
    // Cancellation-aware: stops accepting new events once the run is cancelled,
    // then drains any remaining buffered events and signals completion.
    // Persistence is synchronous within the forwarder to guarantee ordering
    // and completeness in events.jsonl. The agent loop emits events at LLM
    // pace (~seconds per step), so the I/O cost is negligible.
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = event_forwarder_token.cancelled() => {
                    // Drain remaining buffered events before exiting
                    while let Ok(event) = event_rx.try_recv() {
                        let _ = event_storage.lock().unwrap().append_agent_event(&event);
                    }
                    break;
                }
                maybe_event = event_rx.recv() => {
                    match maybe_event {
                        Some(event) => {
                            // Durable trace: persist every event to events.jsonl
                            let _ = event_storage.lock().unwrap().append_agent_event(&event);

                            match &event {
                                AgentEvent::StepCompleted {
                                    step_index,
                                    tool_name,
                                    summary,
                                } => {
                                    let _ = event_emit_handle.emit(
                                        "agent://step",
                                        AgentStepPayload {
                                            run_id: event_run_id.clone(),
                                            summary: summary.clone(),
                                            tool_name: tool_name.clone(),
                                            step_number: *step_index,
                                        },
                                    );
                                }
                                AgentEvent::NodeAdded { node } => {
                                    let _ = event_emit_handle.emit("agent://node_added",
                                        serde_json::json!({ "run_id": event_run_id, "node": node }));
                                }
                                AgentEvent::EdgeAdded { edge } => {
                                    let _ = event_emit_handle.emit("agent://edge_added",
                                        serde_json::json!({ "run_id": event_run_id, "edge": edge }));
                                }
                                AgentEvent::GoalComplete { .. } => {
                                    // Terminal completion is emitted as agent://complete
                                    // by the main task after the agent loop finishes.
                                    // This in-band event is only used for durable tracing.
                                }
                                AgentEvent::Error { message } => {
                                    let _ = event_emit_handle.emit(
                                        "agent://error",
                                        serde_json::json!({ "run_id": event_run_id, "message": message }),
                                    );
                                }
                                AgentEvent::Warning { message } => {
                                    let _ = event_emit_handle.emit(
                                        "agent://warning",
                                        serde_json::json!({ "run_id": event_run_id, "message": message }),
                                    );
                                }
                                AgentEvent::CdpConnected { app_name, port } => {
                                    let _ = event_emit_handle.emit(
                                        "agent://cdp_connected",
                                        serde_json::json!({
                                            "run_id": event_run_id,
                                            "app_name": app_name,
                                            "port": port,
                                        }),
                                    );
                                }
                                AgentEvent::StepFailed {
                                    step_index,
                                    tool_name,
                                    error,
                                } => {
                                    let _ = event_emit_handle.emit(
                                        "agent://step_failed",
                                        serde_json::json!({
                                            "run_id": event_run_id,
                                            "step_number": step_index,
                                            "tool_name": tool_name,
                                            "error": error,
                                        }),
                                    );
                                }
                                AgentEvent::SubAction { tool_name, summary } => {
                                    let _ = event_emit_handle.emit(
                                        "agent://sub_action",
                                        serde_json::json!({
                                            "run_id": event_run_id,
                                            "tool_name": tool_name,
                                            "summary": summary,
                                        }),
                                    );
                                }
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        let _ = events_done_tx.send(());
    });

    // Spawn a task to forward approval requests to the Tauri frontend
    // and store the oneshot response sender in the handle.
    // Cancellation-aware so it stops when force_stop() fires, preventing
    // stale approvals from leaking into a subsequent run.
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                req = approval_rx.recv() => {
                    match req {
                        Some((request, resp_tx)) => {
                            // After winning the select race, re-check cancellation
                            // to avoid leaking a stale approval into the next run.
                            // Send explicit rejection so the engine sees Ok(false)
                            // instead of Err (channel closed → ApprovalUnavailable).
                            if forwarder_token.is_cancelled() {
                                let _ = resp_tx.send(false);
                                break;
                            }
                            {
                                let handle = approval_emit_handle.state::<Mutex<AgentHandle>>();
                                let mut guard = handle.lock().unwrap();
                                guard.pending_approval_tx = Some(resp_tx);
                            }
                            let _ = approval_emit_handle.emit(
                                "agent://approval_required",
                                serde_json::json!({
                                    "run_id": approval_run_id,
                                    "step_index": request.step_index,
                                    "tool_name": request.tool_name,
                                    "arguments": request.arguments,
                                    "description": request.description,
                                }),
                            );
                        }
                        None => break,
                    }
                }
                _ = forwarder_token.cancelled() => {
                    // Drain any queued approval requests, sending rejection
                    // so the engine sees Ok(false) instead of a channel drop.
                    while let Ok((_req, resp_tx)) = approval_rx.try_recv() {
                        let _ = resp_tx.send(false);
                    }
                    break;
                }
            }
        }
        let _ = approval_done_tx.send(());
    });

    // Store task_handle now that it's available (cancel_token + run_id
    // were already installed before spawn).
    {
        let handle = app.state::<Mutex<AgentHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.task_handle = Some(task_handle);
    }

    // Spawn cleanup task: wait for the agent task, event forwarder, and
    // approval forwarder to all complete before clearing the handle. This
    // prevents stale buffered events or approvals from leaking into a
    // subsequent run.
    tauri::async_runtime::spawn(async move {
        let _ = done_rx.await;
        let _ = events_done_rx.await;
        let _ = approval_done_rx.await;

        let handle = cleanup_handle.state::<Mutex<AgentHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.cancel_token = None;
        guard.task_handle = None;
        guard.pending_approval_tx = None;
        guard.run_id = None;
    });

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn stop_agent(app: tauri::AppHandle) -> Result<(), CommandError> {
    let handle = app.state::<Mutex<AgentHandle>>();
    let mut guard = handle.lock().unwrap();
    if !guard.force_stop() {
        return Err(CommandError::validation("No agent is running"));
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn approve_agent_action(
    app: tauri::AppHandle,
    approved: bool,
) -> Result<(), CommandError> {
    let handle = app.state::<Mutex<AgentHandle>>();
    let mut guard = handle.lock().unwrap();
    let tx = guard
        .pending_approval_tx
        .take()
        .ok_or(CommandError::validation("No pending approval request"))?;
    drop(guard);

    tx.send(approved).map_err(|_| {
        CommandError::validation("Approval channel closed — agent task may have ended")
    })
}
