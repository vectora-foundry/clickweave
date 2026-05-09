use super::*;

#[tauri::command]
#[specta::specta]
pub async fn run_agent(
    app: tauri::AppHandle,
    request: AgentRunRequest,
) -> Result<(), CommandError> {
    ensure_agent_idle(&app)?;

    let mcp_binary_path =
        crate::mcp_resolve::resolve_mcp_binary().map_err(|e| CommandError::mcp(format!("{e}")))?;

    let project_id = parse_project_id(&request)?;

    let mut storage = resolve_storage(
        &app,
        &request.project_path,
        &request.project_name,
        project_id,
    );
    // Privacy kill switch: an explicit `false` from the UI disables
    // all on-disk writes for this run. The default is persist-on to
    // preserve existing behaviour when the UI does not send the flag.
    let persist_traces = request.store_traces.unwrap_or(true);
    storage.set_persistent(persist_traces);
    let storage = Arc::new(Mutex::new(storage));

    // Generate a per-run generation ID so event consumers can reject
    // stale events from a previous run that drain after stop/restart.
    // The frontend may supply its own run_id so the user message bubble
    // can be tagged before `agent://started` arrives — honor it when
    // present and syntactically valid.
    let (run_id, run_uuid) = resolve_run_id(&request)?;
    let anchor_uuid = parse_anchor_node_id(&request)?;
    let prior_turns = parse_prior_turns(&request)?;

    let consecutive_destructive_cap = request.consecutive_destructive_cap;
    let allow_focus_window = request.allow_focus_window;
    let episodic_settings_enabled = request.episodic_enabled.unwrap_or(true);
    let retrieved_episodes_k_override = request.retrieved_episodes_k;
    let episodic_global_participation = request.episodic_global_participation.unwrap_or(false);
    let skills_settings_enabled = request.skills_enabled.unwrap_or(true);
    let applicable_skills_k_override = request.applicable_skills_k;
    let skills_global_participation = request.skills_global_participation.unwrap_or(false);

    let episodic_ctx = build_episodic_context(
        &app,
        &storage,
        &request,
        persist_traces,
        episodic_settings_enabled,
        episodic_global_participation,
    )?;
    let skill_ctx = build_skill_context(
        &app,
        &storage,
        &request,
        persist_traces,
        skills_settings_enabled,
        skills_global_participation,
    )?;
    let agent_config = request.agent.into_llm_config(None);
    let permission_policy: Option<PermissionPolicy> = request.permissions.map(Into::into);

    // Capture the run-start timestamp so PromotePass scopes promotion
    // to episodes touched during this run.
    let run_start_utc = chrono::Utc::now();

    let cancel_token = CancellationToken::new();
    let agent_token = cancel_token.clone();
    let forwarder_token = cancel_token.clone();
    let event_forwarder_token = cancel_token.clone();

    // Live event channel: agent runner -> Tauri event emitter
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<RunnerOutput>(64);

    // Approval channel: agent runner sends requests, we forward to UI and store
    // the oneshot response sender in the handle for `approve_agent_action` to use.
    let (approval_tx, approval_rx) =
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
    let terminal_event_tx = event_tx.clone();
    let approval_run_id = run_id.clone();
    let task_episodic_ctx = episodic_ctx.clone();
    let task_skill_ctx = skill_ctx.clone();
    let proposal_skill_ctx = skill_ctx.clone();
    let proposal_agent_config = agent_config.clone();
    let promotion_episodic_ctx = episodic_ctx.clone();
    let promotion_project_id = episodic_ctx.project_id.clone();

    // Channels used to signal cleanup when the agent task, event forwarder,
    // and approval forwarder have all finished, preventing stale event leakage.
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let (events_done_tx, events_done_rx) = tokio::sync::oneshot::channel::<()>();
    let (approval_done_tx, approval_done_rx) = tokio::sync::oneshot::channel::<()>();

    // Install cancel_token and run_id before spawning so stop_agent() works
    // even during the spawn window (before task_handle is available).
    install_agent_run_handle(&app, cancel_token, &run_id);

    // Emit agent://started so the frontend knows the run_id before any other events.
    let _ = app.emit("agent://started", serde_json::json!({ "run_id": &run_id }));

    let task_handle = spawn_agent_run_task(AgentRunTaskInput {
        mcp_binary_path,
        agent_token,
        terminal_event_tx,
        emit_handle,
        task_run_id,
        done_tx,
        agent_config: agent_config.clone(),
        consecutive_destructive_cap,
        allow_focus_window,
        episodic_settings_enabled,
        retrieved_episodes_k_override,
        skills_settings_enabled,
        applicable_skills_k_override,
        skills_global_participation,
        storage: task_storage,
        event_tx: event_tx.clone(),
        approval_tx,
        goal,
        prior_turns,
        permission_policy,
        run_uuid,
        anchor_uuid,
        episodic_ctx: task_episodic_ctx,
        skill_ctx: task_skill_ctx,
        persist_traces,
        promotion_episodic_ctx,
        promotion_project_id,
        run_start_utc,
    });

    spawn_agent_event_forwarder(
        event_forwarder_token,
        event_rx,
        event_storage,
        event_emit_handle,
        event_run_id,
        proposal_skill_ctx,
        proposal_agent_config,
        events_done_tx,
    );
    spawn_approval_forwarder(
        approval_rx,
        forwarder_token,
        approval_emit_handle,
        approval_run_id,
        approval_done_tx,
    );
    store_agent_task_handle(&app, task_handle);
    spawn_agent_cleanup(cleanup_handle, done_rx, events_done_rx, approval_done_rx);

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

/// Wire form for `resolve_completion_disagreement`. Mirrors
/// `DisagreementResolutionAction` but derives `specta::Type` so the
/// TypeScript binding picks it up.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum CompletionDisagreementActionWire {
    Confirm,
    Cancel,
}

