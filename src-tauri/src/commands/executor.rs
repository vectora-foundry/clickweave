use super::error::CommandError;
use super::types::*;
use clickweave_engine::agent::skills::{ActionSketchStep, Skill, SkillStore};
use clickweave_engine::executor::skill_runner::{SkillRunContext, run_skill_steps};
use clickweave_engine::{ExecutorCommand, ExecutorEvent, ExecutorState};
use clickweave_mcp::McpClient;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tokio_util::sync::CancellationToken;
use tracing::warn;
use uuid::Uuid;

#[derive(Default)]
pub struct ExecutorHandle {
    cancel_token: Option<CancellationToken>,
    cmd_tx: Option<tokio::sync::mpsc::Sender<ExecutorCommand>>,
    task_handle: Option<tauri::async_runtime::JoinHandle<()>>,
    run_generation: u64,
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

/// IPC payload for `run_skill` (D33). Replaces the legacy `RunRequest`
/// which carried a full `Workflow` graph. Every field on the legacy
/// request that fed downstream privacy / supervision gates is preserved
/// here so Phase 1.L acceptance still passes.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct RunSkillRequest {
    /// Saved-project workspace path. `None` for unsaved projects — in that
    /// case `RunStorage::new_app_data(app_data, &project_name, project_id)`
    /// resolves the storage from the manifest identity below.
    pub project_path: Option<String>,
    /// Project identity carried forward from `ProjectManifest` (D33).
    /// Required for unsaved-project skill resolution and for run-trace
    /// storage paths.
    pub project_id: Uuid,
    pub project_name: String,
    pub skill_id: String,
    #[serde(default)]
    pub variables: HashMap<String, serde_json::Value>,
    pub agent: EndpointConfig,
    pub fast: Option<EndpointConfig>,
    /// Optional supervisor model for Test mode.
    pub supervisor: Option<EndpointConfig>,
    pub execution_mode: clickweave_core::ExecutionMode,
    #[serde(default = "default_supervision_delay_ms")]
    pub supervision_delay_ms: u64,
    /// Privacy kill switch — `Some(false)` disables run/skill artifact
    /// persistence (D31). `None` falls back to settings.
    pub store_traces: Option<bool>,
}

fn default_supervision_delay_ms() -> u64 {
    500
}

