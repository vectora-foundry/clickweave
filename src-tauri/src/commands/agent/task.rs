use super::*;

pub(super) struct AgentRunTaskInput {
    pub(super) mcp_binary_path: String,
    pub(super) agent_token: CancellationToken,
    pub(super) terminal_event_tx: tokio::sync::mpsc::Sender<RunnerOutput>,
    pub(super) emit_handle: tauri::AppHandle,
    pub(super) task_run_id: String,
    pub(super) done_tx: tokio::sync::oneshot::Sender<()>,
    pub(super) agent_config: clickweave_llm::LlmConfig,
    pub(super) consecutive_destructive_cap: Option<usize>,
    pub(super) allow_focus_window: Option<bool>,
    pub(super) episodic_settings_enabled: bool,
    pub(super) retrieved_episodes_k_override: Option<usize>,
    pub(super) skills_settings_enabled: bool,
    pub(super) applicable_skills_k_override: Option<usize>,
    pub(super) skills_global_participation: bool,
    pub(super) storage: Arc<Mutex<clickweave_core::storage::RunStorage>>,
    pub(super) event_tx: tokio::sync::mpsc::Sender<RunnerOutput>,
    pub(super) approval_tx:
        tokio::sync::mpsc::Sender<(ApprovalRequest, tokio::sync::oneshot::Sender<bool>)>,
    pub(super) goal: String,
    pub(super) prior_turns: Vec<clickweave_engine::agent::PriorTurn>,
    pub(super) permission_policy: Option<PermissionPolicy>,
    pub(super) run_uuid: uuid::Uuid,
    pub(super) anchor_uuid: Option<uuid::Uuid>,
    pub(super) episodic_ctx: EpisodicContext,
    pub(super) skill_ctx: SkillContext,
    pub(super) persist_traces: bool,
    pub(super) promotion_episodic_ctx: EpisodicContext,
    pub(super) promotion_project_id: String,
    pub(super) run_start_utc: chrono::DateTime<chrono::Utc>,
}

pub(super) fn spawn_agent_run_task(
    input: AgentRunTaskInput,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        run_agent_task(input).await;
    })
}

async fn run_agent_task(input: AgentRunTaskInput) {
    let AgentRunTaskInput {
        mcp_binary_path,
        agent_token,
        terminal_event_tx,
        emit_handle,
        task_run_id,
        done_tx,
        agent_config,
        consecutive_destructive_cap,
        allow_focus_window,
        episodic_settings_enabled,
        retrieved_episodes_k_override,
        skills_settings_enabled,
        applicable_skills_k_override,
        skills_global_participation,
        storage,
        event_tx,
        approval_tx,
        goal,
        prior_turns,
        permission_policy,
        run_uuid,
        anchor_uuid,
        episodic_ctx,
        skill_ctx,
        persist_traces,
        promotion_episodic_ctx,
        promotion_project_id,
        run_start_utc,
    } = input;

    let Some(mcp) = spawn_mcp_for_agent(
        &mcp_binary_path,
        &agent_token,
        &terminal_event_tx,
        &emit_handle,
        &task_run_id,
    )
    .await
    else {
        let _ = done_tx.send(());
        return;
    };

    let llm = clickweave_llm::LlmClient::new(agent_config.clone().with_thinking(false));
    let vision: Arc<dyn clickweave_llm::DynChatBackend> = Arc::new(clickweave_llm::LlmClient::new(
        agent_config.with_thinking(false).with_max_tokens(512),
    ));
    let config = agent_config_from_request(
        consecutive_destructive_cap,
        allow_focus_window,
        episodic_settings_enabled,
        retrieved_episodes_k_override,
        skills_settings_enabled,
        applicable_skills_k_override,
        skills_global_participation,
    );

    let (variant_context, verification_artifacts_dir) = match initialize_agent_storage(&storage) {
        Ok(v) => v,
        Err(message) => {
            emit_agent_task_error(&terminal_event_tx, &emit_handle, &task_run_id, message).await;
            let _ = done_tx.send(());
            return;
        }
    };

    let goal_block = clickweave_engine::agent::build_goal_block(
        &goal,
        &prior_turns,
        if variant_context.is_empty() {
            None
        } else {
            Some(variant_context.as_str())
        },
        1000,
    );
    let channels = AgentChannels {
        event_tx: event_tx.clone(),
        approval_tx,
    };

    let result = tokio::select! {
        res = clickweave_engine::agent::run_agent_workflow(
            &llm,
            config,
            goal_block,
            &mcp,
            Some(channels),
            Some(vision.clone()),
            permission_policy,
            run_uuid,
            anchor_uuid,
            verification_artifacts_dir,
            Some(storage.clone()),
            Some(episodic_ctx.clone()),
            Some(skill_ctx.clone()),
        ) => res,
        _ = agent_token.cancelled() => {
            emit_after_agent_event_drain(
                &terminal_event_tx,
                &emit_handle,
                "agent://stopped",
                serde_json::json!({ "run_id": task_run_id, "reason": "cancelled" }),
            )
            .await;
            let _ = done_tx.send(());
            return;
        }
    };

    match result {
        Ok((state, writer_tx)) => {
            handle_agent_success(
                state,
                writer_tx,
                &emit_handle,
                &agent_token,
                &storage,
                &task_run_id,
                &terminal_event_tx,
                persist_traces,
                &promotion_episodic_ctx,
                &promotion_project_id,
                run_start_utc,
            )
            .await;
        }
        Err(e) => {
            emit_agent_task_error(
                &terminal_event_tx,
                &emit_handle,
                &task_run_id,
                format!("{e}"),
            )
            .await;
        }
    }

    let _ = done_tx.send(());
}

