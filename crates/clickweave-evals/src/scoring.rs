use super::*;

pub(crate) fn count_step_events(events: &[Value]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event.get("type").and_then(Value::as_str),
                Some("step_completed" | "step_failed")
            )
        })
        .count()
}

pub(crate) fn score_deterministic(
    scenario: &EvalScenario,
    completed: bool,
    steps: usize,
    tool_trace: &[ToolTrace],
    llm_trace: &[LlmTurnTrace],
    events: &[Value],
) -> DeterministicScore {
    let seen: HashSet<&str> = tool_trace.iter().map(|call| call.tool.as_str()).collect();
    let agent_tool_names = collect_agent_tool_names(llm_trace);
    let agent_seen: HashSet<&str> = agent_tool_names.iter().copied().collect();
    let required_tools_missing = missing_required_names(&scenario.scoring.required_tools, &seen);
    let required_agent_tools_missing =
        missing_required_names(&scenario.scoring.required_agent_tools, &agent_seen);
    let required_agent_tool_groups_missing = missing_required_agent_tool_groups(
        &scenario.scoring.required_agent_tool_groups,
        &agent_seen,
    );
    let agent_counts = count_agent_tool_names(&agent_tool_names);
    let required_agent_tool_counts_missing = missing_required_agent_tool_counts(
        &scenario.scoring.required_agent_tool_counts,
        &agent_counts,
    );
    let forbidden: HashSet<&str> = scenario
        .scoring
        .forbidden_tools
        .iter()
        .map(String::as_str)
        .collect();
    let forbidden_tool_calls = count_forbidden_tool_calls(tool_trace, &forbidden);
    let forbidden_agent: HashSet<&str> = scenario
        .scoring
        .forbidden_agent_tools
        .iter()
        .map(String::as_str)
        .collect();
    let forbidden_agent_tool_calls =
        count_forbidden_agent_tool_calls(&agent_tool_names, &forbidden_agent);
    let allowed_error_tools: HashSet<&str> = scenario
        .scoring
        .allowed_error_tools
        .iter()
        .map(String::as_str)
        .collect();
    let invalid_tool_errors = count_invalid_tool_errors(tool_trace, &allowed_error_tools);
    let repeated_action_warnings = count_repeated_action_warnings(events);
    let agent_tool_calls = agent_tool_names.len();
    let max_agent_tool_calls_excess =
        excess_over_limit(agent_tool_calls, scenario.scoring.max_agent_tool_calls);
    let max_repeated_action_warnings_excess = excess_over_limit(
        repeated_action_warnings,
        scenario.scoring.max_repeated_action_warnings,
    );

    let mut score = 1.0_f32;
    if scenario.scoring.completion_required && !completed {
        score -= 0.35;
    }
    score -= required_tools_missing.len() as f32 * 0.12;
    score -= required_agent_tools_missing.len() as f32 * 0.12;
    score -= required_agent_tool_groups_missing.len() as f32 * 0.12;
    score -= required_agent_tool_counts_missing.len() as f32 * 0.12;
    score -= forbidden_tool_calls as f32 * 0.15;
    score -= forbidden_agent_tool_calls as f32 * 0.20;
    score -= invalid_tool_errors as f32 * 0.12;
    score -= repeated_action_warnings as f32 * 0.05;
    score -= max_agent_tool_calls_excess as f32 * 0.04;
    score -= max_repeated_action_warnings_excess as f32;
    if scenario.max_steps > 0 {
        score -= (steps as f32 / scenario.max_steps as f32).min(1.0) * 0.08;
    }

    DeterministicScore {
        score: score.clamp(0.0, 1.0),
        completed,
        steps,
        required_tools_missing,
        required_agent_tools_missing,
        required_agent_tool_groups_missing,
        required_agent_tool_counts_missing,
        forbidden_tool_calls,
        forbidden_agent_tool_calls,
        invalid_tool_errors,
        repeated_action_warnings,
        agent_tool_calls,
        max_agent_tool_calls_excess,
        max_repeated_action_warnings_excess,
    }
}

fn collect_agent_tool_names(llm_trace: &[LlmTurnTrace]) -> Vec<&str> {
    llm_trace
        .iter()
        .filter_map(|turn| turn.assistant.as_ref())
        .flat_map(|assistant| assistant.tool_calls.iter().map(|call| call.name.as_str()))
        .collect()
}

fn missing_required_names(required: &[String], seen: &HashSet<&str>) -> Vec<String> {
    required
        .iter()
        .filter(|tool| !seen.contains(tool.as_str()))
        .cloned()
        .collect()
}

fn missing_required_agent_tool_groups(
    required_groups: &[Vec<String>],
    agent_seen: &HashSet<&str>,
) -> Vec<Vec<String>> {
    required_groups
        .iter()
        .filter(|group| !group.iter().any(|tool| agent_seen.contains(tool.as_str())))
        .cloned()
        .collect()
}

fn count_agent_tool_names<'a>(agent_tool_names: &[&'a str]) -> HashMap<&'a str, usize> {
    let mut agent_counts = HashMap::new();
    for name in agent_tool_names {
        *agent_counts.entry(*name).or_insert(0) += 1;
    }
    agent_counts
}

fn missing_required_agent_tool_counts(
    required_counts: &HashMap<String, usize>,
    agent_counts: &HashMap<&str, usize>,
) -> Vec<String> {
    required_counts
        .iter()
        .filter_map(|(tool, required)| {
            let actual = agent_counts.get(tool.as_str()).copied().unwrap_or(0);
            (actual < *required).then(|| format!("{tool}: required {required}, saw {actual}"))
        })
        .collect()
}

fn count_forbidden_tool_calls(tool_trace: &[ToolTrace], forbidden: &HashSet<&str>) -> usize {
    tool_trace
        .iter()
        .filter(|call| forbidden.contains(call.tool.as_str()))
        .count()
}

fn count_forbidden_agent_tool_calls(agent_tool_names: &[&str], forbidden: &HashSet<&str>) -> usize {
    agent_tool_names
        .iter()
        .filter(|tool| forbidden.contains(**tool))
        .count()
}

fn count_invalid_tool_errors(
    tool_trace: &[ToolTrace],
    allowed_error_tools: &HashSet<&str>,
) -> usize {
    tool_trace
        .iter()
        .filter(|call| !call.success && !allowed_error_tools.contains(call.tool.as_str()))
        .count()
}

fn count_repeated_action_warnings(events: &[Value]) -> usize {
    events
        .iter()
        .filter(|event| {
            event.get("type").and_then(Value::as_str) == Some("warning")
                && event
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|m| m.contains("repeated"))
        })
        .count()
}

fn excess_over_limit(actual: usize, max: Option<usize>) -> usize {
    max.map(|limit| actual.saturating_sub(limit))
        .unwrap_or_default()
}