impl From<CompletionDisagreementActionWire> for DisagreementResolutionAction {
    fn from(a: CompletionDisagreementActionWire) -> Self {
        match a {
            CompletionDisagreementActionWire::Confirm => DisagreementResolutionAction::Confirm,
            CompletionDisagreementActionWire::Cancel => DisagreementResolutionAction::Cancel,
        }
    }
}

/// Resolve a pending VLM completion disagreement. The operator picks
/// either `confirm` (override the VLM, mark the run complete) or
/// `cancel` (agree with the VLM, halt the run). The backend records the
/// decision to `events.jsonl` + `variant_index.jsonl` and emits the
/// appropriate terminal Tauri event.
///
/// Concurrency note: the AgentHandle lock is held across the oneshot
/// send on purpose. `force_stop` (the Stop button) also locks the
/// AgentHandle, cancels the run's CancellationToken, and takes the
/// disagreement sender from the same slot. If this command released
/// the lock after `.take()` but before `.send()`, a concurrent
/// `force_stop` could trip the cancel token in the gap and the
/// `tokio::select!` in `await_disagreement_resolution` would pick the
/// cancel branch before the confirm ever arrived — silently losing
/// the operator's decision. `oneshot::Sender::send` is synchronous
/// and infallible except for a dropped receiver, so holding the
/// `std::sync::Mutex` across it is cheap and race-closing.
#[tauri::command]
#[specta::specta]
pub async fn resolve_completion_disagreement(
    app: tauri::AppHandle,
    action: CompletionDisagreementActionWire,
) -> Result<(), CommandError> {
    let handle = app.state::<Mutex<AgentHandle>>();
    let mut guard = handle.lock().unwrap();
    let tx = guard
        .pending_disagreement_tx
        .take()
        .ok_or(CommandError::validation(
            "No pending completion disagreement",
        ))?;
    tx.send(action.into()).map_err(|_| {
        CommandError::validation("Disagreement channel closed — agent task may have ended")
    })
}