/// Dispatch a skill run via the native `skill_runner` (D28).
///
/// Resolves the requested skill from the project's `SkillStore`,
/// creates a per-run record under `<skills>/<skill_id>/runs/`, spawns
/// the MCP sidecar, and runs `run_skill_steps` against the skill's
/// `action_sketch`. Per-step events flow through the `ExecutorEvent`
/// channel and out to the UI via `executor://*` topics, mirroring the
/// shape used by the deleted `WorkflowExecutor`.
#[tauri::command]
#[specta::specta]
pub async fn run_skill(
    app: tauri::AppHandle,
    request: RunSkillRequest,
) -> Result<(), CommandError> {
    {
        let handle = app.state::<Mutex<ExecutorHandle>>();
        if handle.lock().unwrap().cmd_tx.is_some() {
            return Err(CommandError::already_running());
        }
    }

    let mut storage = resolve_storage(
        &app,
        &request.project_path,
        &request.project_name,
        request.project_id,
    );
    // Privacy kill switch — disable persistence before any run record
    // is written so an opted-out run never produces on-disk artifacts.
    let persist_traces = request.store_traces.unwrap_or(true);
    storage.set_persistent(persist_traces);

    let skills_dir = storage
        .project_skills_dir()
        .map_err(|e| CommandError::io(format!("resolve project_skills_dir: {e}")))?;
    let store = SkillStore::new(skills_dir);

    let skill = load_skill_by_id(&store, &request.skill_id)?;

    // Locate the MCP sidecar binary. Fall back to a clean error when
    // the build did not link the binary symlink so the UI surfaces a
    // helpful message rather than a generic spawn failure.
    let mcp_binary_path = {
        let status = app.state::<McpStatus>();
        match &status.0 {
            Ok(p) => p.clone(),
            Err(reason) => {
                return Err(CommandError::internal(format!(
                    "MCP sidecar unavailable: {reason}"
                )));
            }
        }
    };

    let cancel_token = CancellationToken::new();
    let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::channel::<ExecutorCommand>(8);
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<ExecutorEvent>(64);

    // Reserve a run-record on disk before spawning the runner so a
    // crash mid-spawn still leaves a parseable history entry.
    let run_record = storage
        .create_skill_run(&skill.id)
        .map_err(|e| CommandError::io(format!("create skill run: {e}")))?;

    let run_generation = {
        let handle = app.state::<Mutex<ExecutorHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.run_generation = guard.run_generation.wrapping_add(1);
        guard.cancel_token = Some(cancel_token.clone());
        guard.cmd_tx = Some(cmd_tx);
        guard.run_generation
    };

    spawn_executor_event_forwarder(app.clone(), event_rx, run_generation);

    let task_handle = tauri::async_runtime::spawn(async move {
        let _ = event_tx
            .send(ExecutorEvent::StateChanged(ExecutorState::Running))
            .await;

        let outcome = run_skill_dispatch(
            &skill,
            &request.variables,
            &mcp_binary_path,
            &cancel_token,
            &event_tx,
        )
        .await;

        // Persist the final run status. We swallow disk errors here —
        // the trace forwarder still emits the terminal events the UI
        // listens for, so a failed save doesn't lose user-visible
        // signal.
        let mut updated = run_record.clone();
        updated.finished_at = Some(chrono::Utc::now());
        updated.duration_ms = Some(
            (updated.finished_at.unwrap() - updated.started_at)
                .num_milliseconds()
                .max(0) as u64,
        );
        updated.status = match &outcome {
            Ok(()) => clickweave_core::RunStatus::Ok,
            Err(_) if cancel_token.is_cancelled() => clickweave_core::RunStatus::Cancelled,
            Err(_) => clickweave_core::RunStatus::Failed,
        };
        if let Err(e) = storage.save_skill_run(&updated) {
            warn!(error = %e, "Failed to persist skill-run terminal record");
        }

        if let Err(e) = &outcome {
            let _ = event_tx
                .send(ExecutorEvent::Error(format!("Skill run failed: {e}")))
                .await;
        }
        let _ = event_tx.send(ExecutorEvent::WorkflowCompleted).await;
        let _ = event_tx
            .send(ExecutorEvent::StateChanged(ExecutorState::Idle))
            .await;
    });

    {
        let handle = app.state::<Mutex<ExecutorHandle>>();
        let mut guard = handle.lock().unwrap();
        if guard.run_generation == run_generation {
            guard.task_handle = Some(task_handle);
        }
    }

    Ok(())
}

fn load_skill_by_id(store: &SkillStore, skill_id: &str) -> Result<Skill, CommandError> {
    let files = store
        .list_files()
        .map_err(|e| CommandError::io(format!("list skills: {e}")))?;
    for path in files {
        match store.read_skill(&path) {
            Ok(skill) if skill.id == skill_id => return Ok(skill),
            Ok(_) => continue,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Skipping unreadable skill on dispatch")
            }
        }
    }
    Err(CommandError::validation(format!(
        "Skill not found: {skill_id}"
    )))
}

async fn run_skill_dispatch(
    skill: &Skill,
    variables: &HashMap<String, serde_json::Value>,
    mcp_binary_path: &str,
    cancel_token: &CancellationToken,
    event_tx: &tokio::sync::mpsc::Sender<ExecutorEvent>,
) -> anyhow::Result<()> {
    let mcp = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled before MCP spawn");
        }
        res = McpClient::spawn(mcp_binary_path, &[]) => res?,
    };

    let mut ctx = SkillRunContext::new(&mcp, variables.clone());

    tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled");
        }
        res = run_skill_steps(&mut ctx, &skill.action_sketch) => {
            match res {
                Ok(()) => {
                    let _ = event_tx
                        .send(ExecutorEvent::Log(format!(
                            "Skill '{}' completed ({} steps)",
                            skill.name,
                            ctx.completed_steps.len()
                        )))
                        .await;
                    Ok(())
                }
                Err(e) => Err(anyhow::anyhow!(format!("{e}"))),
            }
        }
    }
}

