use super::*;

pub(super) fn spawn_agent_event_forwarder(
    event_forwarder_token: CancellationToken,
    mut event_rx: tokio::sync::mpsc::Receiver<RunnerOutput>,
    event_storage: Arc<Mutex<clickweave_core::storage::RunStorage>>,
    event_emit_handle: tauri::AppHandle,
    event_run_id: String,
    proposal_skill_ctx: SkillContext,
    proposal_agent_config: clickweave_llm::LlmConfig,
    events_done_tx: tokio::sync::oneshot::Sender<()>,
) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = event_forwarder_token.cancelled() => {
                    drain_remaining_runner_outputs(
                        &mut event_rx,
                        &event_storage,
                        &proposal_skill_ctx,
                        proposal_agent_config.clone(),
                    );
                    break;
                }
                maybe_output = event_rx.recv() => {
                    match maybe_output {
                        Some(output) => handle_runner_output(
                            output,
                            &event_storage,
                            &event_emit_handle,
                            &event_run_id,
                            &proposal_skill_ctx,
                            proposal_agent_config.clone(),
                        ),
                        None => break,
                    }
                }
            }
        }
        let _ = events_done_tx.send(());
    });
}

fn drain_remaining_runner_outputs(
    event_rx: &mut tokio::sync::mpsc::Receiver<RunnerOutput>,
    event_storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    proposal_skill_ctx: &SkillContext,
    proposal_agent_config: clickweave_llm::LlmConfig,
) {
    while let Ok(output) = event_rx.try_recv() {
        match output {
            RunnerOutput::Event(event) => {
                let _ = event_storage.lock().unwrap().append_agent_event(&event);
            }
            RunnerOutput::DrainBarrier { ack } => {
                let _ = ack.send(());
            }
            RunnerOutput::SkillProposalNeeded {
                skill_id, version, ..
            } => {
                spawn_skill_proposal_task(
                    proposal_skill_ctx,
                    proposal_agent_config.clone(),
                    skill_id,
                    version,
                );
            }
        }
    }
}

fn handle_runner_output(
    output: RunnerOutput,
    event_storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    event_emit_handle: &tauri::AppHandle,
    event_run_id: &str,
    proposal_skill_ctx: &SkillContext,
    proposal_agent_config: clickweave_llm::LlmConfig,
) {
    match output {
        RunnerOutput::Event(event) => {
            let _ = event_storage.lock().unwrap().append_agent_event(&event);
            forward_agent_event(event_emit_handle, event_run_id, &event);
            maybe_spawn_skill_proposal_task(&event, proposal_skill_ctx, proposal_agent_config);
        }
        RunnerOutput::DrainBarrier { ack } => {
            let _ = ack.send(());
        }
        RunnerOutput::SkillProposalNeeded {
            skill_id, version, ..
        } => {
            spawn_skill_proposal_task(proposal_skill_ctx, proposal_agent_config, skill_id, version);
        }
    }
}

pub(super) fn spawn_approval_forwarder(
    mut approval_rx: tokio::sync::mpsc::Receiver<(
        ApprovalRequest,
        tokio::sync::oneshot::Sender<bool>,
    )>,
    forwarder_token: CancellationToken,
    approval_emit_handle: tauri::AppHandle,
    approval_run_id: String,
    approval_done_tx: tokio::sync::oneshot::Sender<()>,
) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                req = approval_rx.recv() => {
                    match req {
                        Some((request, resp_tx)) => {
                            if forwarder_token.is_cancelled() {
                                let _ = resp_tx.send(false);
                                break;
                            }
                            install_pending_approval(&approval_emit_handle, resp_tx);
                            emit_approval_required(&approval_emit_handle, &approval_run_id, request);
                        }
                        None => break,
                    }
                }
                _ = forwarder_token.cancelled() => {
                    reject_queued_approval_requests(&mut approval_rx);
                    break;
                }
            }
        }
        let _ = approval_done_tx.send(());
    });
}

fn install_pending_approval(app: &tauri::AppHandle, resp_tx: tokio::sync::oneshot::Sender<bool>) {
    let handle = app.state::<Mutex<AgentHandle>>();
    let mut guard = handle.lock().unwrap();
    guard.pending_approval_tx = Some(resp_tx);
}

fn emit_approval_required(app: &tauri::AppHandle, run_id: &str, request: ApprovalRequest) {
    let scope = run_id
        .parse::<uuid::Uuid>()
        .map(|id| clickweave_core::SafetyScope::AdHoc { run_id: id })
        .ok();
    let _ = app.emit(
        "agent://approval_required",
        serde_json::json!({
            "scope": scope,
            "run_id": run_id,
            "tool_name": request.tool_name,
            "arguments": request.arguments,
            "description": request.description,
        }),
    );
}

fn reject_queued_approval_requests(
    approval_rx: &mut tokio::sync::mpsc::Receiver<(
        ApprovalRequest,
        tokio::sync::oneshot::Sender<bool>,
    )>,
) {
    while let Ok((_req, resp_tx)) = approval_rx.try_recv() {
        let _ = resp_tx.send(false);
    }
}

pub(super) fn spawn_agent_cleanup(
    cleanup_handle: tauri::AppHandle,
    done_rx: tokio::sync::oneshot::Receiver<()>,
    events_done_rx: tokio::sync::oneshot::Receiver<()>,
    approval_done_rx: tokio::sync::oneshot::Receiver<()>,
) {
    tauri::async_runtime::spawn(async move {
        let _ = done_rx.await;
        let _ = events_done_rx.await;
        let _ = approval_done_rx.await;

        let handle = cleanup_handle.state::<Mutex<AgentHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.cancel_token = None;
        guard.task_handle = None;
        guard.pending_approval_tx = None;
        guard.pending_disagreement_tx = None;
        guard.run_id = None;
    });
}