async fn spawn_mcp_for_agent(
    mcp_binary_path: &str,
    agent_token: &CancellationToken,
    terminal_event_tx: &tokio::sync::mpsc::Sender<RunnerOutput>,
    emit_handle: &tauri::AppHandle,
    task_run_id: &str,
) -> Option<clickweave_mcp::McpClient> {
    tokio::select! {
        res = clickweave_mcp::McpClient::spawn(mcp_binary_path, &[]) => {
            match res {
                Ok(m) => Some(m),
                Err(e) => {
                    emit_agent_task_error(
                        terminal_event_tx,
                        emit_handle,
                        task_run_id,
                        format!("MCP spawn failed: {e}"),
                    )
                    .await;
                    None
                }
            }
        }
        _ = agent_token.cancelled() => {
            emit_after_agent_event_drain(
                terminal_event_tx,
                emit_handle,
                "agent://stopped",
                serde_json::json!({ "run_id": task_run_id, "reason": "cancelled" }),
            )
            .await;
            None
        }
    }
}

fn initialize_agent_storage(
    storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
) -> Result<(String, Option<std::path::PathBuf>), String> {
    let mut guard = storage.lock().unwrap();
    if let Err(e) = guard.begin_execution() {
        return Err(format!("Run storage init failed: {e}"));
    }
    let variant_index = VariantIndex::load_existing(&guard.variant_index_path(), guard.base_path());
    Ok((
        variant_index.as_context_text(),
        guard.execution_artifacts_dir(),
    ))
}