fn spawn_executor_event_forwarder(
    emit_handle: tauri::AppHandle,
    mut event_rx: tokio::sync::mpsc::Receiver<ExecutorEvent>,
    run_generation: u64,
) {
    let cleanup_handle = emit_handle.clone();
    tauri::async_runtime::spawn(async move {
        let mut saw_idle = false;
        while let Some(event) = event_rx.recv().await {
            if matches!(event, ExecutorEvent::StateChanged(ExecutorState::Idle)) {
                saw_idle = true;
            }
            if let Err(e) = emit_executor_event(&emit_handle, event) {
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
        clear_executor_handle_if_current(&mut guard, run_generation);
    });
}

fn clear_executor_handle_if_current(guard: &mut ExecutorHandle, run_generation: u64) {
    if guard.run_generation != run_generation {
        return;
    }

    guard.cancel_token = None;
    guard.cmd_tx = None;
    guard.task_handle = None;
}

fn emit_executor_event(emit_handle: &tauri::AppHandle, event: ExecutorEvent) -> tauri::Result<()> {
    match event {
        ExecutorEvent::Log(msg) => emit_handle.emit("executor://log", LogPayload { message: msg }),
        ExecutorEvent::Error(msg) => {
            emit_handle.emit("executor://error", LogPayload { message: msg })
        }
        ExecutorEvent::StateChanged(state) => {
            emit_handle.emit("executor://state", StatePayload::from_state(state))
        }
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
        ExecutorEvent::WorkflowCompleted => emit_handle.emit("executor://workflow_completed", ()),
        ExecutorEvent::ChecksCompleted(verdicts) => {
            emit_handle.emit("executor://checks_completed", verdicts)
        }
        ExecutorEvent::RunCreated(_, _) => Ok(()),
        ExecutorEvent::SupervisionPassed { scope, summary } => emit_handle.emit(
            "executor://supervision_passed",
            SupervisionPassedPayload { scope, summary },
        ),
        ExecutorEvent::SupervisionPaused {
            scope,
            finding,
            screenshot,
        } => emit_handle.emit(
            "executor://supervision_paused",
            SupervisionPausedPayload {
                scope,
                finding,
                screenshot,
            },
        ),
        ExecutorEvent::AmbiguityResolved {
            node_id,
            target,
            candidates,
            chosen_uid,
            reasoning,
            viewport_width,
            viewport_height,
            screenshot_path,
            screenshot_base64,
        } => emit_handle.emit(
            "executor://ambiguity_resolved",
            AmbiguityResolvedPayload {
                node_id: node_id.to_string(),
                target,
                candidates: candidates
                    .into_iter()
                    .map(CandidateViewPayload::from)
                    .collect(),
                chosen_uid,
                reasoning,
                viewport_width,
                viewport_height,
                screenshot_path,
                screenshot_base64,
            },
        ),
        ExecutorEvent::NodeCancelled(id) => emit_handle.emit(
            "executor://node_cancelled",
            NodePayload {
                node_id: id.to_string(),
            },
        ),
    }
}

impl StatePayload {
    fn from_state(state: ExecutorState) -> Self {
        Self {
            state: match state {
                ExecutorState::Idle => "idle".to_owned(),
                ExecutorState::Running => "running".to_owned(),
            },
        }
    }
}

impl From<clickweave_engine::CandidateView> for CandidateViewPayload {
    fn from(candidate: clickweave_engine::CandidateView) -> Self {
        Self {
            uid: candidate.uid,
            snippet: candidate.snippet,
            rect: candidate.rect.map(|r| CandidateRectPayload {
                x: r.x,
                y: r.y,
                width: r.width,
                height: r.height,
            }),
        }
    }
}

/// Request body for `resume_skill_from_failure`.
/// Inherits all fields from `RunSkillRequest` and adds the section to resume from.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ResumeSkillFromFailureRequest {
    pub project_path: Option<String>,
    pub project_id: Uuid,
    pub project_name: String,
    pub skill_id: String,
    #[serde(default)]
    pub variables: HashMap<String, serde_json::Value>,
    pub agent: EndpointConfig,
    pub fast: Option<EndpointConfig>,
    pub supervisor: Option<EndpointConfig>,
    pub execution_mode: clickweave_core::ExecutionMode,
    #[serde(default = "default_supervision_delay_ms")]
    pub supervision_delay_ms: u64,
    pub store_traces: Option<bool>,
    /// The section ID to resume from. All sections before this section are skipped.
    pub from_section_id: String,
}

/// Resume a skill run from a specific section after a failure.
///
/// Loads the skill's section list, collects the step IDs for every section
/// at or after `from_section_id`, and then runs only those steps. Sections
/// before `from_section_id` are skipped entirely.
#[tauri::command]
#[specta::specta]
pub async fn resume_skill_from_failure(
    app: tauri::AppHandle,
    request: ResumeSkillFromFailureRequest,
) -> Result<(), CommandError> {
    {
        let handle = app.state::<Mutex<ExecutorHandle>>();
        if handle.lock().unwrap().cmd_tx.is_some() {
            return Err(CommandError::already_running());
        }
    }

    let mut storage = resolve_storage(
        &app,
        &request.project_path,
        &request.project_name,
        request.project_id,
    );
    let persist_traces = request.store_traces.unwrap_or(true);
    storage.set_persistent(persist_traces);

    let skills_dir = storage
        .project_skills_dir()
        .map_err(|e| CommandError::io(format!("resolve project_skills_dir: {e}")))?;
    let store = SkillStore::new(skills_dir);
    let skill = load_skill_by_id(&store, &request.skill_id)?;

    // Collect step IDs for sections at-or-after from_section_id.
    let resume_step_ids: HashSet<String> = {
        let sections = skill.sections.as_slice();
        let start_idx = sections
            .iter()
            .position(|s| s.id == request.from_section_id)
            .ok_or_else(|| {
                CommandError::validation(format!(
                    "section not found in skill: {}",
                    request.from_section_id
                ))
            })?;
        sections[start_idx..]
            .iter()
            .flat_map(|s| s.step_ids.iter().cloned())
            .collect()
    };

    // Filter the action_sketch to only include steps in resume_step_ids.
    let filtered_sketch: Vec<ActionSketchStep> = skill
        .action_sketch
        .iter()
        .filter(|step| {
            let step_id = match step {
                ActionSketchStep::ToolCall { step_id, .. } => step_id,
                ActionSketchStep::Loop { step_id, .. } => step_id,
            };
            resume_step_ids.contains(step_id)
        })
        .cloned()
        .collect();

    let mcp_binary_path = {
        let status = app.state::<McpStatus>();
        match &status.0 {
            Ok(p) => p.clone(),
            Err(reason) => {
                return Err(CommandError::internal(format!(
                    "MCP sidecar unavailable: {reason}"
                )));
            }
        }
    };

    let cancel_token = CancellationToken::new();
    let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::channel::<ExecutorCommand>(8);
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<ExecutorEvent>(64);

    let run_record = storage
        .create_skill_run(&skill.id)
        .map_err(|e| CommandError::io(format!("create skill run: {e}")))?;

    let run_generation = {
        let handle = app.state::<Mutex<ExecutorHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.run_generation = guard.run_generation.wrapping_add(1);
        guard.cancel_token = Some(cancel_token.clone());
        guard.cmd_tx = Some(cmd_tx);
        guard.run_generation
    };

    spawn_executor_event_forwarder(app.clone(), event_rx, run_generation);

    let variables = request.variables.clone();
    let skill_name = skill.name.clone();
    let task_handle = tauri::async_runtime::spawn(async move {
        let _ = event_tx
            .send(ExecutorEvent::StateChanged(ExecutorState::Running))
            .await;

        let outcome = run_skill_steps_from_filtered(
            &filtered_sketch,
            &variables,
            &mcp_binary_path,
            &cancel_token,
            &event_tx,
            &skill_name,
        )
        .await;

        let mut updated = run_record.clone();
        updated.finished_at = Some(chrono::Utc::now());
        updated.duration_ms = Some(
            (updated.finished_at.unwrap() - updated.started_at)
                .num_milliseconds()
                .max(0) as u64,
        );
        updated.status = match &outcome {
            Ok(()) => clickweave_core::RunStatus::Ok,
            Err(_) if cancel_token.is_cancelled() => clickweave_core::RunStatus::Cancelled,
            Err(_) => clickweave_core::RunStatus::Failed,
        };
        if let Err(e) = storage.save_skill_run(&updated) {
            warn!(error = %e, "Failed to persist resume-run terminal record");
        }
        if let Err(e) = &outcome {
            let _ = event_tx
                .send(ExecutorEvent::Error(format!("Skill resume failed: {e}")))
                .await;
        }
        let _ = event_tx.send(ExecutorEvent::WorkflowCompleted).await;
        let _ = event_tx
            .send(ExecutorEvent::StateChanged(ExecutorState::Idle))
            .await;
    });

    {
        let handle = app.state::<Mutex<ExecutorHandle>>();
        let mut guard = handle.lock().unwrap();
        if guard.run_generation == run_generation {
            guard.task_handle = Some(task_handle);
        }
    }

    Ok(())
}

async fn run_skill_steps_from_filtered(
    filtered_sketch: &[ActionSketchStep],
    variables: &HashMap<String, serde_json::Value>,
    mcp_binary_path: &str,
    cancel_token: &CancellationToken,
    event_tx: &tokio::sync::mpsc::Sender<ExecutorEvent>,
    skill_name: &str,
) -> anyhow::Result<()> {
    let mcp = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled before MCP spawn");
        }
        res = McpClient::spawn(mcp_binary_path, &[]) => res?,
    };

    let mut ctx = SkillRunContext::new(&mcp, variables.clone());

    tokio::select! {
        biased;
        _ = cancel_token.cancelled() => {
            anyhow::bail!("Cancelled");
        }
        res = run_skill_steps(&mut ctx, filtered_sketch) => {
            match res {
                Ok(()) => {
                    let _ = event_tx
                        .send(ExecutorEvent::Log(format!(
                            "Skill '{}' resume completed ({} steps)",
                            skill_name,
                            ctx.completed_steps.len()
                        )))
                        .await;
                    Ok(())
                }
                Err(e) => Err(anyhow::anyhow!(format!("{e}"))),
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executor_cleanup_clears_current_generation_handles() {
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::channel(1);
        let mut handle = ExecutorHandle {
            cancel_token: Some(CancellationToken::new()),
            cmd_tx: Some(cmd_tx),
            task_handle: None,
            run_generation: 7,
        };

        clear_executor_handle_if_current(&mut handle, 7);

        assert!(handle.cancel_token.is_none());
        assert!(handle.cmd_tx.is_none());
        assert!(handle.task_handle.is_none());
    }

    #[test]
    fn executor_cleanup_preserves_newer_generation_handles() {
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::channel(1);
        let mut handle = ExecutorHandle {
            cancel_token: Some(CancellationToken::new()),
            cmd_tx: Some(cmd_tx),
            task_handle: None,
            run_generation: 8,
        };

        clear_executor_handle_if_current(&mut handle, 7);

        assert!(handle.cancel_token.is_some());
        assert!(handle.cmd_tx.is_some());
        assert!(handle.task_handle.is_none());
    }
}
