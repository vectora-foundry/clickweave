use clickweave_core::cdp::CdpFindElementMatch;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::skills::{SkillScope, SkillState};
use crate::agent::step_record::BoundaryKind;
use crate::agent::task_state::TaskState;
use crate::agent::trace_graph::AgentTraceGraph;

/// Wrapper carried on the engine-to-Tauri mpsc channel. Splits durable,
/// serializable events from one-shot channel control messages.
#[derive(Debug)]
pub enum RunnerOutput {
    /// A durable, serializable agent event. Persisted to `events.jsonl`
    /// and mapped to a Tauri topic by the forwarder.
    Event(AgentEvent),

    /// Non-persisted ordering barrier. The forwarder skips durable
    /// persistence for this variant and signals `ack` after every
    /// previously queued event has been handled.
    DrainBarrier {
        ack: tokio::sync::oneshot::Sender<()>,
    },

    /// Non-persisted Tauri-side trigger for skill proposal generation.
    SkillProposalNeeded {
        skill_id: String,
        version: u32,
        run_id: String,
    },
}

impl RunnerOutput {
    pub fn into_event(self) -> Option<AgentEvent> {
        match self {
            RunnerOutput::Event(event) => Some(event),
            RunnerOutput::DrainBarrier { .. } | RunnerOutput::SkillProposalNeeded { .. } => None,
        }
    }
}

impl From<AgentEvent> for RunnerOutput {
    fn from(event: AgentEvent) -> Self {
        RunnerOutput::Event(event)
    }
}

/// Default ceiling on agent observe-act iterations. Chosen to cover typical
/// multi-step tasks (login → action → confirm) with headroom while keeping a
/// runaway loop from burning through an LLM budget. Callers set a larger
/// value explicitly for research/automation tasks that need more steps.
const DEFAULT_MAX_STEPS: usize = 30;
/// Default consecutive-error budget before the agent aborts. Low on purpose
/// — hitting three errors in a row almost always means the strategy is
/// wrong rather than that one more retry would recover.
const DEFAULT_MAX_CONSECUTIVE_ERRORS: usize = 3;
/// Default consecutive-destructive-tool cap. Three irreversible actions in
/// a row is the circuit-breaker point where the operator should review.
const DEFAULT_CONSECUTIVE_DESTRUCTIVE_CAP: usize = 3;

