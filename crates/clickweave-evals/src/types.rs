use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTrace {
    pub tool: String,
    pub arguments: Value,
    pub success: bool,
    pub result: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmTurnTrace {
    pub request_messages: Value,
    pub assistant: Option<AssistantTrace>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantTrace {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCallTrace>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallTrace {
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeterministicScore {
    pub score: f32,
    pub completed: bool,
    pub steps: usize,
    pub required_tools_missing: Vec<String>,
    pub required_agent_tools_missing: Vec<String>,
    pub required_agent_tool_groups_missing: Vec<Vec<String>>,
    pub required_agent_tool_counts_missing: Vec<String>,
    pub forbidden_tool_calls: usize,
    pub forbidden_agent_tool_calls: usize,
    pub invalid_tool_errors: usize,
    pub repeated_action_warnings: usize,
    pub agent_tool_calls: usize,
    pub max_agent_tool_calls_excess: usize,
    pub max_repeated_action_warnings_excess: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticJudgeReport {
    pub score: f32,
    pub verdict: String,
    pub failure_class: String,
    pub root_cause: String,
    #[serde(default)]
    pub prompt_feedback: Vec<String>,
    #[serde(default)]
    pub recommended_prompt_patch: String,
    pub overfit_risk: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub scenario_id: String,
    pub prompt_sha: String,
    pub deterministic: DeterministicScore,
    pub semantic_judge: Option<SemanticJudgeReport>,
    pub final_score: f32,
    pub tool_trace: Vec<ToolTrace>,
    pub llm_trace: Vec<LlmTurnTrace>,
    pub events: Vec<Value>,
    pub eval_halt: Option<EvalHalt>,
    pub privacy: PrivacyReport,
    pub run_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSuiteReport {
    pub scenario_count: usize,
    pub final_score_mean: f32,
    pub deterministic_score_mean: f32,
    pub prompt_sha: Option<String>,
    pub reports: Vec<EvalReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalHalt {
    pub reason: String,
    pub agent_tool: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyReport {
    pub synthetic_fixture_only: bool,
    pub screenshots_omitted: bool,
    pub secrets_redacted: bool,
    pub local_paths_redacted: bool,
}
