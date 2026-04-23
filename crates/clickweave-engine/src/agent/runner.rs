//! State-spine agent runner.
//!
//! This module implements the single-pass ReAct loop over a harness-owned
//! `WorldModel` + `TaskState`. Each LLM turn produces an `AgentTurn`:
//! 0..N typed task-state mutations followed by exactly one action.
//!
//! Phase 2c: the runner type is built up incrementally across a series of
//! tasks, alongside its tests. It is **not** wired into the public re-exports
//! of `agent/mod.rs` — the cutover from `loop_runner.rs` lands in Phase 3.

#![allow(dead_code)] // Phase 2c: live wiring lands in Phase 3 cutover.

use serde::{Deserialize, Serialize};

use crate::agent::phase::{self, PhaseSignals};
use crate::agent::task_state::{TaskState, TaskStateMutation};
use crate::agent::types::{AgentCache, AgentConfig, AgentState};
use crate::agent::world_model::{InvalidationEvent, WorldModel};

/// The one action an `AgentTurn` must carry (D10).
///
/// `ToolCall` dispatches to MCP; `AgentDone` / `AgentReplan` are harness-local
/// pseudo-tools that never reach MCP.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentAction {
    ToolCall {
        tool_name: String,
        arguments: serde_json::Value,
        tool_call_id: String,
    },
    AgentDone {
        summary: String,
    },
    AgentReplan {
        reason: String,
    },
}

/// Batched single-pass agent output: task-state mutations followed by one
/// action. Mutations apply in order before the action dispatches.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentTurn {
    pub mutations: Vec<TaskStateMutation>,
    pub action: AgentAction,
}

/// State-spine runner. Owns the harness-side `WorldModel` + `TaskState` and
/// drives a single-pass ReAct loop: observe -> render -> decide -> apply ->
/// dispatch -> continuity -> invalidate.
///
/// Phase 2c: the struct carries a superset of fields — the minimum the new
/// control loop exercises plus the "compatibility fields" the Phase 3 cutover
/// needs to preserve the existing `run_agent_workflow` seam
/// (`(AgentState, AgentCache)` tuple). Fields the live Phase 2c tests don't
/// touch are covered by the module-wide `#![allow(dead_code)]`.
pub struct StateRunner {
    // --- Core state-spine fields ---
    pub world_model: WorldModel,
    pub task_state: TaskState,
    pub step_index: usize,
    pub consecutive_errors: usize,
    pub last_replan_step: Option<usize>,
    pub pending_events: Vec<InvalidationEvent>,

    // --- Compatibility fields (P2.H4) ---
    // Carried so the Phase 3 cutover can swap the public seam without
    // silently dropping what callers rely on today.
    pub config: AgentConfig,
    pub state: AgentState,
    pub workflow: clickweave_core::Workflow,
    pub cache: AgentCache,
    pub last_node_id: Option<uuid::Uuid>,
    pub recent_destructive_tools: Vec<String>,

    // --- Collaborators (builder-style) ---
    pub storage: Option<std::sync::Arc<std::sync::Mutex<clickweave_core::storage::RunStorage>>>,
    pub run_id: uuid::Uuid,
}

impl StateRunner {
    pub fn new(goal: String, config: AgentConfig) -> Self {
        let workflow = clickweave_core::Workflow::default();
        let state = AgentState::new(workflow.clone());
        Self {
            world_model: WorldModel::default(),
            task_state: TaskState::new(goal),
            step_index: 0,
            consecutive_errors: 0,
            last_replan_step: None,
            pending_events: Vec::new(),
            config,
            state,
            workflow,
            cache: AgentCache::default(),
            last_node_id: None,
            recent_destructive_tools: Vec::new(),
            storage: None,
            run_id: uuid::Uuid::new_v4(),
        }
    }

    pub fn with_cache(mut self, cache: AgentCache) -> Self {
        self.cache = cache;
        self
    }

    pub fn with_run_id(mut self, run_id: uuid::Uuid) -> Self {
        self.run_id = run_id;
        self
    }

    /// Consume the runner and return `(AgentState, AgentCache)` — matches the
    /// existing `run_agent_workflow` seam so the Tauri call site keeps
    /// working without a public-surface change at cutover.
    pub fn into_state_and_cache(self) -> (AgentState, AgentCache) {
        (self.state, self.cache)
    }

    #[cfg(test)]
    pub fn new_for_test(goal: String) -> Self {
        Self::new(goal, AgentConfig::default())
    }

    pub fn queue_invalidation(&mut self, e: InvalidationEvent) {
        self.pending_events.push(e);
    }

    /// Apply any pending invalidation events and re-infer the phase from
    /// structural signals.
    pub fn observe(&mut self) {
        let events = std::mem::take(&mut self.pending_events);
        self.world_model.apply_events(events);
        self.task_state.phase = phase::infer(&PhaseSignals {
            stack_depth: self.task_state.subgoal_stack.len(),
            consecutive_errors: self.consecutive_errors,
            last_replan_step: self.last_replan_step,
            current_step: self.step_index,
        });
    }

    /// Whether the step is eligible to be served from `AgentCache` without
    /// asking the LLM (D17). Only in `Phase::Exploring` with an empty
    /// subgoal stack and no active watch slots — anything else means the
    /// LLM has in-flight intent that a cached decision would clobber.
    pub fn is_replay_eligible(&self) -> bool {
        self.task_state.phase == crate::agent::phase::Phase::Exploring
            && self.task_state.subgoal_stack.is_empty()
            && self.task_state.watch_slots.is_empty()
    }
}

