use super::*;
use clickweave_engine::agent::skills::extractor::synthesize_skill_id_for_signature;
use clickweave_engine::agent::skills::replay::ReplayJson;
use clickweave_engine::agent::skills::signature::{
    compute_applicability_signature_from_parts, compute_subgoal_signature_from_parts,
};
use clickweave_engine::agent::skills::{
    ActionSketchStep, ApplicabilityHints, ExpectedWorldModelDelta, OutcomePredicate,
    ProvenanceEntry, Skill, SkillError, SkillScope, SkillState, SkillStats, SkillStore,
    parse_skill_md, prose_generator,
};
use std::collections::HashMap;

/// One agent step sent from the frontend for skill materialisation.
/// `args_json` is the JSON-serialised tool arguments; empty string is
/// treated the same as `{}`.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct AgentStepWire {
    pub summary: String,
    pub tool_name: String,
    pub args_json: String,
}

/// Convert a slice of `AgentStepWire` items into `ActionSketchStep`s.
/// Each non-empty step becomes a `ToolCall` variant. Steps with an empty
/// `tool_name` are skipped (they represent no-tool assistant messages).
fn steps_wire_to_sketch(steps: &[AgentStepWire]) -> Vec<ActionSketchStep> {
    steps
        .iter()
        .filter(|s| !s.tool_name.is_empty())
        .map(|s| {
            let args = if s.args_json.is_empty() {
                serde_json::Value::Object(Default::default())
            } else {
                serde_json::from_str(&s.args_json)
                    .unwrap_or(serde_json::Value::Object(Default::default()))
            };
            ActionSketchStep::ToolCall {
                step_id: uuid::Uuid::new_v4().to_string(),
                tool: s.tool_name.clone(),
                args,
                captures_pre: vec![],
                captures: vec![],
                expected_world_model_delta: ExpectedWorldModelDelta::default(),
                requires_approval: None,
            }
        })
        .collect()
}

/// Build a new `Skill` from an agent run's tool calls.
fn build_skill_from_agent_steps(
    sketch: Vec<ActionSketchStep>,
    body: String,
    name: &str,
    description: &str,
    project_id: &str,
) -> Skill {
    let now = chrono::Utc::now();
    let subgoal_signature = compute_subgoal_signature_from_parts(name, "", "");
    let id = synthesize_skill_id_for_signature(name, &subgoal_signature);
    let applicability = ApplicabilityHints {
        apps: vec![],
        hosts: vec![],
        signature: compute_applicability_signature_from_parts("", ""),
    };
    Skill {
        id,
        version: 1,
        state: SkillState::Draft,
        scope: SkillScope::ProjectLocal,
        name: name.to_string(),
        description: description.to_string(),
        tags: vec!["agent-run".to_string()],
        subgoal_text: name.to_string(),
        subgoal_signature,
        applicability,
        parameter_schema: vec![],
        action_sketch: sketch,
        outputs: vec![],
        outcome_predicate: OutcomePredicate::SubgoalCompleted {
            post_state_world_model_signature: None,
        },
        provenance: vec![ProvenanceEntry {
            run_id: format!("agent:{project_id}"),
            step_index: 0,
            completed_at: now,
            workflow_hash: project_id.to_string(),
        }],
        stats: SkillStats {
            occurrence_count: 1,
            success_rate: 1.0,
            last_seen_at: Some(now),
            last_invoked_at: None,
        },
        edited_by_user: false,
        created_at: now,
        updated_at: now,
        produced_node_ids: vec![],
        body,
        schema_version: clickweave_engine::agent::skills::SKILL_SCHEMA_VERSION,
        variables: vec![],
        sections: vec![],
        replay: None,
    }
}

fn agent_skill_error_to_command(error: SkillError) -> CommandError {
    match error {
        SkillError::Io(e) => CommandError::io(format!("{e}")),
        SkillError::InvalidParameters(message) => CommandError::validation(message),
        other => CommandError::validation(format!("{other}")),
    }
}

