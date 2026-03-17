use super::error::CommandError;
use super::types::*;
use clickweave_core::validate_workflow;
use clickweave_engine::{ExecutorCommand, ExecutorEvent, ExecutorState, WorkflowExecutor};
use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tokio_util::sync::CancellationToken;
use tracing::warn;

#[derive(Default)]
pub struct ExecutorHandle {
    cancel_token: Option<CancellationToken>,
    cmd_tx: Option<tokio::sync::mpsc::Sender<ExecutorCommand>>,
    task_handle: Option<tauri::async_runtime::JoinHandle<()>>,
}

impl ExecutorHandle {
    /// Stop the running executor task. Signals cancellation via the token
    /// (graceful), then aborts the tokio task (forceful fallback). The MCP
    /// subprocess is killed as a side effect: aborting the task drops
    /// `McpClient`, whose `Drop` impl calls `kill()`.
    /// Returns `true` if a task was actually running.
    pub fn force_stop(&mut self) -> bool {
        let had_task = self.task_handle.is_some();
        // Signal cancellation first (graceful)
        if let Some(token) = self.cancel_token.take() {
            token.cancel();
        }
        // Then abort the task (forceful fallback)
        if let Some(task) = self.task_handle.take() {
            task.abort();
        }
        self.cmd_tx = None;
        had_task
    }
}

#[tauri::command]
#[specta::specta]
pub async fn run_workflow(app: tauri::AppHandle, request: RunRequest) -> Result<(), CommandError> {
    {
        let handle = app.state::<Mutex<ExecutorHandle>>();
        if handle.lock().unwrap().cmd_tx.is_some() {
            return Err(CommandError::already_running());
        }
    }

    validate_workflow(&request.workflow)
        .map_err(|e| CommandError::validation(format!("Validation failed: {}", e)))?;

    let agent_config = request.agent.into_llm_config(None);
    let vlm_config = request
        .vlm
        .filter(|v| !v.is_empty())
        .map(|v| v.into_llm_config(Some(0.0)));
    let supervision_config = request
        .planner
        .filter(|p| !p.is_empty())
        .map(|p| p.into_llm_config(None));

    let storage = resolve_storage(
        &app,
        &request.project_path,
        &request.workflow.name,
        request.workflow.id,
    );
    let project_path = request.project_path.map(|p| project_dir(&p));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<ExecutorEvent>(256);
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<ExecutorCommand>(8);

    let emit_handle = app.clone();
    let cleanup_handle = emit_handle.clone();

    let mcp_configs = clickweave_mcp::default_server_configs(&request.mcp_command);
    let cancel_token = CancellationToken::new();
    let executor_token = cancel_token.clone();

    let task_handle = tauri::async_runtime::spawn(async move {
        let mut executor = WorkflowExecutor::new(
            request.workflow,
            agent_config,
            vlm_config,
            supervision_config,
            mcp_configs,
            request.execution_mode,
            project_path,
            event_tx,
            storage,
            executor_token,
        );
        executor.run(cmd_rx).await;
    });

    {
        let handle = app.state::<Mutex<ExecutorHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.cancel_token = Some(cancel_token);
        guard.cmd_tx = Some(cmd_tx);
        guard.task_handle = Some(task_handle);
    }

    tauri::async_runtime::spawn(async move {
        let mut saw_idle = false;
        while let Some(event) = event_rx.recv().await {
            if matches!(event, ExecutorEvent::StateChanged(ExecutorState::Idle)) {
                saw_idle = true;
            }
            let emit_result = match event {
                ExecutorEvent::Log(msg) | ExecutorEvent::Error(msg) => {
                    emit_handle.emit("executor://log", LogPayload { message: msg })
                }
                ExecutorEvent::StateChanged(state) => emit_handle.emit(
                    "executor://state",
                    StatePayload {
                        state: match state {
                            ExecutorState::Idle => "idle".to_owned(),
                            ExecutorState::Running => "running".to_owned(),
                        },
                    },
                ),
                ExecutorEvent::NodeStarted(id) => emit_handle.emit(
                    "executor://node_started",
                    NodePayload {
                        node_id: id.to_string(),
                    },
                ),
                ExecutorEvent::NodeCompleted(id) => emit_handle.emit(
                    "executor://node_completed",
                    NodePayload {
                        node_id: id.to_string(),
                    },
                ),
                ExecutorEvent::NodeFailed(id, err) => emit_handle.emit(
                    "executor://node_failed",
                    NodeErrorPayload {
                        node_id: id.to_string(),
                        error: err,
                    },
                ),
                ExecutorEvent::WorkflowCompleted => {
                    emit_handle.emit("executor://workflow_completed", ())
                }
                ExecutorEvent::ChecksCompleted(verdicts) => {
                    emit_handle.emit("executor://checks_completed", verdicts)
                }
                ExecutorEvent::RunCreated(_, _) => Ok(()),
                ExecutorEvent::SupervisionPassed {
                    node_id,
                    node_name,
                    summary,
                } => emit_handle.emit(
                    "executor://supervision_passed",
                    SupervisionPassedPayload {
                        node_id: node_id.to_string(),
                        node_name,
                        summary,
                    },
                ),
                ExecutorEvent::SupervisionPaused {
                    node_id,
                    node_name,
                    finding,
                    screenshot,
                } => emit_handle.emit(
                    "executor://supervision_paused",
                    SupervisionPausedPayload {
                        node_id: node_id.to_string(),
                        node_name,
                        finding,
                        screenshot,
                    },
                ),
            };
            if let Err(e) = emit_result {
                warn!("Failed to emit executor event to UI: {}", e);
            }
        }

        // On forceful abort the executor task is killed before it can emit
        // StateChanged(Idle), so the UI would stay stuck on "Running".
        // Only emit the fallback idle if the executor didn't send one itself.
        if !saw_idle {
            let _ = emit_handle.emit(
                "executor://state",
                StatePayload {
                    state: "idle".to_owned(),
                },
            );
        }

        let state = cleanup_handle.state::<Mutex<ExecutorHandle>>();
        let mut guard = state.lock().unwrap();
        guard.cancel_token = None;
        guard.cmd_tx = None;
        guard.task_handle = None;
    });

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn stop_workflow(app: tauri::AppHandle) -> Result<(), CommandError> {
    let handle = app.state::<Mutex<ExecutorHandle>>();
    let mut guard = handle.lock().unwrap();
    if !guard.force_stop() {
        return Err(CommandError::validation("No workflow is running"));
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn supervision_respond(
    app: tauri::AppHandle,
    action: String,
) -> Result<(), CommandError> {
    let handle = app.state::<Mutex<ExecutorHandle>>();
    let guard = handle.lock().unwrap();
    let tx = guard
        .cmd_tx
        .as_ref()
        .ok_or(CommandError::validation("No workflow is running"))?
        .clone();
    drop(guard);

    let command = match action.as_str() {
        "retry" => ExecutorCommand::Resume,
        "skip" => ExecutorCommand::Skip,
        "abort" => ExecutorCommand::Abort,
        _ => {
            return Err(CommandError::validation(format!(
                "Unknown supervision action: {}",
                action
            )));
        }
    };
    tx.try_send(command)
        .map_err(|e| CommandError::internal(format!("Failed to send command: {}", e)))
}