async fn emit_agent_task_error(
    terminal_event_tx: &tokio::sync::mpsc::Sender<RunnerOutput>,
    emit_handle: &tauri::AppHandle,
    task_run_id: &str,
    message: String,
) {
    emit_after_agent_event_drain(
        terminal_event_tx,
        emit_handle,
        "agent://error",
        serde_json::json!({ "run_id": task_run_id, "message": message }),
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn handle_agent_success(
    state: AgentState,
    writer_tx: Option<
        tokio::sync::mpsc::Sender<clickweave_engine::agent::episodic::types::WriteRequest>,
    >,
    emit_handle: &tauri::AppHandle,
    agent_token: &CancellationToken,
    storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    task_run_id: &str,
    terminal_event_tx: &tokio::sync::mpsc::Sender<RunnerOutput>,
    persist_traces: bool,
    promotion_episodic_ctx: &EpisodicContext,
    promotion_project_id: &str,
    run_start_utc: chrono::DateTime<chrono::Utc>,
) {
    let resolved_terminal = resolve_terminal_reason(
        state.terminal_reason,
        emit_handle,
        agent_token,
        storage,
        task_run_id,
    )
    .await;
    append_variant_entry(storage, persist_traces, &resolved_terminal);
    emit_resolved_terminal(
        terminal_event_tx,
        emit_handle,
        task_run_id,
        &resolved_terminal,
    )
    .await;
    queue_terminal_promotion(
        writer_tx,
        emit_handle,
        task_run_id,
        promotion_episodic_ctx,
        promotion_project_id,
        run_start_utc,
        &resolved_terminal,
    )
    .await;
}

async fn resolve_terminal_reason(
    terminal_reason: Option<TerminalReason>,
    emit_handle: &tauri::AppHandle,
    agent_token: &CancellationToken,
    storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    task_run_id: &str,
) -> Option<TerminalReason> {
    match terminal_reason {
        Some(TerminalReason::CompletionDisagreement {
            agent_summary,
            vlm_reasoning,
        }) => {
            await_disagreement_resolution(
                emit_handle,
                agent_token,
                storage,
                task_run_id,
                agent_summary,
                vlm_reasoning,
            )
            .await
        }
        other => other,
    }
}

fn append_variant_entry(
    storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    persist_traces: bool,
    resolved_terminal: &Option<TerminalReason>,
) {
    if !persist_traces {
        return;
    }
    let (divergence_summary, success) = match resolved_terminal {
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
}

async fn emit_resolved_terminal(
    terminal_event_tx: &tokio::sync::mpsc::Sender<RunnerOutput>,
    emit_handle: &tauri::AppHandle,
    task_run_id: &str,
    resolved_terminal: &Option<TerminalReason>,
) {
    match resolved_terminal {
        Some(TerminalReason::Completed { summary })
        | Some(TerminalReason::DisagreementConfirmed {
            agent_summary: summary,
        }) => {
            emit_after_agent_event_drain(
                terminal_event_tx,
                emit_handle,
                "agent://complete",
                serde_json::json!({ "run_id": task_run_id, "summary": summary }),
            )
            .await;
        }
        Some(TerminalReason::DisagreementCancelled { .. }) => {
            emit_after_agent_event_drain(
                terminal_event_tx,
                emit_handle,
                "agent://stopped",
                serde_json::json!({
                    "run_id": task_run_id,
                    "reason": "user_cancelled_disagreement",
                }),
            )
            .await;
        }
        Some(reason) => {
            let mut payload = serde_json::to_value(reason).unwrap_or_default();
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("run_id".to_string(), serde_json::json!(task_run_id));
            }
            emit_after_agent_event_drain(
                terminal_event_tx,
                emit_handle,
                "agent://stopped",
                payload,
            )
            .await;
        }
        None => {
            emit_after_agent_event_drain(
                terminal_event_tx,
                emit_handle,
                "agent://stopped",
                serde_json::json!({ "run_id": task_run_id, "reason": "cancelled" }),
            )
            .await;
        }
    }
}

async fn queue_terminal_promotion(
    writer_tx: Option<
        tokio::sync::mpsc::Sender<clickweave_engine::agent::episodic::types::WriteRequest>,
    >,
    emit_handle: &tauri::AppHandle,
    task_run_id: &str,
    promotion_episodic_ctx: &EpisodicContext,
    promotion_project_id: &str,
    run_start_utc: chrono::DateTime<chrono::Utc>,
    resolved_terminal: &Option<TerminalReason>,
) {
    if !promotion_episodic_ctx.enabled || promotion_episodic_ctx.global_path.is_none() {
        return;
    }
    let Some(tx) = writer_tx else {
        return;
    };
    use clickweave_engine::agent::episodic::{
        PromotionTerminalKind, types::WriteRequest as EpisodicWriteRequest,
    };
    let terminal_kind = match resolved_terminal {
        Some(TerminalReason::Completed { .. }) => PromotionTerminalKind::Clean,
        _ => PromotionTerminalKind::SkipPromotion,
    };
    if let Err(e) = tx.try_send(EpisodicWriteRequest::PromotePass {
        workflow_hash: promotion_project_id.to_string(),
        terminal_kind,
        run_started_at: run_start_utc,
    }) {
        tracing::warn!(error = %e, "episodic: PromotePass dropped at terminal");
        let _ = emit_handle.emit(
            "agent://warning",
            serde_json::json!({
                "run_id": task_run_id,
                "message": format!("episodic: promotion dropped: backpressure ({e})"),
            }),
        );
    }
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel::<()>();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        if tx
            .send(EpisodicWriteRequest::Flush { ack: ack_tx })
            .await
            .is_err()
        {
            return;
        }
        let _ = ack_rx.await;
    })
    .await;
}

// ── Commands ────────────────────────────────────────────────────
