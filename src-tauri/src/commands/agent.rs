use super::error::CommandError;
use super::types::*;
use clickweave_core::variant_index::{VariantEntry, VariantIndex};
use clickweave_engine::agent::{
    AgentCache, AgentChannels, AgentConfig, AgentEvent, ApprovalRequest,
};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
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
    pub summary: String,
    pub tool_name: String,
    pub step_number: usize,
}

// ── Handle ──────────────────────────────────────────────────────

#[derive(Default)]
pub struct AgentHandle {
    cancel_token: Option<CancellationToken>,
    steering_tx: Option<tokio::sync::mpsc::Sender<String>>,
    task_handle: Option<tauri::async_runtime::JoinHandle<()>>,
    /// Pending approval oneshot sender — set when the agent is waiting for approval.
    pending_approval_tx: Option<tokio::sync::oneshot::Sender<bool>>,
}

impl AgentHandle {
    /// Cancel the running agent task and abort the tokio task.
    /// Returns `true` if a task was actually running.
    pub fn force_stop(&mut self) -> bool {
        let had_task = self.task_handle.is_some();
        if let Some(token) = self.cancel_token.take() {
            token.cancel();
        }
        if let Some(task) = self.task_handle.take() {
            task.abort();
        }
        self.steering_tx = None;
        self.pending_approval_tx = None;
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
        if handle.lock().unwrap().task_handle.is_some() {
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

    let mut storage = resolve_storage(
        &app,
        &request.project_path,
        &request.workflow_name,
        workflow_id,
    );

    let cancel_token = CancellationToken::new();
    let agent_token = cancel_token.clone();

    let (_steering_tx, _steering_rx) = tokio::sync::mpsc::channel::<String>(8);

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

    // Channel used to signal the cleanup task when the agent task finishes.
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

    let task_handle = tauri::async_runtime::spawn(async move {
        // Spawn MCP server
        let mcp = match clickweave_mcp::McpClient::spawn(&mcp_binary_path, &[]).await {
            Ok(m) => m,
            Err(e) => {
                let _ = emit_handle.emit(
                    "agent://error",
                    serde_json::json!({ "message": format!("MCP spawn failed: {e}") }),
                );
                let _ = done_tx.send(());
                return;
            }
        };

        // Create LLM client
        let llm = clickweave_llm::LlmClient::new(agent_config);
        let config = AgentConfig::default();

        // Begin storage execution
        let _exec_dir = storage.begin_execution();

        // Load cross-run variant index and decision cache
        let variant_index = VariantIndex::load(&storage.variant_index_path());
        let variant_context = variant_index.as_context_text();
        let cache = AgentCache::load_from_path(&storage.agent_cache_path());

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
                    "agent://error",
                    serde_json::json!({ "message": "Agent cancelled" }),
                );
                let _ = done_tx.send(());
                return;
            }
        };

        match result {
            Ok((state, updated_cache)) => {
                // Persist the updated cache
                let _ = updated_cache.save_to_path(&storage.agent_cache_path());

                // Record this run as a variant entry
                let variant_entry = VariantEntry {
                    execution_dir: storage
                        .execution_dir_name()
                        .unwrap_or("unknown")
                        .to_string(),
                    diverged_at_step: None,
                    divergence_summary: if state.completed {
                        "Followed reference trajectory".to_string()
                    } else {
                        format!(
                            "Stopped after {} steps without completing",
                            state.steps.len()
                        )
                    },
                    success: state.completed,
                };
                let _ = VariantIndex::append(&storage.variant_index_path(), &variant_entry);

                // Final complete event (steps are emitted live now)
                let _ = emit_handle.emit("agent://complete", ());
            }
            Err(e) => {
                let _ = emit_handle.emit(
                    "agent://error",
                    serde_json::json!({ "message": format!("{e}") }),
                );
            }
        }

        let _ = done_tx.send(());
    });

    // Spawn a task to forward live agent events to the Tauri frontend.
    tauri::async_runtime::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match &event {
                AgentEvent::StepCompleted {
                    step_index,
                    tool_name,
                    summary,
                } => {
                    let _ = event_emit_handle.emit(
                        "agent://step",
                        AgentStepPayload {
                            summary: summary.clone(),
                            tool_name: tool_name.clone(),
                            step_number: *step_index,
                        },
                    );
                }
                AgentEvent::NodeAdded { node } => {
                    let _ = event_emit_handle.emit("agent://node_added", node);
                }
                AgentEvent::EdgeAdded { edge } => {
                    let _ = event_emit_handle.emit("agent://edge_added", edge);
                }
                AgentEvent::GoalComplete { summary } => {
                    let _ = event_emit_handle.emit(
                        "agent://goal_complete",
                        serde_json::json!({ "summary": summary }),
                    );
                }
                AgentEvent::Error { message } => {
                    let _ = event_emit_handle
                        .emit("agent://error", serde_json::json!({ "message": message }));
                }
                AgentEvent::CdpConnected { app_name, port } => {
                    let _ = event_emit_handle.emit(
                        "agent://cdp_connected",
                        serde_json::json!({
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
                            "step_number": step_index,
                            "tool_name": tool_name,
                            "error": error,
                        }),
                    );
                }
            }
        }
    });

    // Spawn a task to forward approval requests to the Tauri frontend
    // and store the oneshot response sender in the handle.
    tauri::async_runtime::spawn(async move {
        while let Some((request, resp_tx)) = approval_rx.recv().await {
            // Store the response sender so `approve_agent_action` can use it
            {
                let handle = approval_emit_handle.state::<Mutex<AgentHandle>>();
                let mut guard = handle.lock().unwrap();
                guard.pending_approval_tx = Some(resp_tx);
            }
            // Emit the approval request to the frontend
            let _ = approval_emit_handle.emit("agent://approval_required", &request);
        }
    });

    {
        let handle = app.state::<Mutex<AgentHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.cancel_token = Some(cancel_token);
        guard.steering_tx = Some(_steering_tx);
        guard.task_handle = Some(task_handle);
    }

    // Spawn cleanup task: wait for the agent task to signal completion, then clear the handle.
    tauri::async_runtime::spawn(async move {
        let _ = done_rx.await;

        let handle = cleanup_handle.state::<Mutex<AgentHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.cancel_token = None;
        guard.steering_tx = None;
        guard.task_handle = None;
        guard.pending_approval_tx = None;
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
pub async fn steer_agent(app: tauri::AppHandle, message: String) -> Result<(), CommandError> {
    let handle = app.state::<Mutex<AgentHandle>>();
    let guard = handle.lock().unwrap();
    let tx = guard
        .steering_tx
        .as_ref()
        .ok_or(CommandError::validation("No agent is running"))?
        .clone();
    drop(guard);

    tx.try_send(message)
        .map_err(|e| CommandError::internal(format!("Failed to send steering message: {e}")))
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

    let _ = tx.send(approved);
    Ok(())
}