#[cfg(test)]
mod observe_tests {
    use super::*;

    #[test]
    fn observe_applies_pending_events_and_infers_phase() {
        let mut runner = StateRunner::new_for_test("goal".to_string());
        runner.queue_invalidation(InvalidationEvent::FocusChanging {
            tool: "launch_app".to_string(),
        });
        runner.observe();
        assert_eq!(
            runner.task_state.phase,
            crate::agent::phase::Phase::Exploring
        );
    }
}

#[cfg(test)]
mod cache_gate_tests {
    use super::*;
    use crate::agent::task_state::{TaskStateMutation, WatchSlotName};

    #[test]
    fn replay_eligible_only_in_exploring_with_empty_state() {
        let mut r = StateRunner::new_for_test("g".to_string());
        r.observe();
        assert!(r.is_replay_eligible());
    }

    #[test]
    fn replay_not_eligible_with_active_subgoal() {
        let mut r = StateRunner::new_for_test("g".to_string());
        r.task_state
            .apply(
                &TaskStateMutation::PushSubgoal {
                    text: "x".to_string(),
                },
                1,
            )
            .unwrap();
        r.observe();
        assert!(!r.is_replay_eligible());
    }

    #[test]
    fn replay_not_eligible_with_active_watch_slot() {
        let mut r = StateRunner::new_for_test("g".to_string());
        r.task_state
            .apply(
                &TaskStateMutation::SetWatchSlot {
                    name: WatchSlotName::PendingModal,
                    note: "n".to_string(),
                },
                1,
            )
            .unwrap();
        r.observe();
        assert!(!r.is_replay_eligible());
    }

    #[test]
    fn replay_not_eligible_when_recovering() {
        let mut r = StateRunner::new_for_test("g".to_string());
        r.consecutive_errors = 1;
        r.observe();
        assert!(!r.is_replay_eligible());
    }
}

#[cfg(test)]
mod agent_turn_parsing_tests {
    use super::*;

    #[test]
    fn parses_tool_call_with_no_mutations() {
        let json = r#"{
            "mutations": [],
            "action": {"kind":"tool_call","tool_name":"cdp_click","arguments":{"uid":"d5"},"tool_call_id":"tc-1"}
        }"#;
        let turn: AgentTurn = serde_json::from_str(json).unwrap();
        assert!(turn.mutations.is_empty());
        match turn.action {
            AgentAction::ToolCall { tool_name, .. } => assert_eq!(tool_name, "cdp_click"),
            _ => panic!("expected tool_call"),
        }
    }

    #[test]
    fn parses_agent_done() {
        let json = r#"{
            "mutations": [],
            "action": {"kind":"agent_done","summary":"completed login"}
        }"#;
        let turn: AgentTurn = serde_json::from_str(json).unwrap();
        match turn.action {
            AgentAction::AgentDone { summary } => assert_eq!(summary, "completed login"),
            _ => panic!("expected agent_done"),
        }
    }

    #[test]
    fn parses_mutations_then_action() {
        let json = r#"{
            "mutations": [
                {"kind":"push_subgoal","text":"open login"},
                {"kind":"record_hypothesis","text":"form has 2 fields"}
            ],
            "action": {"kind":"tool_call","tool_name":"cdp_find_elements","arguments":{},"tool_call_id":"tc-2"}
        }"#;
        let turn: AgentTurn = serde_json::from_str(json).unwrap();
        assert_eq!(turn.mutations.len(), 2);
    }

    #[test]
    fn rejects_missing_action() {
        let json = r#"{"mutations": []}"#;
        let result = serde_json::from_str::<AgentTurn>(json);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_unknown_mutation_kind() {
        let json = r#"{
            "mutations": [{"kind":"set_phase","phase":"executing"}],
            "action": {"kind":"agent_done","summary":""}
        }"#;
        let result = serde_json::from_str::<AgentTurn>(json);
        assert!(result.is_err(), "set_phase is not a valid mutation (D5)");
    }

    #[test]
    fn rejects_malformed_json() {
        // P1.M4: the design's error-path table says a malformed AgentTurn
        // triggers one repair retry; the parser must surface the error
        // clearly rather than returning a default.
        let json = r#"{"mutations": [], "action":"#; // truncated
        let result = serde_json::from_str::<AgentTurn>(json);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_tool_call_without_tool_name() {
        let json = r#"{
            "mutations": [],
            "action": {"kind":"tool_call","arguments":{},"tool_call_id":"tc-1"}
        }"#;
        let result = serde_json::from_str::<AgentTurn>(json);
        assert!(result.is_err(), "tool_call must require tool_name");
    }

    #[test]
    fn accepts_tool_call_with_empty_arguments_object() {
        // Empty arguments is valid — some tools take no args (e.g. take_ax_snapshot).
        let json = r#"{
            "mutations": [],
            "action": {"kind":"tool_call","tool_name":"take_ax_snapshot","arguments":{},"tool_call_id":"tc-1"}
        }"#;
        let turn: AgentTurn = serde_json::from_str(json).unwrap();
        assert!(matches!(turn.action, AgentAction::ToolCall { .. }));
    }
}
