use super::*;

pub async fn run_eval<B, J>(
    scenario: EvalScenario,
    agent: B,
    agent_system_prompt: Option<String>,
    judge: Option<&J>,
) -> Result<EvalReport>
where
    B: ChatBackend,
    J: ChatBackend,
{
    scenario.validate_privacy()?;
    if let Some(prompt) = agent_system_prompt.as_deref()
        && personal_marker(prompt).is_some()
    {
        bail!("agent prompt candidate appears to contain private material");
    }
    let default_prompt = include_str!("../../clickweave-engine/prompts/agent_system.md");
    let prompt_sha = prompt_sha(agent_system_prompt.as_deref().unwrap_or(default_prompt));
    let mcp = ScenarioMcp::new(&scenario);
    let recording_agent = if scenario.scoring.stop_after_agent_tools.is_empty() {
        RecordingBackend::new(agent)
    } else {
        RecordingBackend::with_stop_after_agent_tools(
            agent,
            &scenario.scoring.stop_after_agent_tools,
        )
    };
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<RunnerOutput>(256);
    let (approval_tx, _approval_rx) = tokio::sync::mpsc::channel(1);

    let mut config = AgentConfig {
        max_steps: scenario.max_steps,
        ..AgentConfig::default()
    };
    config.allow_focus_window = false;

    let run = run_agent_workflow_with_prompt_override(
        &recording_agent,
        config,
        scenario.goal.clone(),
        &mcp,
        Some(AgentChannels {
            event_tx,
            approval_tx,
        }),
        None,
        Some(PermissionPolicy {
            allow_all: true,
            ..PermissionPolicy::default()
        }),
        uuid::Uuid::new_v4(),
        None,
        None,
        None,
        None,
        None,
        agent_system_prompt,
    )
    .await;
    let eval_halt = recording_agent.eval_halt();
    let (completed, state_steps, run_error) = match run {
        Ok((state, _writer)) => (state.completed, state.steps.len(), None),
        Err(err)
            if eval_halt.is_some() && err.chain().any(|cause| cause.is::<EvalHaltTriggered>()) =>
        {
            (false, 0, None)
        }
        Err(err) => (false, 0, Some(redact_text(&err.to_string()))),
    };

    let mut events = Vec::new();
    while let Ok(output) = event_rx.try_recv() {
        if let Some(event) = output.into_event() {
            events.push(redact_value(serde_json::to_value(event)?));
        }
    }

    let llm_trace = recording_agent.traces();
    let tool_trace = mcp.traces();
    let steps = if eval_halt.is_some() {
        count_step_events(&events)
    } else {
        state_steps
    };
    let deterministic = score_deterministic(
        &scenario,
        completed,
        steps,
        &tool_trace,
        &llm_trace,
        &events,
    );
    let semantic_judge = if let Some(judge_backend) = judge {
        Some(
            run_semantic_judge(
                judge_backend,
                &scenario,
                &prompt_sha,
                &deterministic,
                &tool_trace,
                &llm_trace,
                run_error.as_deref(),
            )
            .await?,
        )
    } else {
        None
    };
    let judge_score = semantic_judge.as_ref().map(|j| j.score).unwrap_or(0.0);
    let final_score = if semantic_judge.is_some() {
        deterministic.score * 0.8 + judge_score * 0.2
    } else {
        deterministic.score
    };

    Ok(EvalReport {
        scenario_id: scenario.id,
        prompt_sha,
        deterministic,
        semantic_judge,
        final_score,
        tool_trace,
        llm_trace,
        events,
        eval_halt,
        privacy: PrivacyReport {
            synthetic_fixture_only: true,
            screenshots_omitted: true,
            secrets_redacted: true,
            local_paths_redacted: true,
        },
        run_error,
    })
}