/// Events emitted by the agent loop during execution.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    StepCompleted {
        step_index: usize,
        tool_name: String,
        summary: String,
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
    ///
    /// Emitted twice per sub-call: once before the MCP call (summary
    /// describes the intent) and once after (summary describes the
    /// outcome, including a failure reason when applicable). UI layers
    /// that want to render "started vs completed" pairs can match on the
    /// summary prefix.
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
    /// Emitted after `apply_mutations` applies at least one mutation
    /// during a turn. Carries the runner's `run_id` (D17) plus a
    /// snapshot of the current `TaskState` so frontends can reflect
    /// the updated subgoal stack / watch slots / phase without a round
    /// trip through the durable trace.
    TaskStateChanged {
        run_id: Uuid,
        task_state: TaskState,
    },
    /// Emitted once per step after `observe` runs, carrying a small
    /// list of `WorldModel` field names whose freshness-wrapped value
    /// changed during that step's observe phase. Consumers use the
    /// diff to decide which state-block sections to re-render.
    WorldModelChanged {
        run_id: Uuid,
        diff: WorldModelDiff,
    },
    /// Emitted every time a boundary `StepRecord` is persisted to
    /// `events.jsonl` (D8 / Task 3a.6.5). Lets the frontend surface
    /// boundary milestones without re-reading the durable trace.
    BoundaryRecordWritten {
        run_id: Uuid,
        boundary_kind: BoundaryKind,
        step_index: usize,
        milestone_text: Option<String>,
    },
    /// Emitted after the runner's episodic-retrieval pass returns at
    /// least one candidate (Spec 2 D24). Triggered on run-start and on
    /// the `Exploring/Executing -> Recovering` phase transition.
    /// `episode_ids` are the IDs of the retrieved episodes in
    /// score-descending order; `scope_breakdown` partitions `count`
    /// across the two stores so frontends can show provenance without
    /// re-walking the payload.
    ///
    /// Payload shape locked by Spec 2 D33
    /// (`docs/design/2026-04-24_agent-episodic-memory.md:699`).
    EpisodesRetrieved {
        run_id: Uuid,
        trigger: crate::agent::episodic::RetrievalTrigger,
        count: usize,
        episode_ids: Vec<String>,
        scope_breakdown: ScopeBreakdown,
    },
    /// Emitted by the episodic writer task after a `RecoverySucceeded`
    /// recovery snapshot is persisted to a SQLite store (Spec 2 D30).
    /// Carries the `run_id` captured at writer-spawn so the frontend's
    /// stale-run filter can drop late events from a previous run.
    /// `outcome` is `"inserted"` or `"merged"`, reflecting the
    /// dedup-aware insert path. `scope` identifies which store the
    /// row landed in (workflow-local for the `DeriveAndInsert` write
    /// path; global for promotion-time writes routed through the
    /// shared store API).
    ///
    /// Payload shape locked by Spec 2 D33
    /// (`docs/design/2026-04-24_agent-episodic-memory.md:700`).
    EpisodeWritten {
        run_id: Uuid,
        outcome: String,
        episode_id: String,
        scope: crate::agent::episodic::EpisodeScope,
        occurrence_count: u32,
    },
    /// Emitted by the episodic writer task after the run-terminal
    /// promotion pass copies one or more workflow-local episodes into
    /// the global store (Spec 2 D31). `promoted_episode_ids` lists
    /// the global-store row IDs each candidate landed at (existing
    /// row IDs on dedup-merge, freshly minted IDs on insert);
    /// `skipped_count` is the number of candidates the
    /// `should_promote` gate rejected.
    ///
    /// Payload shape locked by Spec 2 D33
    /// (`docs/design/2026-04-24_agent-episodic-memory.md:701`).
    EpisodePromoted {
        run_id: Uuid,
        promoted_episode_ids: Vec<String>,
        skipped_count: usize,
    },
    /// Emitted by the runner each time the LLM picks `invoke_skill`
    /// from the offered `<applicable_skills>` block and the runner
    /// has resolved the target to a confirmed/promoted on-disk skill.
    /// Carries the `(skill_id, version)` pair plus the validated
    /// parameter count (the parameters themselves stay off the wire so
    /// frontends can render a count chip without size-cap concerns).
    SkillInvoked {
        run_id: Uuid,
        skill_id: String,
        version: u32,
        parameter_count: u32,
    },
    /// Emitted by the runner after the extractor inserts or merges a
    /// skill at a `CompleteSubgoal` boundary. Frontends use this to
    /// refresh the Skills panel index without polling. `state` and
    /// `scope` echo the skill's on-disk fields so a panel slice can
    /// place the entry in the correct bucket without an extra read.
    SkillExtracted {
        run_id: Uuid,
        skill_id: String,
        version: u32,
        state: SkillState,
        scope: SkillScope,
    },
    /// Emitted by the Tauri command layer after a user accepts an LLM
    /// refinement proposal and `confirm_skill_proposal` flips a draft
    /// skill to `Confirmed`. Frontends use this to move a skill from
    /// the Drafts bucket to the Confirmed bucket.
    SkillConfirmed {
        run_id: Uuid,
        skill_id: String,
        version: u32,
    },
}

/// Scope partitioning carried by [`AgentEvent::EpisodesRetrieved`].
/// Exists as a named type so frontends can address the breakdown
/// shape directly instead of unpacking flat sibling fields, matching
/// the locked Spec 2 D33 payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ScopeBreakdown {
    /// Number of retrieved episodes from the workflow-local store.
    pub workflow: usize,
    /// Number of retrieved episodes from the cross-workflow global store.
    pub global: usize,
}

