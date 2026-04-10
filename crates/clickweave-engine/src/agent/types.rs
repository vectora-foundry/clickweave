use clickweave_core::Workflow;
use clickweave_core::cdp::CdpFindElementMatch;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Configuration for an agent run.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Maximum number of observe-act iterations before the agent gives up.
    pub max_steps: usize,
    /// Maximum consecutive errors before aborting.
    pub max_consecutive_errors: usize,
    /// Whether to build a workflow graph as the agent executes.
    pub build_workflow: bool,
    /// Whether to use the decision cache for repeated page states.
    pub use_cache: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_steps: 30,
            max_consecutive_errors: 3,
            build_workflow: true,
            use_cache: true,
        }
    }
}

/// A single step in the agent's observe-act history.
#[derive(Debug, Clone)]
pub struct AgentStep {
    /// Step index (0-based).
    pub index: usize,
    /// Elements visible on the page at this step.
    pub elements: Vec<CdpFindElementMatch>,
    /// The command the LLM chose.
    pub command: AgentCommand,
    /// The outcome of executing the command.
    pub outcome: StepOutcome,
    /// Page URL at the time of observation.
    pub page_url: String,
}

/// The action the LLM decided to take.
#[derive(Debug, Clone)]
pub enum AgentCommand {
    /// Execute an MCP tool call.
    ToolCall {
        tool_name: String,
        arguments: serde_json::Value,
        tool_call_id: String,
    },
    /// The agent declares the goal is complete.
    Done { summary: String },
    /// The agent requests a re-plan (goal seems unreachable).
    Replan { reason: String },
    /// The LLM returned text instead of a tool call.
    TextOnly { text: String },
}

/// Mutable state accumulated during an agent run.
#[derive(Debug)]
pub struct AgentState {
    /// Steps executed so far.
    pub steps: Vec<AgentStep>,
    /// Workflow being built (when `build_workflow` is true).
    pub workflow: Workflow,
    /// ID of the last node added to the workflow graph.
    pub last_node_id: Option<Uuid>,
    /// Consecutive error count (resets on success).
    pub consecutive_errors: usize,
    /// Whether the agent has completed its goal.
    pub completed: bool,
    /// Final summary when the agent completes.
    pub summary: Option<String>,
    /// Current page URL.
    pub current_url: String,
}

impl AgentState {
    pub fn new(workflow: Workflow) -> Self {
        Self {
            steps: Vec::new(),
            workflow,
            last_node_id: None,
            consecutive_errors: 0,
            completed: false,
            summary: None,
            current_url: String::new(),
        }
    }
}

/// The result of executing a single step.
#[derive(Debug, Clone)]
pub enum StepOutcome {
    /// Tool executed successfully with the given result text.
    Success(String),
    /// Tool execution failed with an error message.
    Error(String),
    /// Agent declared the goal complete.
    Done(String),
    /// Agent requested a re-plan.
    Replan(String),
}

/// A cached decision for a previously seen page state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedDecision {
    /// The tool name that was called.
    pub tool_name: String,
    /// The tool arguments.
    pub arguments: serde_json::Value,
    /// Fingerprint of the page elements at the time of the decision.
    pub element_fingerprint: String,
    /// Number of times this cache entry has been used.
    pub hit_count: u32,
}

/// In-memory cache mapping page fingerprints to past decisions.
#[derive(Debug, Default)]
pub struct AgentCache {
    /// Map from cache key to cached decision.
    pub entries: HashMap<String, CachedDecision>,
}
