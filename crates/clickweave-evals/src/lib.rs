use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::{error::Error, fmt};

use anyhow::{Context, Result, bail};
use clickweave_engine::Mcp;
use clickweave_engine::agent::{
    AgentChannels, AgentConfig, PermissionPolicy, RunnerOutput,
    run_agent_workflow_with_prompt_override,
};
use clickweave_llm::{ChatBackend, ChatOptions, ChatResponse, LlmConfig, Message, ToolCall};
use clickweave_mcp::{ToolCallResult, ToolContent};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const CODEX_JUDGE_PROMPT: &str = include_str!("../prompts/codex_judge.md");

mod judge;
mod mcp;
mod privacy;
mod recording;
mod runner;
mod scenario;
mod scoring;
mod types;

pub use judge::parse_judge_report;
pub(crate) use judge::run_semantic_judge;
pub use mcp::ScenarioMcp;
pub(crate) use privacy::{
    personal_marker, private_marker, prompt_sha, redact_messages, redact_tool_call,
};
pub use privacy::{redact_text, redact_value};
pub(crate) use recording::EvalHaltTriggered;
pub use recording::RecordingBackend;
pub use runner::run_eval;
pub use scenario::{
    EvalScenario, ScoringSpec, ToolBehavior, ToolResponse, ToolSpec, load_scenarios_dir,
};
pub(crate) use scoring::{count_step_events, score_deterministic};
pub use types::{
    AssistantTrace, DeterministicScore, EvalHalt, EvalReport, EvalSuiteReport, LlmTurnTrace,
    PrivacyReport, SemanticJudgeReport, ToolCallTrace, ToolTrace,
};

pub fn llm_config(base_url: String, model: String, api_key: Option<String>) -> LlmConfig {
    LlmConfig {
        base_url,
        model,
        api_key: api_key.filter(|key| !key.is_empty()),
        temperature: Some(0.0),
        max_tokens: Some(2048),
        ..LlmConfig::default()
    }
    .with_thinking(false)
}

#[cfg(test)]
mod tests;
