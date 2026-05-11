use super::*;

pub(super) fn ensure_agent_idle(app: &tauri::AppHandle) -> Result<(), CommandError> {
    let handle = app.state::<Mutex<AgentHandle>>();
    let guard = handle.lock().unwrap();
    if guard.cancel_token.is_some() || guard.task_handle.is_some() {
        return Err(CommandError::already_running());
    }
    Ok(())
}

pub(super) fn parse_project_id(request: &AgentRunRequest) -> Result<uuid::Uuid, CommandError> {
    request
        .project_id
        .parse()
        .map_err(|_| CommandError::validation("Invalid project ID"))
}

pub(super) fn resolve_run_id(
    request: &AgentRunRequest,
) -> Result<(String, uuid::Uuid), CommandError> {
    let run_id = request
        .run_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let run_uuid = run_id
        .parse()
        .map_err(|_| CommandError::validation("Invalid run_id"))?;
    Ok((run_id, run_uuid))
}

pub(super) fn parse_anchor_node_id(
    request: &AgentRunRequest,
) -> Result<Option<uuid::Uuid>, CommandError> {
    match request.anchor_node_id.as_deref() {
        Some(s) if !s.is_empty() => s
            .parse()
            .map(Some)
            .map_err(|_| CommandError::validation("Invalid anchor_node_id")),
        _ => Ok(None),
    }
}

pub(super) fn parse_prior_turns(
    request: &AgentRunRequest,
) -> Result<Vec<clickweave_engine::agent::PriorTurn>, CommandError> {
    request
        .prior_turns
        .iter()
        .map(|t| {
            let run_id: uuid::Uuid = t
                .run_id
                .parse()
                .map_err(|_| CommandError::validation("Invalid prior_turn.run_id"))?;
            Ok(clickweave_engine::agent::PriorTurn {
                goal: t.goal.clone(),
                summary: t.summary.clone(),
                run_id,
            })
        })
        .collect()
}

pub(super) fn build_episodic_context(
    app: &tauri::AppHandle,
    storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    request: &AgentRunRequest,
    persist_traces: bool,
    enabled: bool,
    global_participation: bool,
) -> Result<EpisodicContext, CommandError> {
    if !persist_traces || !enabled {
        return Ok(EpisodicContext::disabled());
    }

    let wl_path = storage.lock().unwrap().base_path().join("episodic.sqlite");
    let global_path = if global_participation {
        Some(app_data_episodic_path(app)?)
    } else {
        None
    };
    Ok(EpisodicContext {
        enabled: true,
        workflow_local_path: wl_path,
        global_path,
        project_id: request.project_id.clone(),
    })
}

pub(super) fn build_skill_context(
    app: &tauri::AppHandle,
    storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    request: &AgentRunRequest,
    persist_traces: bool,
    enabled: bool,
    global_participation: bool,
) -> Result<SkillContext, CommandError> {
    let project_skills_dir = {
        let guard = storage.lock().unwrap();
        if persist_traces && enabled {
            guard
                .project_skills_dir()
                .map_err(|e| CommandError::io(format!("resolve project skills dir: {e}")))?
        } else {
            guard.base_path().join("skills")
        }
    };
    let global_skills_dir = if persist_traces && enabled && global_participation {
        Some(app_data_global_skills_dir(app)?)
    } else {
        None
    };
    Ok(SkillContext {
        enabled: persist_traces && enabled,
        project_skills_dir,
        global_skills_dir,
        project_id: request.project_id.clone(),
    })
}

pub(super) fn agent_config_from_request(
    consecutive_destructive_cap: Option<usize>,
    allow_focus_window: Option<bool>,
    episodic_settings_enabled: bool,
    retrieved_episodes_k_override: Option<usize>,
    skills_settings_enabled: bool,
    applicable_skills_k_override: Option<usize>,
    skills_global_participation: bool,
) -> AgentConfig {
    let mut config = AgentConfig::default();
    if let Some(cap) = consecutive_destructive_cap {
        config.consecutive_destructive_cap = cap;
    }
    if let Some(allow) = allow_focus_window {
        config.allow_focus_window = allow;
    }
    config.episodic_enabled = episodic_settings_enabled;
    if let Some(k) = retrieved_episodes_k_override {
        config.retrieved_episodes_k = k.clamp(1, 10);
    }
    config.skills_enabled = skills_settings_enabled;
    if let Some(k) = applicable_skills_k_override {
        config.applicable_skills_k = k.clamp(1, 10);
    }
    config.skills_global_participation = skills_global_participation;
    config
}

pub(super) fn install_agent_run_handle(
    app: &tauri::AppHandle,
    cancel_token: CancellationToken,
    run_id: &str,
) {
    let handle = app.state::<Mutex<AgentHandle>>();
    let mut guard = handle.lock().unwrap();
    guard.cancel_token = Some(cancel_token);
    guard.run_id = Some(run_id.to_string());
}

pub(super) fn store_agent_task_handle(
    app: &tauri::AppHandle,
    task_handle: tauri::async_runtime::JoinHandle<()>,
) {
    let handle = app.state::<Mutex<AgentHandle>>();
    let mut guard = handle.lock().unwrap();
    guard.task_handle = Some(task_handle);
}
