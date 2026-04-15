use clickweave_core::Workflow;
use clickweave_core::cdp::CdpFindElementMatch;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Events emitted by the agent loop during execution.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    StepCompleted {
        step_index: usize,
        tool_name: String,
        summary: String,
    },
    NodeAdded {
        node: Box<clickweave_core::Node>,
    },
    EdgeAdded {
        edge: clickweave_core::Edge,
    },
    GoalComplete {
        summary: String,
    },
    Error {
        message: String,
    },
    /// A nonfatal warning — the run continues but the operator should know.
    Warning {
        message: String,
    },
    CdpConnected {
        app_name: String,
        port: u16,
    },
    StepFailed {
        step_index: usize,
        tool_name: String,
        error: String,
    },
    /// An automatic sub-action performed by the agent (e.g. CDP auto-connect
    /// probing, quitting, relaunching). Not a user-approved step.
    SubAction {
        tool_name: String,
        summary: String,
    },
    /// VLM completion verification disagreed with the agent's self-reported
    /// `agent_done`. The run is halted and the user is shown the screenshot
    /// plus the VLM reasoning so they can decide whether the agent really
    /// completed the goal.
    CompletionDisagreement {
        /// Base64-encoded JPEG produced by the `take_screenshot` MCP tool,
        /// already prepared for VLM consumption (resized + re-encoded).
        screenshot_b64: String,
        /// The full VLM response text, including any explanation after the
        /// YES/NO token.
        vlm_reasoning: String,
        /// The summary the agent provided when it called `agent_done`.
        agent_summary: String,
    },
    /// The agent executed N destructive tool calls in a row, hitting the
    /// configured cap. The run halts; the UI surfaces a short notice.
    ConsecutiveDestructiveCapHit {
        /// Tool names of the destructive calls that triggered the halt,
        /// in execution order (oldest first).
        recent_tool_names: Vec<String>,
        /// The cap value the run was configured with.
        cap: usize,
    },
}

/// Approval request sent to the UI before executing an action.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalRequest {
    pub step_index: usize,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub description: String,
}

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
    /// Halt the run after this many consecutive destructive tool calls.
    /// `0` disables the cap entirely.
    pub consecutive_destructive_cap: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_steps: 30,
            max_consecutive_errors: 3,
            build_workflow: true,
            use_cache: true,
            consecutive_destructive_cap: 3,
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

impl AgentCommand {
    /// Return the tool name if this is a tool call, or `"unknown"` otherwise.
    pub fn tool_name_or_unknown(&self) -> &str {
        match self {
            Self::ToolCall { tool_name, .. } => tool_name,
            _ => "unknown",
        }
    }
}

/// Truncate text to `max_chars`, snapping to a character boundary.
/// Returns the original text if it fits within the limit.
pub fn truncate_summary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let end = text.floor_char_boundary(max_chars);
    format!("{}...", &text[..end])
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
    /// Why the agent loop terminated. `None` while still running.
    pub terminal_reason: Option<TerminalReason>,
    /// Current page URL.
    pub current_url: String,
    /// Destructive tool names executed consecutively, oldest first.
    /// Resets to empty when a non-destructive tool succeeds. The length
    /// is compared against `AgentConfig::consecutive_destructive_cap`.
    pub recent_destructive_tools: Vec<String>,
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
            terminal_reason: None,
            current_url: String::new(),
            recent_destructive_tools: Vec::new(),
        }
    }
}

/// Why the agent loop terminated.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum TerminalReason {
    /// The agent called `agent_done`.
    Completed { summary: String },
    /// The loop hit the `max_steps` limit without completing.
    MaxStepsReached { steps_executed: usize },
    /// Too many consecutive errors triggered abort.
    MaxErrorsReached { consecutive_errors: usize },
    /// The approval channel is permanently unavailable (receiver dropped).
    ApprovalUnavailable,
    /// The agent called `agent_done`, but the VLM completion check disagreed
    /// based on the post-run screenshot. The run halts and the UI surfaces
    /// the disagreement for user adjudication instead of re-planning.
    CompletionDisagreement {
        agent_summary: String,
        vlm_reasoning: String,
    },
    /// The consecutive-destructive-call cap was reached. The run halts so
    /// the operator can review what the agent did instead of barrelling
    /// through more destructive actions unchecked.
    ConsecutiveDestructiveCap {
        /// Destructive tools that triggered the cap, in execution order.
        recent_tool_names: Vec<String>,
        /// The cap the run was configured with.
        cap: usize,
    },
}

impl TerminalReason {
    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }

    pub fn divergence_summary(&self) -> String {
        match self {
            Self::Completed { summary } => format!("Completed: {}", summary),
            Self::MaxStepsReached { steps_executed } => {
                format!("Stopped after {} steps (max steps reached)", steps_executed)
            }
            Self::MaxErrorsReached { consecutive_errors } => {
                format!("Aborted after {} consecutive errors", consecutive_errors)
            }
            Self::ApprovalUnavailable => "Aborted: approval system unavailable".to_string(),
            Self::CompletionDisagreement { vlm_reasoning, .. } => {
                format!("Completion verification disagreed: {}", vlm_reasoning)
            }
            Self::ConsecutiveDestructiveCap {
                recent_tool_names,
                cap,
            } => format!(
                "Halted: reached {} consecutive destructive actions ({})",
                cap,
                recent_tool_names.join(", ")
            ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_summary_short_text_unchanged() {
        assert_eq!(truncate_summary("hello", 10), "hello");
    }

    #[test]
    fn truncate_summary_long_text_truncated() {
        let long = "a".repeat(200);
        let result = truncate_summary(&long, 50);
        assert!(result.len() < 60);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn truncate_summary_multibyte_snaps_to_boundary() {
        // 3 bytes per char × 4 = 12 bytes; truncate at 5 snaps to char boundary
        let text = "café!"; // 'é' is 2 bytes
        let result = truncate_summary(text, 4);
        assert!(result.ends_with("..."));
        // Should not panic or split a multibyte char
    }
}