fn write_skill_files(store: &SkillStore, skill: &Skill) -> Result<(), CommandError> {
    store
        .write_skill(skill)
        .map_err(agent_skill_error_to_command)?;
    let _ = store.write_replay(
        &skill.id,
        &ReplayJson {
            skill_id: skill.id.clone(),
            schema_version: clickweave_engine::agent::skills::SKILL_SCHEMA_VERSION,
            steps: HashMap::new(),
            section_history: vec![],
        },
    );
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct SaveRunAsSkillRequest {
    pub project_path: Option<String>,
    pub project_name: String,
    pub project_id: String,
    pub name: String,
    pub goal: String,
    pub steps: Vec<AgentStepWire>,
    pub store_traces: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct AddRunToSkillRequest {
    pub project_path: Option<String>,
    pub project_name: String,
    pub project_id: String,
    pub skill_id: String,
    pub version: u32,
    pub goal: String,
    pub steps: Vec<AgentStepWire>,
    pub store_traces: bool,
}

/// Save the current agent run as a new skill. The run's tool calls are
/// converted to `ActionSketchStep[]`, prose is generated, and the skill
/// is written to the project's skill store.
#[tauri::command]
#[specta::specta]
pub async fn save_run_as_skill(
    app: tauri::AppHandle,
    request: SaveRunAsSkillRequest,
) -> Result<Skill, CommandError> {
    if !request.store_traces {
        return Err(CommandError::validation(
            "Skill file access is disabled while trace persistence is off",
        ));
    }

    let project_id = parse_uuid(&request.project_id, "project")?;

    let name = request.name.trim();
    let name = if name.is_empty() {
        request.goal.trim()
    } else {
        name
    };
    let name = if name.is_empty() {
        "Agent Run Skill"
    } else {
        name
    };

    let sketch = steps_wire_to_sketch(&request.steps);
    let body = prose_generator::generate(&sketch, name);

    let today = chrono::Utc::now().format("%Y-%m-%d");
    let description = format!("Agent run captured {today}: {}", request.goal.trim());

    let skill = build_skill_from_agent_steps(sketch, body, name, &description, &request.project_id);

    let storage = resolve_storage(
        &app,
        &request.project_path,
        &request.project_name,
        project_id,
    );
    let skills_dir = storage
        .project_skills_dir()
        .map_err(|e| CommandError::io(format!("resolve project skills dir: {e}")))?;

    let store = SkillStore::new(skills_dir);
    write_skill_files(&store, &skill)?;

    let _ = app.emit(
        "agent://skill_extracted",
        serde_json::json!({
            "skill_id": skill.id.clone(),
            "version": skill.version,
            "state": skill.state,
            "scope": skill.scope,
        }),
    );

    Ok(skill)
}

/// Append the current agent run's steps to an existing skill as a new
/// section. Reads the existing SKILL.md, appends the new steps, and
/// writes the updated file at an incremented version.
#[tauri::command]
#[specta::specta]
pub async fn add_run_to_skill(
    app: tauri::AppHandle,
    request: AddRunToSkillRequest,
) -> Result<Skill, CommandError> {
    if !request.store_traces {
        return Err(CommandError::validation(
            "Skill file access is disabled while trace persistence is off",
        ));
    }

    let project_id = parse_uuid(&request.project_id, "project")?;

    let storage = resolve_storage(
        &app,
        &request.project_path,
        &request.project_name,
        project_id,
    );
    let skills_dir = storage
        .project_skills_dir()
        .map_err(|e| CommandError::io(format!("resolve project skills dir: {e}")))?;

    let store = SkillStore::new(skills_dir.clone());

    // Load the existing skill from `<skill_id>/SKILL.md`.
    let skill_path = store.skill_md_path(&request.skill_id);
    let current_md = std::fs::read_to_string(&skill_path)
        .map_err(|e| CommandError::io(format!("read SKILL.md: {e}")))?;
    let mut skill =
        parse_skill_md(&current_md).map_err(|e| CommandError::validation(e.to_string()))?;

    // Append the new steps to the existing action_sketch.
    let new_steps = steps_wire_to_sketch(&request.steps);
    skill.action_sketch.extend(new_steps);

    // Regenerate prose for the full updated sketch.
    skill.body = prose_generator::generate(&skill.action_sketch, &skill.name);

    // Bump the version stamped in the frontmatter; the on-disk file
    // path stays the same `<skill_id>/SKILL.md`.
    skill.version += 1;

    // Persist the new revision over the canonical SKILL.md.
    store
        .write_skill(&skill)
        .map_err(|e| CommandError::io(format!("write SKILL.md: {e}")))?;

    // Update replay.json skeleton.
    let _ = store.write_replay(
        &skill.id,
        &ReplayJson {
            skill_id: skill.id.clone(),
            schema_version: clickweave_engine::agent::skills::SKILL_SCHEMA_VERSION,
            steps: HashMap::new(),
            section_history: vec![],
        },
    );

    let _ = app.emit(
        "agent://skill_extracted",
        serde_json::json!({
            "skill_id": skill.id.clone(),
            "version": skill.version,
            "state": skill.state,
            "scope": skill.scope,
        }),
    );

    Ok(skill)
}

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
