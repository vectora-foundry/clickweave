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
    /// The operator resolved a pending `CompletionDisagreement`. Emitted
    /// by the Tauri layer after the user chooses Confirm or Cancel in the
    /// assistant panel. Persisted to `events.jsonl` so the durable run
    /// trace records the operator's final decision.
    CompletionDisagreementResolved {
        /// Operator decision — `"confirm"` (override VLM, mark complete)
        /// or `"cancel"` (agree with VLM, halt the run).
        action: DisagreementResolutionAction,
        /// The summary the agent provided with `agent_done`.
        agent_summary: String,
        /// The VLM reasoning the operator saw before deciding.
        vlm_reasoning: String,
    },
}

/// Operator decision for a pending `CompletionDisagreement`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DisagreementResolutionAction {
    /// Override the VLM — the agent's self-reported completion stands.
    Confirm,
    /// Agree with the VLM — cancel the run.
    Cancel,
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
    /// The operator resolved a prior `CompletionDisagreement` by confirming
    /// that the agent really did complete the goal (VLM was wrong). This
    /// reason is synthesized by the Tauri layer after the user's decision,
    /// not by the engine's loop runner — but it lives here so the persisted
    /// terminal state in `events.jsonl` + variant-index stays a single
    /// match-on-one-enum shape.
    DisagreementConfirmed { agent_summary: String },
    /// The operator resolved a prior `CompletionDisagreement` by cancelling
    /// the run (they agreed with the VLM). Synthesized by the Tauri layer.
    DisagreementCancelled {
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
    /// The agent invoked the identical (tool, args) pair twice in a row and
    /// got the identical error back both times. Treated as a deterministic
    /// loop and halted immediately rather than burning through the
    /// `max_consecutive_errors` budget on the same failing call.
    LoopDetected { tool_name: String, error: String },
}

impl TerminalReason {
    pub fn is_completed(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::DisagreementConfirmed { .. }
        )
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
            Self::DisagreementConfirmed { agent_summary } => format!(
                "Completed (user override after VLM disagreement): {}",
                agent_summary
            ),
            Self::DisagreementCancelled { vlm_reasoning, .. } => format!(
                "Cancelled by user after VLM disagreement: {}",
                vlm_reasoning
            ),
            Self::ConsecutiveDestructiveCap {
                recent_tool_names,
                cap,
            } => format!(
                "Halted: reached {} consecutive destructive actions ({})",
                cap,
                recent_tool_names.join(", ")
            ),
            Self::LoopDetected { tool_name, error } => format!(
                "Loop detected: `{}` kept returning the same error — {}",
                tool_name, error
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
    /// Node UUIDs this cached decision has produced over its lifetime.
    /// A single decision can produce multiple nodes when replayed across
    /// runs. Eviction-on-delete removes the decision only when this Vec
    /// becomes empty. Legacy entries deserialize as empty.
    #[serde(default)]
    pub produced_node_ids: Vec<Uuid>,
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

    #[test]
    fn disagreement_confirmed_is_completed() {
        let reason = TerminalReason::DisagreementConfirmed {
            agent_summary: "clicked Submit".to_string(),
        };
        assert!(
            reason.is_completed(),
            "DisagreementConfirmed must be treated as a successful completion \
             so the variant index records success=true"
        );
    }

    #[test]
    fn disagreement_cancelled_is_not_completed() {
        let reason = TerminalReason::DisagreementCancelled {
            agent_summary: "clicked Submit".to_string(),
            vlm_reasoning: "modal still visible".to_string(),
        };
        assert!(
            !reason.is_completed(),
            "DisagreementCancelled must record success=false so future runs \
             know the operator cancelled after VLM disagreement"
        );
    }

    #[test]
    fn disagreement_confirmed_summary_embeds_agent_summary() {
        let reason = TerminalReason::DisagreementConfirmed {
            agent_summary: "calculator shows 42".to_string(),
        };
        let summary = reason.divergence_summary();
        assert!(summary.contains("user override"));
        assert!(summary.contains("calculator shows 42"));
    }

    #[test]
    fn disagreement_cancelled_summary_embeds_vlm_reasoning() {
        let reason = TerminalReason::DisagreementCancelled {
            agent_summary: "clicked Submit".to_string(),
            vlm_reasoning: "form still showing errors".to_string(),
        };
        let summary = reason.divergence_summary();
        assert!(summary.contains("Cancelled"));
        assert!(summary.contains("form still showing errors"));
    }

    #[test]
    fn disagreement_resolution_action_serializes_lowercase() {
        let confirm = serde_json::to_string(&DisagreementResolutionAction::Confirm).unwrap();
        assert_eq!(confirm, "\"confirm\"");
        let cancel = serde_json::to_string(&DisagreementResolutionAction::Cancel).unwrap();
        assert_eq!(cancel, "\"cancel\"");
    }

    #[test]
    fn disagreement_resolution_action_round_trips() {
        for expected in [
            DisagreementResolutionAction::Confirm,
            DisagreementResolutionAction::Cancel,
        ] {
            let s = serde_json::to_string(&expected).unwrap();
            let parsed: DisagreementResolutionAction = serde_json::from_str(&s).unwrap();
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn cached_decision_default_produced_node_ids_is_empty() {
        let d = CachedDecision {
            tool_name: "click".to_string(),
            arguments: serde_json::Value::Null,
            element_fingerprint: String::new(),
            hit_count: 0,
            produced_node_ids: Vec::new(),
        };
        assert!(d.produced_node_ids.is_empty());
    }

    #[test]
    fn cached_decision_missing_produced_node_ids_defaults_to_empty() {
        // Legacy cache entries have no produced_node_ids field.
        let json = r#"{
            "tool_name": "click",
            "arguments": {"uid": "1_0"},
            "element_fingerprint": "abc",
            "hit_count": 1
        }"#;
        let d: CachedDecision = serde_json::from_str(json).unwrap();
        assert!(
            d.produced_node_ids.is_empty(),
            "legacy cache entries must deserialize with empty produced_node_ids"
        );
    }
}