/// Per-step diff of the harness-owned `WorldModel`. Carries the field
/// names whose freshness-wrapped value changed during the current
/// step's `observe` pass (D17). Intentionally minimal — the frontend
/// only needs a re-render hint, not the full snapshot. Field order
/// matches `WorldModel`'s definition.
#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct WorldModelDiff {
    /// Names of `WorldModel` fields whose value changed this step.
    /// Stable field names: `focused_app`, `window_list`, `cdp_page`,
    /// `elements`, `modal_present`, `dialog_present`,
    /// `last_screenshot`, `last_native_ax_snapshot`, `uncertainty`.
    pub changed_fields: Vec<String>,
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
    /// Halt the run after this many consecutive destructive tool calls.
    /// `0` disables the cap entirely.
    pub consecutive_destructive_cap: usize,
    /// Whether the agent is allowed to execute `focus_window` at all.
    ///
    /// Defaults to `false` — runs operate in the background by default and
    /// never steal foreground from the user. Every `focus_window` call is
    /// suppressed unconditionally at the dispatch site (the synthetic skip
    /// message nudges the LLM toward AX / CDP dispatch instead). Operators
    /// who genuinely need focus-stealing for OS-chrome work — e.g.
    /// menubar / dock / Spotlight — can opt in by setting this to `true`,
    /// at which point the AX / CDP-scoped guards in `runner.rs` decide
    /// per-call whether the focus is redundant and should be suppressed
    /// anyway.
    pub allow_focus_window: bool,
    /// Maximum elements to render in the state block (D19). The runner may
    /// fetch a larger CDP set for fingerprints/inventory, but the prompt
    /// renders a bounded slice so one page cannot dominate the context window.
    pub state_block_max_elements: usize,
    /// Recent-N window for compaction (D12).
    pub recent_n: usize,
    /// Uncertainty threshold above which the state block marks fields
    /// as "?" rather than rendering their nominal value (D14).
    pub uncertainty_threshold: f32,

    // Spec 2 episodic memory fields ------------------------------------
    /// Master kill-switch. If false, episodic is inactive regardless of
    /// other state (D34).
    pub episodic_enabled: bool,
    /// Top-k episodes to retrieve (default 2).
    pub retrieved_episodes_k: usize,
    /// Per-scope LRU cap for the workflow-local store.
    pub episodic_max_per_scope_workflow: usize,
    /// Per-scope LRU cap for the global store.
    pub episodic_max_per_scope_global: usize,
    /// Half-life (in days) of the time-decay scoring factor.
    pub episodic_decay_halflife_days: f32,
    /// Score weights forwarded to the store at construction time.
    pub episodic_score_weights: EpisodicScoreWeights,
    /// Maximum number of global-tier hits that may show up in a single
    /// retrieval before the workflow-priority merge truncates.
    pub episodic_global_cap_per_retrieval: usize,
    /// Multiplier applied to workflow-local scores before the cross-tier
    /// merge — bumps in-workflow recoveries above global ones at equal
    /// raw score (D21).
    pub episodic_workflow_priority_multiplier: f32,

    // Spec 3 procedural-skills fields ----------------------------------
    /// Master kill-switch for the procedural-skills layer. When false,
    /// extraction, retrieval, and replay all become no-ops regardless of
    /// other state.
    pub skills_enabled: bool,
    /// Top-k applicable skills surfaced into the next user turn after a
    /// `push_subgoal` mutation (default 2).
    pub applicable_skills_k: usize,
    /// Whether the run reads from / writes to the opt-in global skill
    /// store (off by default — keeps cross-project skill exposure
    /// deliberate).
    pub skills_global_participation: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct EpisodicScoreWeights {
    pub structured: f32,
    pub text: f32,
    pub occurrence: f32,
}

impl Default for EpisodicScoreWeights {
    fn default() -> Self {
        Self {
            structured: 0.6,
            text: 0.3,
            occurrence: 0.1,
        }
    }
}

impl From<EpisodicScoreWeights> for crate::agent::episodic::retrieval::ScoreWeights {
    fn from(w: EpisodicScoreWeights) -> Self {
        Self {
            structured: w.structured,
            text: w.text,
            occurrence: w.occurrence,
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_steps: DEFAULT_MAX_STEPS,
            max_consecutive_errors: DEFAULT_MAX_CONSECUTIVE_ERRORS,
            build_workflow: true,
            consecutive_destructive_cap: DEFAULT_CONSECUTIVE_DESTRUCTIVE_CAP,
            allow_focus_window: false,
            state_block_max_elements: crate::agent::render::DEFAULT_MAX_ELEMENTS,
            recent_n: 6,
            uncertainty_threshold: 0.75,
            episodic_enabled: true,
            retrieved_episodes_k: 2,
            episodic_max_per_scope_workflow: 500,
            episodic_max_per_scope_global: 2000,
            episodic_decay_halflife_days: 90.0,
            episodic_score_weights: EpisodicScoreWeights::default(),
            episodic_global_cap_per_retrieval: 1,
            episodic_workflow_priority_multiplier: 1.3,
            skills_enabled: true,
            applicable_skills_k: 2,
            skills_global_participation: false,
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

/// Mutable state accumulated during an agent run.
#[derive(Debug)]
pub struct AgentState {
    /// Steps executed so far.
    pub steps: Vec<AgentStep>,
    /// Trace graph being built (when `build_workflow` is true).
    pub trace_graph: AgentTraceGraph,
    /// ID of the last node added to the trace graph.
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
    pub fn new(trace_graph: AgentTraceGraph) -> Self {
        Self {
            steps: Vec::new(),
            trace_graph,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
