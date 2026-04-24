//! State-spine agent runner.
//!
//! This module implements the single-pass ReAct loop over a harness-owned
//! `WorldModel` + `TaskState`. Each LLM turn produces an `AgentTurn`:
//! 0..N typed task-state mutations followed by exactly one action.
//!
//! Phase 2c: the runner type is built up incrementally across a series of
//! tasks, alongside its tests. Phase 3 landed the cutover, replacing the
//! legacy runner with this state-spine module.

#![allow(dead_code)] // Phase 2c: live wiring lands in Phase 3 cutover.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use clickweave_llm::{ChatBackend, DynChatBackend, Message};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::agent::permissions::{
    PermissionAction, PermissionPolicy, ToolAnnotations, evaluate as evaluate_permission,
};
use crate::agent::phase::{self, PhaseSignals};
use crate::agent::recovery::{RecoveryAction, recovery_strategy};
use crate::agent::task_state::{TaskState, TaskStateMutation};
use crate::agent::types::{
    AgentCache, AgentCommand, AgentConfig, AgentEvent, AgentState, AgentStep, ApprovalRequest,
    StepOutcome, TerminalReason, WorldModelDiff,
};
use crate::agent::world_model::{InvalidationEvent, WorldModel};
use crate::executor::Mcp;

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
    /// Live event channel. When `None` the runner runs silently.
    pub event_tx: Option<mpsc::Sender<AgentEvent>>,
    /// Approval-gate channel pair. When `None` no prompt fires (the
    /// permission policy is consulted in isolation).
    pub approval_gate: Option<crate::agent::approval::ApprovalGate>,
    /// Optional VLM backend used to verify `agent_done`. Stored as
    /// `Arc<dyn DynChatBackend>` per D-PR1 so primary and VLM backends
    /// can be different concrete types without polluting `StateRunner`'s
    /// generics.
    pub vision: Option<Arc<dyn DynChatBackend>>,
    /// Permission policy consulted before every non-observation tool
    /// call. Default policy denies nothing and asks for nothing —
    /// matches the legacy behaviour.
    pub permissions: PermissionPolicy,
    /// Directory for completion-verification artifacts (PNG + JSON).
    /// `None` disables artifact persistence.
    pub verification_artifacts_dir: Option<PathBuf>,
    /// Monotonic counter feeding the `completion_verification_<n>.{png,json}`
    /// filename ordinal so repeated `verify_completion` calls within the
    /// same run do not overwrite each other. Mirrors the legacy
    /// `AgentRunner::verification_count` field.
    pub verification_count: u32,

    // --- CDP lifecycle bookkeeping (Task 3a.6) ---
    /// Shared CDP connection state — identical to the legacy field on the
    /// old `AgentRunner`. Populated when [`Self::auto_connect_cdp`]
    /// succeeds and consulted by [`Self::should_skip_focus_window`] and
    /// `verify_completion` so the completion-verification screenshot
    /// targets the right window.
    pub(crate) cdp_state: crate::cdp_lifecycle::CdpState,
    /// Per-app `kind` hint learned from a structured MCP response
    /// (`focus_window` / `launch_app` with `{"kind": "..."}`). Populated
    /// before the CDP decision runs so subsequent `focus_window` calls
    /// can be suppressed when AX / CDP dispatch is available. Mirrors
    /// the legacy `AgentRunner::known_app_kinds` field.
    pub(crate) known_app_kinds: HashMap<String, String>,
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
            event_tx: None,
            approval_gate: None,
            vision: None,
            permissions: PermissionPolicy::default(),
            verification_artifacts_dir: None,
            verification_count: 0,
            cdp_state: crate::cdp_lifecycle::CdpState::new(),
            known_app_kinds: HashMap::new(),
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

    /// Attach a shared `RunStorage` so boundary `StepRecord`s land in the
    /// execution-level `events.jsonl`. Storage is optional: the runner
    /// still runs end-to-end without a handle — snapshots just become
    /// no-ops.
    pub fn with_storage(
        mut self,
        storage: std::sync::Arc<std::sync::Mutex<clickweave_core::storage::RunStorage>>,
    ) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Attach a live event channel. Events emitted by the runner are
    /// forwarded through this sender; `None` runs silently.
    pub fn with_events(mut self, tx: mpsc::Sender<AgentEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Attach an approval-gate channel. The runner sends
    /// `(ApprovalRequest, oneshot::Sender<bool>)` pairs through it and
    /// waits for a reply before dispatching every approval-gated tool.
    pub fn with_approval(
        mut self,
        request_tx: mpsc::Sender<(ApprovalRequest, oneshot::Sender<bool>)>,
    ) -> Self {
        self.approval_gate = Some(crate::agent::approval::ApprovalGate { request_tx });
        self
    }

    /// Attach a VLM backend used to verify `agent_done` against a fresh
    /// screenshot (D-PR1: stored as `Arc<dyn DynChatBackend>` so the
    /// VLM can be a different concrete backend from the primary).
    pub fn with_vision(mut self, vlm: Arc<dyn DynChatBackend>) -> Self {
        self.vision = Some(vlm);
        self
    }

    /// Replace the default permission policy.
    pub fn with_permissions(mut self, policy: PermissionPolicy) -> Self {
        self.permissions = policy;
        self
    }

    /// Set the directory where completion-verification artifacts are
    /// persisted (PNG screenshot + JSON metadata).
    pub fn with_verification_artifacts_dir(mut self, dir: PathBuf) -> Self {
        self.verification_artifacts_dir = Some(dir);
        self
    }

    /// Consume the runner and return the accumulated [`AgentCache`].
    /// API parity with `AgentRunner::into_cache` — the Phase 3b cutover
    /// keeps `run_agent_workflow`'s `(AgentState, AgentCache)` contract.
    pub fn into_cache(self) -> AgentCache {
        self.cache
    }

    /// Write a boundary `StepRecord` through the shared `RunStorage` handle.
    /// Silently no-ops when no storage is attached or the lock is poisoned
    /// — persistence is best-effort, never fatal.
    pub fn write_step_record(&self, record: &crate::agent::step_record::StepRecord) {
        if let Some(s) = &self.storage
            && let Ok(guard) = s.lock()
        {
            let _ = guard.append_agent_event(record);
        }
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

    /// Borrow the current CDP bookkeeping. Used by `verify_completion`
    /// to target the screenshot scope at the connected window, and
    /// (in tests) to assert CDP auto-connect side effects.
    pub(crate) fn cdp_state(&self) -> &crate::cdp_lifecycle::CdpState {
        &self.cdp_state
    }

    /// Seed `known_app_kinds` directly. Test-only — the live flow
    /// populates this via [`Self::record_app_kind`] from the MCP
    /// response shape.
    #[cfg(test)]
    pub(crate) fn record_app_kind_for_test(&mut self, app_name: &str, kind: &str) {
        self.known_app_kinds
            .insert(app_name.to_string(), kind.to_string());
    }

    /// Seed the active CDP connection identity. Test-only — the live
    /// flow populates this through [`Self::auto_connect_cdp`].
    #[cfg(test)]
    pub(crate) fn set_cdp_connected_for_test(&mut self, app_name: &str, pid: i32) {
        self.cdp_state.set_connected(app_name, pid);
    }

    /// Test-only seed for the `(app_kind, cdp_connected)` state the
    /// runner would otherwise reach only after `launch_app` →
    /// `auto_connect_cdp` → `on_cdp_connected`. Used by integration
    /// tests that want to exercise the post-CDP-connect focus_window
    /// skip path without the full quit/relaunch/connect choreography.
    /// Port of the legacy `AgentRunner::seed_cdp_live_for_test` for
    /// 3a.7.b test migration.
    #[cfg(test)]
    pub(crate) fn seed_cdp_live_for_test(&mut self, app_name: &str, kind: &str) {
        self.record_app_kind(app_name, kind);
        self.cdp_state.set_connected(app_name, 0);
    }

    /// Public-for-tests view of `cdp_state`. Keeps the field private
    /// outside the module while letting integration tests assert the
    /// post-tool auto-connect bookkeeping.
    #[cfg(test)]
    pub(crate) fn cdp_state_for_test(&self) -> &crate::cdp_lifecycle::CdpState {
        &self.cdp_state
    }

    /// Test-only entry point into the selected-page snapshot helper so
    /// the agent-vs-executor parity suite can exercise exactly the code
    /// path the live run would hit, rather than poking fields. Ported
    /// from the legacy `AgentRunner::snapshot_selected_page_url_for_test`
    /// for 3a.7.a test migration.
    #[cfg(test)]
    pub(crate) async fn snapshot_selected_page_url_for_test(
        &mut self,
        app_name: &str,
        pid: i32,
        mcp: &(impl crate::executor::Mcp + ?Sized),
    ) {
        crate::cdp_lifecycle::snapshot_selected_page_url(mcp, &mut self.cdp_state, app_name, pid)
            .await;
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

    /// Apply the batch of task-state mutations from an `AgentTurn`, in
    /// order. Invalid mutations become warnings but do not abort the pass —
    /// subsequent mutations and the action still run. Matches the
    /// error-path table in the spec.
    pub fn apply_mutations(&mut self, muts: &[TaskStateMutation]) -> Vec<String> {
        let mut warnings = Vec::new();
        for m in muts {
            if let Err(e) = self.task_state.apply(m, self.step_index) {
                warnings.push(format!("{}", e));
            }
        }
        warnings
    }

    /// Rewrite raw AX uid references in a workflow node into replay-stable
    /// `AxTarget::Descriptor` payloads using the current
    /// `last_native_ax_snapshot` body. Port of the legacy
    /// `enrich_ax_descriptor` helper — D15 moves the source of truth off
    /// the transcript onto `WorldModel`.
    ///
    /// No-op when no native AX snapshot has been captured yet, when the
    /// node type is not an AX dispatch variant, when the target is already
    /// a `Descriptor`, or when the uid is not present in the snapshot.
    pub fn enrich_ax_descriptor(&self, node_type: &mut clickweave_core::NodeType) {
        use clickweave_core::{AxTarget, NodeType};

        let Some(ax) = &self.world_model.last_native_ax_snapshot else {
            return;
        };

        let target: &mut AxTarget = match node_type {
            NodeType::AxClick(p) => &mut p.target,
            NodeType::AxSetValue(p) => &mut p.target,
            NodeType::AxSelect(p) => &mut p.target,
            _ => return,
        };

        let uid = match target {
            AxTarget::ResolvedUid(uid) if !uid.is_empty() => uid.clone(),
            _ => return,
        };

        let parsed = crate::agent::world_model::parse_ax_snapshot(&ax.value.ax_tree_text);
        let Some(entry) = parsed.into_iter().find(|e| e.uid == uid) else {
            return;
        };
        *target = AxTarget::Descriptor {
            role: entry.role,
            name: entry.name.unwrap_or_default(),
            parent_name: entry.parent_name,
        };
    }

    /// Build a workflow node for the executed tool call. Returns the UUID of
    /// the new node, or `None` when the tool is observation-only, when
    /// workflow-graph building is disabled via `config.build_workflow`, or
    /// when the tool-to-[`clickweave_core::NodeType`] mapping fails.
    ///
    /// On success the node is pushed onto `state.workflow.nodes`, an
    /// `AgentEvent::NodeAdded` fires, and — when a prior node exists —
    /// an edge from the previous node to this one is pushed onto
    /// `state.workflow.edges` with a matching `AgentEvent::EdgeAdded`. The
    /// first node in a run is chained from `state.last_node_id`, which the
    /// top-level loop seeds from the caller-provided `anchor_node_id` so the
    /// first tool call is linked to the prior workflow graph when one is
    /// supplied. Every node is stamped with `source_run_id: self.run_id`.
    ///
    /// Port of the legacy `AgentRunner::add_workflow_node`.
    pub async fn add_workflow_node(
        &mut self,
        tool_name: &str,
        arguments: &Value,
        known_tools: &[Value],
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
    ) -> Option<uuid::Uuid> {
        use clickweave_core::{Node, Position, tool_mapping::tool_invocation_to_node_type};

        if !self.config.build_workflow {
            return None;
        }
        if is_observation_tool(tool_name, annotations_by_tool) {
            return None;
        }

        let mut node_type = match tool_invocation_to_node_type(tool_name, arguments, known_tools) {
            Ok(nt) => nt,
            Err(e) => {
                warn!(
                    error = %e,
                    tool = tool_name,
                    "state-spine: could not map tool to workflow node type — workflow graph will be incomplete"
                );
                self.emit_event(AgentEvent::Warning {
                    message: format!("Failed to map tool '{}' to workflow node: {}", tool_name, e),
                })
                .await;
                return None;
            }
        };

        // AX dispatch descriptor enrichment. The tool-mapping inbound path
        // writes `AxTarget::ResolvedUid(uid)`; upgrade to `Descriptor`
        // against the most recent native AX snapshot so the node replays
        // correctly after a fresh snapshot (different generation id).
        self.enrich_ax_descriptor(&mut node_type);

        let position = Position {
            x: 0.0,
            y: (self.state.workflow.nodes.len() as f32) * 120.0,
        };
        let node = Node::new(node_type, position, tool_name, "").with_run_id(self.run_id);
        let node_id = node.id;

        // Emit the live NodeAdded event before mutating the workflow so
        // subscribers observe creation order that matches the event stream.
        self.emit_event(AgentEvent::NodeAdded {
            node: Box::new(node.clone()),
        })
        .await;
        self.state.workflow.nodes.push(node);

        // Chain from the previous node (or the caller-supplied anchor on the
        // first iteration).
        if let Some(prev_id) = self.state.last_node_id {
            let edge = clickweave_core::Edge {
                from: prev_id,
                to: node_id,
            };
            self.emit_event(AgentEvent::EdgeAdded { edge: edge.clone() })
                .await;
            self.state.workflow.edges.push(edge);
        }

        self.state.last_node_id = Some(node_id);
        Some(node_id)
    }

    /// After a successful tool call, refresh the world model's identity
    /// fields that the tool just captured. Non-snapshot tools are no-ops.
    pub fn update_continuity_after_tool_success(&mut self, tool_name: &str, body: &str) {
        use crate::agent::world_model::{
            AxSnapshotData, Fresh, FreshnessSource, ScreenshotRef, parse_ax_snapshot,
        };
        match tool_name {
            "take_ax_snapshot" => {
                let parsed = parse_ax_snapshot(body);
                let snapshot_id = parsed
                    .first()
                    .map(|e| e.uid.clone())
                    .unwrap_or_else(|| format!("ax-{}", self.step_index));
                self.world_model.last_native_ax_snapshot = Some(Fresh {
                    value: AxSnapshotData {
                        snapshot_id,
                        element_count: parsed.len(),
                        captured_at_step: self.step_index,
                        ax_tree_text: body.to_string(),
                    },
                    written_at: self.step_index,
                    source: FreshnessSource::DirectObservation,
                    ttl_steps: Some(8),
                });
            }
            "take_screenshot" => {
                let id = serde_json::from_str::<serde_json::Value>(body)
                    .ok()
                    .and_then(|v| {
                        v.get("screenshot_id")
                            .and_then(|s| s.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_else(|| format!("ss-{}", self.step_index));
                self.world_model.last_screenshot = Some(Fresh {
                    value: ScreenshotRef {
                        screenshot_id: id,
                        captured_at_step: self.step_index,
                    },
                    written_at: self.step_index,
                    source: FreshnessSource::DirectObservation,
                    ttl_steps: Some(8),
                });
            }
            _ => {}
        }
    }

    /// Fetch interactive elements from the current page via MCP.
    ///
    /// Port of `AgentRunner::fetch_elements`: calls `cdp_find_elements` when
    /// the tool is available, parses the response into `CdpFindElementMatch`es,
    /// updates `state.current_url`, and returns the parsed matches. Errors and
    /// missing-tool paths return an empty vec so the rest of the loop degrades
    /// gracefully. A serde parse failure (schema drift between server and
    /// engine) surfaces as an `AgentEvent::Warning` — a genuinely empty page
    /// and a wire-format drift look identical from the agent's perspective, so
    /// the operator needs the explicit signal.
    pub(crate) async fn fetch_elements<M: Mcp + ?Sized>(
        &mut self,
        mcp: &M,
    ) -> Vec<clickweave_core::cdp::CdpFindElementMatch> {
        if !mcp.has_tool("cdp_find_elements") {
            return Vec::new();
        }
        match mcp
            .call_tool(
                "cdp_find_elements",
                Some(serde_json::json!({"query": "", "max_results": 300})),
            )
            .await
        {
            Ok(result) if result.is_error != Some(true) => {
                let text = crate::cdp_lifecycle::extract_text(&result);
                match serde_json::from_str::<clickweave_core::cdp::CdpFindElementsResponse>(&text) {
                    Ok(parsed) => {
                        self.state.current_url = parsed.page_url;
                        return parsed.matches;
                    }
                    Err(parse_err) => {
                        tracing::debug!(
                            error = %parse_err,
                            "state-spine: failed to parse cdp_find_elements response"
                        );
                        self.emit_event(AgentEvent::Warning {
                            message: format!(
                                "cdp_find_elements response failed to parse: {} — continuing with empty elements",
                                parse_err
                            ),
                        })
                        .await;
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(error = %e, "state-spine: cdp_find_elements call failed");
            }
        }
        Vec::new()
    }

    /// Build a terminal `StepRecord` for a completed / halted run. Used by
    /// the control loop on run-end boundaries and by integration tests.
    pub fn build_step_record(
        &self,
        boundary_kind: crate::agent::step_record::BoundaryKind,
        action_taken: serde_json::Value,
        outcome: serde_json::Value,
    ) -> crate::agent::step_record::StepRecord {
        use crate::agent::step_record::{StepRecord, WorldModelSnapshot};
        StepRecord {
            step_index: self.step_index,
            boundary_kind,
            world_model_snapshot: WorldModelSnapshot::from_world_model(&self.world_model),
            task_state_snapshot: self.task_state.clone(),
            action_taken,
            outcome,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Persist one `BoundaryKind::SubgoalCompleted` record per milestone
    /// appended during the current turn (D8). Called from
    /// [`Self::run`] right after `run_turn` reports `milestones_appended >
    /// 0`. Records the turn's batched mutations as `action_taken` so the
    /// subgoal summaries are recoverable from `events.jsonl` without a
    /// separate transcript lookup. Emits one
    /// `AgentEvent::BoundaryRecordWritten` per persisted record (D17).
    async fn write_subgoal_completed_records(&self, count: usize, turn: &AgentTurn) {
        let action_taken =
            serde_json::to_value(&turn.mutations).unwrap_or_else(|_| serde_json::json!([]));
        for _ in 0..count {
            self.persist_boundary_record(
                crate::agent::step_record::BoundaryKind::SubgoalCompleted,
                action_taken.clone(),
                serde_json::json!({"kind": "subgoal_completed"}),
            )
            .await;
        }
    }

    /// Persist one `BoundaryKind::RecoverySucceeded` record on the exact
    /// `Recovering -> Executing` transition (D8). Called from
    /// [`Self::run`] when a tool success cleared the consecutive-error
    /// streak. `action_taken` / `outcome` record the successful turn so
    /// Spec 2's episodic memory can reason about what resolved the
    /// recovery. Emits one `AgentEvent::BoundaryRecordWritten` (D17).
    async fn write_recovery_succeeded_record(&self, turn: &AgentTurn, outcome: &TurnOutcome) {
        let action_taken =
            serde_json::to_value(&turn.action).unwrap_or_else(|_| serde_json::json!({}));
        let outcome_json = match outcome {
            TurnOutcome::ToolSuccess {
                tool_name,
                tool_body,
            } => serde_json::json!({
                "kind": "tool_success",
                "tool_name": tool_name,
                "body_len": tool_body.len(),
            }),
            // RecoverySucceeded is only written on ToolSuccess; the other
            // variants never reach this path (see `run()`'s guard).
            _ => serde_json::json!({"kind": "tool_success"}),
        };
        self.persist_boundary_record(
            crate::agent::step_record::BoundaryKind::RecoverySucceeded,
            action_taken,
            outcome_json,
        )
        .await;
    }

    /// Persist the single `BoundaryKind::Terminal` record at run end (D8).
    /// Called exactly once from [`Self::run`] after the control loop has
    /// populated `state.terminal_reason`. Encodes the terminal reason into
    /// the outcome payload so the record is self-describing without a
    /// cross-reference to the rest of `events.jsonl`. Emits one
    /// `AgentEvent::BoundaryRecordWritten` (D17).
    async fn write_terminal_record(&self) {
        let terminal_reason = self.state.terminal_reason.as_ref();
        let outcome_json = terminal_reason
            .map(|tr| serde_json::to_value(tr).unwrap_or_else(|_| serde_json::json!({})))
            .unwrap_or_else(|| serde_json::json!({"kind": "unknown"}));
        // Best-effort action_taken: a minimal projection of the last
        // recorded step (tool_name only — `AgentCommand` itself isn't
        // `Serialize`). Falls back to the outcome for zero-step runs.
        let action_taken = self
            .state
            .steps
            .last()
            .map(|step| {
                serde_json::json!({
                    "tool_name": step.command.tool_name_or_unknown(),
                    "step_index": step.index,
                })
            })
            .unwrap_or_else(|| outcome_json.clone());
        self.persist_boundary_record(
            crate::agent::step_record::BoundaryKind::Terminal,
            action_taken,
            outcome_json,
        )
        .await;
    }

    /// Shared body for the three `write_*_record` boundary paths: build
    /// the `StepRecord`, persist via `RunStorage`, and emit the matching
    /// `BoundaryRecordWritten` event.
    async fn persist_boundary_record(
        &self,
        boundary_kind: crate::agent::step_record::BoundaryKind,
        action_taken: serde_json::Value,
        outcome: serde_json::Value,
    ) {
        let record = self.build_step_record(boundary_kind.clone(), action_taken, outcome);
        self.write_step_record(&record);
        self.emit_event(AgentEvent::BoundaryRecordWritten {
            run_id: self.run_id,
            boundary_kind,
            step_index: record.step_index,
        })
        .await;
    }
}

/// Outcome of a single `StateRunner::run_turn` call — what the caller needs
/// to drive the next iteration.
#[derive(Debug, Clone)]
pub enum TurnOutcome {
    /// Tool call was dispatched; `tool_body` is the successful result text.
    ToolSuccess {
        tool_name: String,
        tool_body: String,
    },
    /// Tool call was dispatched; tool returned an error.
    ToolError { tool_name: String, error: String },
    /// Agent signaled completion.
    Done { summary: String },
    /// Agent requested replan.
    Replan { reason: String },
}

/// Executes an MCP tool call and returns either its successful body or an
/// error message. Integration tests stub this with a deterministic sequence;
/// Phase 3 cutover will bind it to the real `McpClient`.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn call_tool(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<String, String>;
}

impl StateRunner {
    /// Apply one `AgentTurn` in the state-spine control flow:
    ///
    /// 1. Apply mutations in order (errors become warnings, not fatal).
    /// 2. Observe (absorb any queued invalidation events + re-infer phase).
    /// 3. Dispatch the action:
    ///     - `ToolCall`: call the executor, update continuity on success,
    ///       queue `ToolFailed` and bump `consecutive_errors` on error.
    ///     - `AgentDone` / `AgentReplan`: return the terminal outcome.
    /// 4. Advance `step_index`.
    ///
    /// Integration tests drive this with deterministic `AgentTurn`s; Phase 3
    /// will wrap this with the LLM loop + compaction + cache replay.
    ///
    /// Return tuple: `(outcome, warnings, milestones_appended)`.
    /// `milestones_appended` counts the number of `CompleteSubgoal`
    /// mutations that successfully popped a subgoal off the stack during
    /// this turn. The control loop in [`StateRunner::run`] uses this count
    /// to emit one `BoundaryKind::SubgoalCompleted` `StepRecord` per
    /// appended milestone (Task 3a.6.5 / D8).
    pub async fn run_turn<E: ToolExecutor + ?Sized>(
        &mut self,
        turn: &AgentTurn,
        executor: &E,
    ) -> (TurnOutcome, Vec<String>, usize) {
        // 1. Apply mutations first — phase inference reads the stack/watch state.
        //    Count successful `CompleteSubgoal` mutations by diffing the
        //    milestones vec length (each `CompleteSubgoal` that passes
        //    validation appends exactly one `Milestone`; see
        //    `TaskState::apply`). Milestones don't shrink during normal
        //    operation, so the delta is an exact count of new milestones.
        let milestones_before = self.task_state.milestones.len();
        let warnings = self.apply_mutations(&turn.mutations);
        let milestones_appended = self
            .task_state
            .milestones
            .len()
            .saturating_sub(milestones_before);

        // 1a. Emit `TaskStateChanged` once per turn when `apply_mutations`
        //     had anything to apply (D17). The event reflects the full
        //     post-mutation state so subscribers never have to reassemble
        //     it from the warnings vec.
        if !turn.mutations.is_empty() {
            self.emit_event(AgentEvent::TaskStateChanged {
                run_id: self.run_id,
                task_state: self.task_state.clone(),
            })
            .await;
        }

        // 2. Observe: snapshot field signatures → drain pending events +
        //    re-infer phase → compute diff → emit `WorldModelChanged` (D17).
        let pre_signatures = self.world_model.field_signatures();
        self.observe();
        let post_signatures = self.world_model.field_signatures();
        let diff = diff_world_model_signatures(&pre_signatures, &post_signatures);
        self.emit_event(AgentEvent::WorldModelChanged {
            run_id: self.run_id,
            diff,
        })
        .await;

        // 3. Dispatch action.
        let outcome = match &turn.action {
            AgentAction::ToolCall {
                tool_name,
                arguments,
                ..
            } => match executor.call_tool(tool_name, arguments).await {
                Ok(body) => {
                    self.update_continuity_after_tool_success(tool_name, &body);
                    self.consecutive_errors = 0;
                    TurnOutcome::ToolSuccess {
                        tool_name: tool_name.clone(),
                        tool_body: body,
                    }
                }
                Err(error) => {
                    self.consecutive_errors += 1;
                    self.queue_invalidation(InvalidationEvent::ToolFailed {
                        tool: tool_name.clone(),
                    });
                    TurnOutcome::ToolError {
                        tool_name: tool_name.clone(),
                        error,
                    }
                }
            },
            AgentAction::AgentDone { summary } => TurnOutcome::Done {
                summary: summary.clone(),
            },
            AgentAction::AgentReplan { reason } => {
                self.last_replan_step = Some(self.step_index);
                TurnOutcome::Replan {
                    reason: reason.clone(),
                }
            }
        };

        // 4. Advance.
        self.step_index += 1;

        (outcome, warnings, milestones_appended)
    }
}

/// Control signal returned from [`StateRunner::try_replay_cache`].
///
/// Mirrors the legacy `ReplayResult` semantics: `Continue` means the
/// replay handled this iteration (success, policy-reject, or approval-
/// reject) and the outer loop should `continue`; `Break` means a terminal
/// condition was reached (approval unavailable, max-errors, destructive
/// cap); `FellThrough` means the LLM path should run this iteration.
enum ReplayResult {
    Continue,
    Break,
    FellThrough,
}

/// Result of requesting user approval for a tool action. Shared by both
/// the cache-replay path (Task 3a.2) and the live dispatch path
/// (Task 3a.3) — the only behavioural difference between the two is the
/// `" (cached)"` suffix in the human-facing description, enforced by the
/// single helper [`StateRunner::request_approval`] below.
enum ApprovalResult {
    Approved,
    Rejected,
    Unavailable,
}

/// State of the consecutive-destructive-tool cap after a tool call.
/// Mirrors the legacy `CapStatus` — private to `runner.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapStatus {
    /// Streak is still below the cap — run continues normally.
    Armed,
    /// Cap reached — the caller must emit the cap-hit event and halt.
    CapReached,
}

/// Why a `focus_window` MCP call was suppressed by the runner. Ported
/// verbatim from the legacy `FocusSkipReason`.
///
/// The LLM sees a synthetic `StepOutcome::Success` whose text comes from
/// [`FocusSkipReason::llm_message`]; that text must stay byte-identical to
/// the legacy strings so replay / transcript tests still pin the same
/// contract.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum FocusSkipReason {
    /// macOS Native target, full AX dispatch toolset available —
    /// AX dispatch is focus-preserving so the real call is redundant.
    AxAvailable,
    /// Electron / Chrome target with a live CDP session and the minimum
    /// CDP dispatch toolset — CDP operates on backgrounded windows.
    CdpLive,
    /// Operator flipped [`AgentConfig::allow_focus_window`] to `false`;
    /// every focus_window is dropped regardless of kind or toolset.
    PolicyDisabled,
}

impl FocusSkipReason {
    const ALL: [Self; 3] = [Self::AxAvailable, Self::CdpLive, Self::PolicyDisabled];

    /// Result text returned to the LLM in the synthetic
    /// `StepOutcome::Success`. Must not drift from the strings the tests
    /// pin — they encode the agent→LLM skip-contract.
    pub(crate) const fn llm_message(self) -> &'static str {
        match self {
            Self::AxAvailable => {
                "skipped focus_window: AX tools available; window focus not required"
            }
            Self::CdpLive => "skipped focus_window: CDP already live; focus not required",
            Self::PolicyDisabled => {
                "focus_window skipped: agent policy disallows focus changes. Use AX dispatch \
                 (ax_click/ax_set_value/ax_select) or CDP (cdp_click/cdp_fill) instead — \
                 these operate on background windows."
            }
        }
    }

    /// Terse summary for the `SubAction` event surface.
    pub(crate) const fn sub_action_summary(self) -> &'static str {
        match self {
            Self::AxAvailable => "skipped: AX dispatch available",
            Self::CdpLive => "skipped: CDP already live; focus not required",
            Self::PolicyDisabled => "skipped: focus_window disabled by agent policy",
        }
    }

    /// Recover the variant from an LLM-visible result text. Used by the
    /// post-step bookkeeping predicate to keep synthetic skips invisible
    /// to CDP auto-connect and workflow-node creation.
    pub(crate) fn from_llm_message(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.llm_message() == text)
    }
}

/// macOS AX-dispatch toolset — every tool required for the
/// focus-preserving automation path. When the MCP server advertises
/// **all** of these plus `take_ax_snapshot`, the agent can drive native
/// apps without moving the cursor or raising windows, which makes a
/// preceding `focus_window` call redundant (and focus-stealing).
///
/// Mirrors the legacy `AX_DISPATCH_TOOLSET` byte-for-byte.
const AX_DISPATCH_TOOLSET: &[&str] = &["take_ax_snapshot", "ax_click", "ax_set_value", "ax_select"];

/// Minimum CDP dispatch toolset required before the runner may suppress
/// a `focus_window` against an Electron / Chrome-browser target. Kept
/// conservative: `cdp_find_elements` + `cdp_click` is enough to prove
/// the agent's next move will operate against the CDP target (all CDP
/// operations are focus-preserving). Servers missing these tools fall
/// through to the real `focus_window`.
///
/// Mirrors the legacy `CDP_DISPATCH_TOOLSET` byte-for-byte.
const CDP_DISPATCH_TOOLSET: &[&str] = &["cdp_find_elements", "cdp_click"];

/// True when every member of `toolset` is advertised by `mcp`.
fn mcp_has_toolset<M: Mcp + ?Sized>(mcp: &M, toolset: &[&str]) -> bool {
    toolset.iter().all(|name| mcp.has_tool(name))
}

/// Observation tools whose cached entries are stale on read. Mirrors
/// the legacy `OBSERVATION_TOOLS` list — duplicated here because the
/// legacy list was a private `const` on `AgentRunner`, and lifting it to
/// a shared module is out of scope for Task 3a.2 (refactoring pass
/// owned by 3b).
const OBSERVATION_TOOLS: &[&str] = &[
    "take_screenshot",
    "list_apps",
    "list_windows",
    "find_text",
    "find_image",
    "element_at_point",
    "take_ax_snapshot",
    "probe_app",
    "get_displays",
    "start_recording",
    "start_hover_tracking",
    "load_image",
    "cdp_list_pages",
    "cdp_take_snapshot",
    "cdp_find_elements",
    "android_list_devices",
];

/// AX dispatch tools whose cached uid goes stale on every
/// `take_ax_snapshot`. See the legacy `AX_DISPATCH_TOOLS`.
const AX_DISPATCH_TOOLS: &[&str] = &["ax_click", "ax_set_value", "ax_select"];

/// Tools that transition app / window / CDP state. Their cache key
/// reflects the pre-state, so replay would fire the transition a second
/// time on unchanged elements. See the legacy `STATE_TRANSITION_TOOLS`.
const STATE_TRANSITION_TOOLS: &[&str] = &[
    "launch_app",
    "focus_window",
    "quit_app",
    "cdp_connect",
    "cdp_disconnect",
];

/// True when the tool is observation-only — either hardcoded in
/// [`OBSERVATION_TOOLS`] or annotated with `readOnlyHint = true`. The
/// `CONFIRMABLE_TOOLS` carve-out (`launch_app` / `quit_app` / `cdp_connect`)
/// takes precedence so destructive side effects stay gated.
// `pub(crate)` so the ported `observation_union_tests` in
// `crate::agent::world_model` can exercise the predicate directly without
// routing through `StateRunner::classify_tool_result` (Task 3a.7.d).
pub(crate) fn is_observation_tool(
    tool_name: &str,
    annotations_by_tool: &HashMap<String, ToolAnnotations>,
) -> bool {
    if clickweave_core::permissions::CONFIRMABLE_TOOLS
        .iter()
        .any(|(n, _)| *n == tool_name)
    {
        return false;
    }
    if OBSERVATION_TOOLS.contains(&tool_name) {
        return true;
    }
    annotations_by_tool
        .get(tool_name)
        .and_then(|a| a.read_only_hint)
        .unwrap_or(false)
}

// `pub(crate)` so the ported `observation_union_tests` in
// `crate::agent::world_model` can verify cache-eligibility without reaching
// through `StateRunner`'s private API (Task 3a.7.d).
pub(crate) fn is_ax_dispatch_tool(tool_name: &str) -> bool {
    AX_DISPATCH_TOOLS.contains(&tool_name)
}

// Same rationale as `is_ax_dispatch_tool` — exposed to the `world_model`-
// hosted port of `observation_union_tests` (Task 3a.7.d).
pub(crate) fn is_state_transition_tool(tool_name: &str) -> bool {
    STATE_TRANSITION_TOOLS.contains(&tool_name)
}

/// Build an index from tool name → MCP annotations from the openai-
/// shaped tool list. Tools without an `annotations` block produce the
/// default (all-`None`) struct. Mirrors the legacy `build_annotations_index`.
fn build_annotations_index(mcp_tools: &[Value]) -> HashMap<String, ToolAnnotations> {
    mcp_tools
        .iter()
        .filter_map(|tool| {
            let name = tool
                .get("function")
                .and_then(|f| f.get("name"))
                .or_else(|| tool.get("name"))
                .and_then(Value::as_str)?;
            Some((name.to_string(), ToolAnnotations::from_tool_json(tool)))
        })
        .collect()
}

/// Join all text content from a `ToolCallResult` into a single string —
/// this is the body the LLM sees in the `tool_result` message.
// `pub(crate)` so the ported `observation_union_tests` in
// `crate::agent::world_model` can pin the joined-text contract (Task 3a.7.d).
pub(crate) fn extract_result_text(result: &clickweave_mcp::ToolCallResult) -> String {
    result
        .content
        .iter()
        .map(|content| match content {
            clickweave_mcp::ToolContent::Text { text } => text.clone(),
            clickweave_mcp::ToolContent::Image { mime_type, .. } => {
                format!("[image: {}]", mime_type)
            }
            clickweave_mcp::ToolContent::Unknown(_) => "[unknown content]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Pure diff over two `WorldModel::field_signatures()` snapshots.
///
/// Returns a `WorldModelDiff` whose `changed_fields` lists — in the
/// order `field_signatures` emits them — every field name whose
/// signature differs between `pre` and `post`. Used by `run_turn` to
/// emit `AgentEvent::WorldModelChanged` once per step after `observe`.
///
/// Panics only in the programmer-error case where `pre` and `post`
/// disagree on field ordering or length; `WorldModel::field_signatures`
/// is deterministic so this should never happen at runtime.
pub(crate) fn diff_world_model_signatures(
    pre: &[(&'static str, Option<usize>)],
    post: &[(&'static str, Option<usize>)],
) -> WorldModelDiff {
    debug_assert_eq!(
        pre.len(),
        post.len(),
        "field_signatures must return a stable-length vec",
    );
    let changed_fields = pre
        .iter()
        .zip(post.iter())
        .filter_map(|(p, q)| {
            debug_assert_eq!(
                p.0, q.0,
                "field_signatures must return fields in the same order",
            );
            (p.1 != q.1).then(|| p.0.to_string())
        })
        .collect();
    WorldModelDiff { changed_fields }
}

impl StateRunner {
    /// Best-effort send of an [`AgentEvent`] through the configured
    /// channel. No-op when the channel is unset or closed — event
    /// emission must never fail the run.
    async fn emit_event(&self, event: AgentEvent) {
        let Some(tx) = &self.event_tx else { return };
        if tx.is_closed() {
            return;
        }
        if let Err(e) = tx.send(event).await {
            warn!("state-spine: failed to emit agent event (channel closed): {e}");
        }
    }

    /// Update the consecutive-destructive-call tracker after a successful
    /// tool call, and report whether the cap has now been hit. Port of
    /// the legacy `AgentRunner::maybe_halt_on_destructive_cap`.
    ///
    /// `destructive_hint == Some(true)` increments the streak; anything else
    /// resets it. A cap value of `0` disables the feature entirely, so the
    /// method always returns `CapStatus::Armed` in that case.
    fn maybe_halt_on_destructive_cap(
        &mut self,
        tool_name: &str,
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
    ) -> CapStatus {
        if self.config.consecutive_destructive_cap == 0 {
            return CapStatus::Armed;
        }
        let destructive = annotations_by_tool
            .get(tool_name)
            .and_then(|a| a.destructive_hint)
            .unwrap_or(false);
        if destructive {
            self.state
                .recent_destructive_tools
                .push(tool_name.to_string());
        } else {
            self.state.recent_destructive_tools.clear();
        }
        if self.state.recent_destructive_tools.len() >= self.config.consecutive_destructive_cap {
            CapStatus::CapReached
        } else {
            CapStatus::Armed
        }
    }

    /// Halt the run because the consecutive-destructive cap was reached.
    /// Emits the cap-hit event and sets the terminal reason. Called once
    /// when `maybe_halt_on_destructive_cap` reports `CapStatus::CapReached`.
    /// Clears `recent_destructive_tools` afterwards so state serialization
    /// reflects the drained streak. Port of the legacy
    /// `AgentRunner::emit_destructive_cap_hit`.
    async fn emit_destructive_cap_hit(&mut self) {
        let recent = std::mem::take(&mut self.state.recent_destructive_tools);
        let cap = self.config.consecutive_destructive_cap;
        warn!(
            cap,
            tools = ?recent,
            "state-spine: consecutive destructive cap reached — halting run"
        );
        self.emit_event(AgentEvent::ConsecutiveDestructiveCapHit {
            recent_tool_names: recent.clone(),
            cap,
        })
        .await;
        self.state.terminal_reason = Some(TerminalReason::ConsecutiveDestructiveCap {
            recent_tool_names: recent,
            cap,
        });
    }

    // -----------------------------------------------------------------
    // Task 3a.6: CDP auto-connect + synthetic focus_window skip
    // -----------------------------------------------------------------

    /// Record a per-app `kind` hint learned from a structured MCP
    /// response or `probe_app`. Port of the legacy
    /// `AgentRunner::record_app_kind`.
    fn record_app_kind(&mut self, app_name: &str, kind: &str) {
        self.known_app_kinds
            .insert(app_name.to_string(), kind.to_string());
    }

    /// True when `(tool_name, result_text)` identifies a runner-skipped
    /// `focus_window` — one of the synthetic successes that
    /// [`Self::should_skip_focus_window`] emits. Post-step bookkeeping
    /// (CDP auto-connect, workflow-node creation) consults this so the
    /// skipped call stays invisible to both the CDP lifecycle and the
    /// graph. Port of `AgentRunner::is_synthetic_focus_skip`.
    pub(crate) fn is_synthetic_focus_skip(tool_name: &str, result_text: &str) -> bool {
        tool_name == "focus_window" && FocusSkipReason::from_llm_message(result_text).is_some()
    }

    /// Decide whether to suppress a `focus_window` MCP call. Returns a
    /// [`FocusSkipReason`] in three cases: (1) operator set
    /// `allow_focus_window = false`, (2) Native app with full AX
    /// dispatch toolset, (3) Electron / Chrome with a live CDP session
    /// and the minimum CDP dispatch toolset. Otherwise `None` —
    /// fall-through to the real MCP call.
    ///
    /// Port of the legacy `AgentRunner::should_skip_focus_window`.
    fn should_skip_focus_window<M: Mcp + ?Sized>(
        &self,
        arguments: &Value,
        mcp: &M,
    ) -> Option<FocusSkipReason> {
        // User-policy short-circuit takes precedence over kind / toolset
        // checks — the operator explicitly asked for "no focus changes,
        // ever".
        if !self.config.allow_focus_window {
            return Some(FocusSkipReason::PolicyDisabled);
        }
        let app_name = arguments.get("app_name").and_then(Value::as_str)?;
        match self.known_app_kinds.get(app_name).map(String::as_str) {
            Some("Native") if mcp_has_toolset(mcp, AX_DISPATCH_TOOLSET) => {
                Some(FocusSkipReason::AxAvailable)
            }
            Some("ElectronApp" | "ChromeBrowser")
                if self.cdp_state.is_connected_to(app_name, 0)
                    && mcp_has_toolset(mcp, CDP_DISPATCH_TOOLSET) =>
            {
                Some(FocusSkipReason::CdpLive)
            }
            _ => None,
        }
    }

    /// Resolve the app identity for CDP probing from a successful
    /// `focus_window` / `launch_app` call. Returns `(app_name, kind)`
    /// where `kind` is a pre-classified `AppKind` string
    /// (`"ElectronApp"`, `"ChromeBrowser"`, `"Native"`) when the MCP
    /// server already told us. Port of the legacy
    /// `AgentRunner::resolve_cdp_target`.
    async fn resolve_cdp_target<M: Mcp + ?Sized>(
        arguments: &Value,
        result_text: &str,
        mcp: &M,
    ) -> Option<(String, Option<String>)> {
        // 1. Structured MCP response (modern focus_window / launch_app).
        if let Ok(parsed) = serde_json::from_str::<Value>(result_text)
            && let Some(name) = parsed.get("app_name").and_then(Value::as_str)
            && !name.is_empty()
        {
            let kind = parsed
                .get("kind")
                .and_then(Value::as_str)
                .map(str::to_owned);
            return Some((name.to_string(), kind));
        }
        // 2. Direct argument (fast, backwards-compatible).
        if let Some(name) = arguments["app_name"].as_str() {
            return Some((name.to_string(), None));
        }
        // 3. pid → list_apps fallback.
        if let Some(pid) = arguments["pid"].as_u64()
            && mcp.has_tool("list_apps")
        {
            match mcp
                .call_tool("list_apps", Some(serde_json::json!({})))
                .await
            {
                Ok(r) if r.is_error != Some(true) => {
                    let text = crate::cdp_lifecycle::extract_text(&r);
                    if let Ok(entries) = serde_json::from_str::<Vec<Value>>(&text)
                        && let Some(name) = entries.iter().find_map(|entry| {
                            if entry["pid"].as_u64() == Some(pid) {
                                entry["name"].as_str().map(str::to_owned)
                            } else {
                                None
                            }
                        })
                    {
                        return Some((name, None));
                    }
                    debug!(pid, "state-spine: list_apps returned no entry matching pid");
                }
                Ok(r) => {
                    debug!(
                        error = %extract_result_text(&r),
                        "state-spine: list_apps returned error during CDP app-name resolution",
                    );
                }
                Err(e) => {
                    debug!(error = %e, "state-spine: list_apps call failed during CDP app-name resolution");
                }
            }
        }
        // 4. window_id → list_windows fallback.
        if let Some(window_id) = arguments["window_id"].as_u64()
            && mcp.has_tool("list_windows")
        {
            match mcp
                .call_tool("list_windows", Some(serde_json::json!({})))
                .await
            {
                Ok(r) if r.is_error != Some(true) => {
                    let text = crate::cdp_lifecycle::extract_text(&r);
                    if let Ok(entries) = serde_json::from_str::<Vec<Value>>(&text)
                        && let Some(name) = entries.iter().find_map(|entry| {
                            if entry["id"].as_u64() == Some(window_id) {
                                entry["owner_name"]
                                    .as_str()
                                    .or_else(|| entry["name"].as_str())
                                    .map(str::to_owned)
                            } else {
                                None
                            }
                        })
                    {
                        return Some((name, None));
                    }
                    debug!(
                        window_id,
                        "state-spine: list_windows returned no entry matching window_id",
                    );
                }
                Ok(r) => {
                    debug!(
                        error = %extract_result_text(&r),
                        "state-spine: list_windows returned error during CDP app-name resolution",
                    );
                }
                Err(e) => {
                    debug!(
                        error = %e,
                        "state-spine: list_windows call failed during CDP app-name resolution",
                    );
                }
            }
        }
        None
    }

    /// Post-connect bookkeeping: mark `(app_name, 0)` as the active CDP
    /// target and record the currently-selected page URL. Port of the
    /// legacy `AgentRunner::on_cdp_connected`.
    async fn on_cdp_connected<M: Mcp + ?Sized>(&mut self, app_name: &str, _port: u16, mcp: &M) {
        self.cdp_state.set_connected(app_name, 0);
        crate::cdp_lifecycle::snapshot_selected_page_url(mcp, &mut self.cdp_state, app_name, 0)
            .await;
    }

    /// After a successful `launch_app` / `focus_window`, probe the app
    /// type and auto-connect CDP for Electron / Chrome targets. Returns
    /// `Some(port)` on success, `None` otherwise. Port of the legacy
    /// `AgentRunner::auto_connect_cdp`. Keeps best-effort semantics —
    /// every failure path logs and falls through.
    async fn auto_connect_cdp<M: Mcp + ?Sized>(
        &mut self,
        app_name: &str,
        kind_hint: Option<&str>,
        mcp: &M,
    ) -> Option<u16> {
        use crate::cdp_lifecycle;

        if !mcp.has_tool("cdp_connect") {
            return None;
        }

        // If the caller already classified the app, trust it and skip
        // the `probe_app` round-trip.
        let cdp_capable_from_hint = matches!(kind_hint, Some("ElectronApp" | "ChromeBrowser"));
        if !cdp_capable_from_hint {
            if matches!(kind_hint, Some("Native")) {
                debug!(
                    app = app_name,
                    "state-spine: kind hint says Native, skipping CDP"
                );
                return None;
            }
            if !mcp.has_tool("probe_app") {
                return None;
            }

            let probe_args = serde_json::json!({"app_name": app_name});
            self.emit_event(AgentEvent::SubAction {
                tool_name: "probe_app".to_string(),
                summary: format!("Auto: probing {} for CDP support", app_name),
            })
            .await;
            let probe_text = match mcp.call_tool("probe_app", Some(probe_args)).await {
                Ok(r) => {
                    self.emit_event(AgentEvent::SubAction {
                        tool_name: "probe_app".to_string(),
                        summary: format!("Auto: probed {} (ok)", app_name),
                    })
                    .await;
                    extract_result_text(&r)
                }
                Err(e) => {
                    self.emit_event(AgentEvent::SubAction {
                        tool_name: "probe_app".to_string(),
                        summary: format!("Auto: probe_app failed for {}: {}", app_name, e),
                    })
                    .await;
                    debug!(app = app_name, error = %e, "state-spine: probe_app failed, skipping CDP");
                    return None;
                }
            };

            if !probe_text.contains("ElectronApp") && !probe_text.contains("ChromeBrowser") {
                debug!(
                    app = app_name,
                    "state-spine: not an Electron/Chrome app, skipping CDP"
                );
                return None;
            }
        }

        tracing::info!(
            app = app_name,
            "state-spine: detected Electron/Chrome app, connecting CDP"
        );

        // Reuse an already-running debug port if we can find one.
        if let Some(port) = crate::executor::deterministic::cdp::existing_debug_port(app_name).await
        {
            tracing::info!(
                app = app_name,
                port,
                "state-spine: reusing existing debug port"
            );
            if cdp_lifecycle::connect_with_retries(mcp, port).await.is_ok() {
                self.on_cdp_connected(app_name, port, mcp).await;
                return Some(port);
            }
        }

        // Quit, relaunch with a debug port, then connect CDP.
        let port = clickweave_core::cdp::rand_ephemeral_port();

        self.emit_event(AgentEvent::SubAction {
            tool_name: "quit_app".to_string(),
            summary: format!("Auto: quitting {} for CDP relaunch", app_name),
        })
        .await;
        let quit_outcome = cdp_lifecycle::quit_and_wait(mcp, app_name, &mut self.cdp_state).await;
        let quit_summary = match quit_outcome {
            cdp_lifecycle::QuitOutcome::Graceful => format!("Auto: {} quit confirmed", app_name),
            cdp_lifecycle::QuitOutcome::TimedOut => {
                format!("Auto: {} did not quit gracefully, force-killing", app_name)
            }
        };
        self.emit_event(AgentEvent::SubAction {
            tool_name: "quit_app".to_string(),
            summary: quit_summary,
        })
        .await;

        if matches!(quit_outcome, cdp_lifecycle::QuitOutcome::TimedOut) {
            warn!(
                app = app_name,
                "state-spine: app did not quit gracefully, force-killing"
            );
            cdp_lifecycle::force_quit(mcp, app_name).await;
        }

        self.emit_event(AgentEvent::SubAction {
            tool_name: "launch_app".to_string(),
            summary: format!("Auto: relaunching {} with debug port {}", app_name, port),
        })
        .await;
        match cdp_lifecycle::launch_with_debug_port(mcp, app_name, port).await {
            Ok(()) => {
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "launch_app".to_string(),
                    summary: format!("Auto: relaunched {} (ok)", app_name),
                })
                .await;
            }
            Err(err) => {
                warn!(
                    app = app_name,
                    error = %err,
                    "state-spine: relaunch with debug port failed"
                );
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "launch_app".to_string(),
                    summary: format!("Auto: relaunch failed for {}: {}", app_name, err),
                })
                .await;
                let fallback = serde_json::json!({"app_name": app_name});
                crate::executor::deterministic::best_effort_tool_call(
                    mcp,
                    "launch_app",
                    Some(fallback),
                    "state-spine fallback relaunch (debug-port launch failed)",
                )
                .await;
                return None;
            }
        }

        cdp_lifecycle::warmup_after_relaunch().await;

        self.emit_event(AgentEvent::SubAction {
            tool_name: "cdp_connect".to_string(),
            summary: format!("Auto: connecting CDP on port {}", port),
        })
        .await;
        match cdp_lifecycle::connect_with_retries(mcp, port).await {
            Ok(()) => {
                tracing::info!(app = app_name, port, "state-spine: CDP connected");
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "cdp_connect".to_string(),
                    summary: format!("Auto: CDP connected on port {} (ok)", port),
                })
                .await;
                self.on_cdp_connected(app_name, port, mcp).await;
                Some(port)
            }
            Err(last_err) => {
                warn!(
                    app = app_name,
                    port,
                    error = %last_err,
                    "state-spine: CDP connection failed after retries",
                );
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "cdp_connect".to_string(),
                    summary: format!("Auto: CDP connect failed on port {}", port),
                })
                .await;
                None
            }
        }
    }

    /// Post-tool hook: after a successful `launch_app` / `focus_window`,
    /// auto-connect CDP and refresh the MCP tool-cache so observation
    /// gates see the newly-surfaced CDP tools. Also keeps `cdp_state`
    /// in lock-step with `quit_app`. Port of the legacy
    /// `AgentRunner::maybe_cdp_connect`.
    async fn maybe_cdp_connect<M: Mcp + ?Sized>(
        &mut self,
        tool_name: &str,
        arguments: &Value,
        result_text: &str,
        mcp: &M,
    ) {
        if tool_name != "launch_app" && tool_name != "focus_window" {
            // Keep CDP state in lock-step with the underlying process.
            if tool_name == "quit_app"
                && let Some(name) = arguments.get("app_name").and_then(Value::as_str)
            {
                self.cdp_state.mark_app_quit(name);
            }
            return;
        }
        let Some((app_name, kind_hint)) =
            Self::resolve_cdp_target(arguments, result_text, mcp).await
        else {
            return;
        };
        // Stash the kind BEFORE the CDP decision so the record is
        // present even when CDP is skipped (Native short-circuit).
        if let Some(kind) = kind_hint.as_deref() {
            self.record_app_kind(&app_name, kind);
        }
        if let Some(cdp_port) = self
            .auto_connect_cdp(&app_name, kind_hint.as_deref(), mcp)
            .await
        {
            self.emit_event(AgentEvent::CdpConnected {
                app_name: app_name.clone(),
                port: cdp_port,
            })
            .await;
            // Refresh the client-side tool cache so observation gates
            // see CDP tools surfaced post-connect.
            if let Err(e) = mcp.refresh_server_tool_list().await {
                warn!(
                    error = %e,
                    "state-spine: post-CDP-connect tool-cache refresh failed",
                );
            }
        }
    }

    /// Evaluate the permission policy for a cached tool call.
    fn policy_for(
        &self,
        tool_name: &str,
        arguments: &Value,
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
    ) -> PermissionAction {
        let ann = annotations_by_tool
            .get(tool_name)
            .copied()
            .unwrap_or_default();
        evaluate_permission(&self.permissions, tool_name, arguments, &ann)
    }

    /// Prompt the operator for approval of a tool action. Port of the
    /// legacy `AgentRunner::request_approval`. Returns `None` when no
    /// approval gate is configured (auto-approve).
    ///
    /// `description_suffix` is appended to the human-facing description so
    /// callers can distinguish live calls from cached replays (the cache
    /// path passes `" (cached)"`; the live path passes `""`). This is the
    /// only behavioural difference between cached and live approval —
    /// sharing this helper avoids drift between the two paths.
    async fn request_approval(
        &self,
        tool_name: &str,
        arguments: &Value,
        step_index: usize,
        description_suffix: &str,
    ) -> Option<ApprovalResult> {
        let gate = self.approval_gate.as_ref()?;
        let description = format!(
            "{} with {}{}",
            tool_name,
            serde_json::to_string(arguments).unwrap_or_default(),
            description_suffix,
        );
        let request = ApprovalRequest {
            step_index,
            tool_name: tool_name.to_string(),
            arguments: arguments.clone(),
            description,
        };
        let (resp_tx, resp_rx) = oneshot::channel();
        if gate.request_tx.send((request, resp_tx)).await.is_ok() {
            match resp_rx.await {
                Ok(true) => {
                    debug!(tool = %tool_name, "state-spine: user approved action");
                    Some(ApprovalResult::Approved)
                }
                Ok(false) => {
                    tracing::info!(tool = %tool_name, "state-spine: user rejected action");
                    Some(ApprovalResult::Rejected)
                }
                Err(_) => {
                    warn!(tool = %tool_name, "state-spine: approval channel closed");
                    Some(ApprovalResult::Unavailable)
                }
            }
        } else {
            warn!(tool = %tool_name, "state-spine: approval channel send failed");
            Some(ApprovalResult::Unavailable)
        }
    }

    /// Convenience wrapper for the cache-replay path — tags the approval
    /// request description as `(cached)` so the UI can distinguish a live
    /// dispatch from a replay.
    async fn request_cached_approval(
        &self,
        tool_name: &str,
        arguments: &Value,
        step_index: usize,
    ) -> Option<ApprovalResult> {
        self.request_approval(tool_name, arguments, step_index, " (cached)")
            .await
    }

    /// Verify an agent-reported completion against a fresh screenshot via
    /// the VLM. Port of the legacy `AgentRunner::verify_completion`.
    ///
    /// Returns the prepared base64 screenshot + VLM reply **only when the
    /// VLM disagreed** (verdict = NO). The caller uses that payload to
    /// synthesise a `CompletionDisagreement` event and terminal reason.
    /// When the VLM agrees, or any step of the verification path fails (no
    /// vision backend, screenshot failure, VLM call failure, empty reply),
    /// returns `None` and the caller falls through to the normal
    /// `Completed` path — verification errors must not tank the run.
    ///
    /// On both YES and NO verdicts, a PNG screenshot + JSON metadata are
    /// written to `self.verification_artifacts_dir` when set. Persistence
    /// failures are logged at `warn` and do not affect the return value.
    async fn verify_completion<M: Mcp + ?Sized>(
        &mut self,
        goal: &str,
        summary: &str,
        mcp: &M,
    ) -> Option<(String, String)> {
        use crate::agent::completion_check::{
            VlmVerdict, build_completion_prompt, parse_yes_no, persist_verification_artifacts,
            pick_completion_screenshot_scope,
        };
        use crate::executor::screenshot::capture_screenshot_for_vlm;

        let vision = self.vision.as_ref()?.clone();

        // Target the screenshot scope at the connected CDP app when we
        // have one — Task 3a.6 wires `cdp_state` up via
        // `maybe_cdp_connect`, so `connected_app` now flows through to
        // the scope picker (matching legacy behaviour).
        let scope = pick_completion_screenshot_scope(self.cdp_state.connected_app.as_ref());
        let Some((prepared_b64, mime)) = capture_screenshot_for_vlm(mcp, scope.clone()).await
        else {
            warn!(
                scope = ?scope,
                "state-spine: completion verification screenshot capture failed — skipping VLM check",
            );
            return None;
        };

        let messages = vec![Message::user_with_images(
            build_completion_prompt(goal, summary),
            vec![(prepared_b64.clone(), mime)],
        )];
        let raw_reply = match vision.chat_boxed(&messages, None).await {
            Ok(resp) => resp
                .choices
                .first()
                .and_then(|c| c.message.content_text())
                .map(str::to_owned),
            Err(e) => {
                warn!(error = %e, "state-spine: VLM call failed — skipping completion check");
                return None;
            }
        };
        let reply = match raw_reply {
            Some(r) if !r.trim().is_empty() => r,
            _ => {
                warn!("state-spine: VLM returned empty reply — skipping completion check");
                return None;
            }
        };

        let verdict = parse_yes_no(&reply);

        // Persist artifacts for both verdicts so every verification call
        // leaves forensic evidence. Failures are non-fatal.
        if let Some(dir) = &self.verification_artifacts_dir {
            let ordinal = self.verification_count;
            if let Err(e) = persist_verification_artifacts(
                dir,
                ordinal,
                verdict,
                &reply,
                goal,
                summary,
                &prepared_b64,
            ) {
                warn!(
                    ordinal,
                    error = %e,
                    "state-spine: failed to persist completion-verification artifacts (non-fatal)",
                );
            }
        }
        self.verification_count += 1;

        match verdict {
            VlmVerdict::Yes => {
                tracing::info!(reply = %reply, "state-spine: VLM confirmed completion");
                None
            }
            VlmVerdict::No => {
                tracing::info!(reply = %reply, "state-spine: VLM rejected completion");
                Some((prepared_b64, reply))
            }
        }
    }

    /// Attempt to serve the current iteration from the [`AgentCache`]
    /// instead of asking the LLM. Port of the legacy
    /// `AgentRunner::try_replay_cache` — preserves every branch of the
    /// legacy semantics per D11.
    ///
    /// **Nine-branch catalogue (4 fall-through × 3 approval × 2 execution
    /// — approval Allow collapses with execution since it shares the
    /// dispatch tail):**
    ///
    /// 1. Fall-through: `!use_cache` or no elements.
    /// 2. Fall-through: same cache key as the last replay.
    /// 3. Fall-through: cache miss / unknown tool.
    /// 4. Fall-through: cached tool is observation-only / AX dispatch /
    ///    state-transition (stale on read).
    /// 5. Approval Deny: evict entry, record error step, bump
    ///    `consecutive_errors`, consult recovery strategy.
    /// 6. Approval Ask → Rejected: evict, record Replan step.
    /// 7. Approval Ask → Unavailable: set `TerminalReason::ApprovalUnavailable`
    ///    and break.
    /// 8. Execute → success: rebuild node (stubbed for Task 3a.5), bump
    ///    hit_count + produced_node_ids lineage, append
    ///    assistant_tool_calls + tool_result to the transcript, emit
    ///    `StepCompleted`, maybe_cdp_connect (stubbed for Task 3a.6),
    ///    destructive-cap check (stubbed for Task 3a.4). Continue.
    /// 9. Execute → tool_error / call_error: null `last_cache_key`, fall
    ///    through to the LLM.
    ///
    /// Preconditions: caller has already consulted
    /// [`Self::is_replay_eligible`]. Replay still verifies `use_cache`
    /// internally for parity with the legacy runner.
    #[allow(clippy::too_many_arguments)]
    async fn try_replay_cache<M: Mcp + ?Sized>(
        &mut self,
        goal: &str,
        elements: &[clickweave_core::cdp::CdpFindElementMatch],
        step_index: usize,
        // Threaded through for `add_workflow_node` (Task 3a.5 wiring):
        // the tool-to-NodeType mapping consults the advertised tool schemas.
        mcp_tools: &[Value],
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
        mcp: &M,
        messages: &mut Vec<Message>,
        previous_result: &mut Option<String>,
        last_cache_key: &mut Option<String>,
        last_failure: &mut Option<(String, Value, String)>,
    ) -> ReplayResult {
        // Branch 1: cache disabled or nothing to fingerprint.
        if !self.config.use_cache || elements.is_empty() {
            return ReplayResult::FellThrough;
        }
        let current_key = super::cache::cache_key(goal, elements);

        // Branch 2: same key as the last replay — break the loop so the
        // LLM picks the next action.
        if last_cache_key.as_ref() == Some(&current_key) {
            *last_cache_key = None;
            return ReplayResult::FellThrough;
        }

        // Branch 3: cache miss.
        let Some(cached) = self.cache.lookup(goal, elements) else {
            *last_cache_key = None;
            return ReplayResult::FellThrough;
        };

        // Branch 4a: observation-only entries (stale-on-read).
        if is_observation_tool(&cached.tool_name, annotations_by_tool) {
            debug!(
                tool = %cached.tool_name,
                "state-spine: skipping cached observation tool (stale entry)"
            );
            *last_cache_key = None;
            return ReplayResult::FellThrough;
        }
        // Branch 4b: AX dispatch uids are generation-scoped.
        if is_ax_dispatch_tool(&cached.tool_name) {
            debug!(
                tool = %cached.tool_name,
                "state-spine: skipping cached AX dispatch entry (uid is generation-scoped)"
            );
            *last_cache_key = None;
            return ReplayResult::FellThrough;
        }
        // Branch 4c: state-transition tools must never replay.
        if is_state_transition_tool(&cached.tool_name) {
            debug!(
                tool = %cached.tool_name,
                "state-spine: skipping cached state-transition entry (not safe to replay)"
            );
            *last_cache_key = None;
            return ReplayResult::FellThrough;
        }

        let cached_tool = cached.tool_name.clone();
        let cached_args = cached.arguments.clone();
        debug!(
            tool = %cached_tool,
            hits = cached.hit_count,
            "state-spine: cache hit — replaying cached decision"
        );

        // Approval gating (branches 5–7). Observation tools already bailed
        // above, so every surviving entry is approval-gated.
        let needs_approval = !is_observation_tool(&cached_tool, annotations_by_tool);
        if needs_approval {
            let policy_action = self.policy_for(&cached_tool, &cached_args, annotations_by_tool);
            if matches!(policy_action, PermissionAction::Deny) {
                // Branch 5: hard policy reject.
                self.cache.remove(goal, elements);
                *last_cache_key = None;
                let err_msg = format!("Tool `{}` denied by permission policy", cached_tool);
                warn!(
                    tool = %cached_tool,
                    "state-spine: cached tool denied by permission policy"
                );
                let command = AgentCommand::ToolCall {
                    tool_name: cached_tool.clone(),
                    arguments: cached_args.clone(),
                    tool_call_id: format!("cache-{}", step_index),
                };
                self.emit_event(AgentEvent::StepFailed {
                    step_index,
                    tool_name: cached_tool.clone(),
                    error: err_msg.clone(),
                })
                .await;
                let step = AgentStep {
                    index: step_index,
                    elements: elements.to_vec(),
                    command,
                    outcome: StepOutcome::Error(err_msg.clone()),
                    page_url: self.state.current_url.clone(),
                };
                self.state.steps.push(step);
                self.state.consecutive_errors += 1;
                self.consecutive_errors = self.state.consecutive_errors;
                *previous_result = Some(format!("Error: {}", err_msg));
                let action = recovery_strategy(
                    self.state.consecutive_errors,
                    self.config.max_consecutive_errors,
                );
                if matches!(action, RecoveryAction::Abort) {
                    self.state.terminal_reason = Some(TerminalReason::MaxErrorsReached {
                        consecutive_errors: self.state.consecutive_errors,
                    });
                    return ReplayResult::Break;
                }
                return ReplayResult::Continue;
            }
            if matches!(policy_action, PermissionAction::Ask) {
                match self
                    .request_cached_approval(&cached_tool, &cached_args, step_index)
                    .await
                {
                    Some(ApprovalResult::Rejected) => {
                        // Branch 6: operator rejected — evict + replan.
                        self.cache.remove(goal, elements);
                        *last_cache_key = None;
                        let command = AgentCommand::ToolCall {
                            tool_name: cached_tool.clone(),
                            arguments: cached_args.clone(),
                            tool_call_id: format!("cache-{}", step_index),
                        };
                        let step = AgentStep {
                            index: step_index,
                            elements: elements.to_vec(),
                            command,
                            outcome: StepOutcome::Replan("User rejected cached action".to_string()),
                            page_url: self.state.current_url.clone(),
                        };
                        self.state.steps.push(step);
                        *previous_result = Some("Replan: user rejected cached action".to_string());
                        return ReplayResult::Continue;
                    }
                    Some(ApprovalResult::Unavailable) => {
                        // Branch 7: approval channel gone — terminal.
                        self.state.terminal_reason = Some(TerminalReason::ApprovalUnavailable);
                        return ReplayResult::Break;
                    }
                    // Approved or no gate configured — fall through to execute.
                    _ => {}
                }
            }
            // PermissionAction::Allow: no prompt.
        }

        // Branches 8 & 9: execute the cached tool call against MCP.
        match mcp.call_tool(&cached_tool, Some(cached_args.clone())).await {
            Ok(result) if !result.is_error.unwrap_or(false) => {
                // Branch 8: success path.
                let result_text = extract_result_text(&result);
                let tool_call_id = format!("cache-{}", step_index);
                let command = AgentCommand::ToolCall {
                    tool_name: cached_tool.clone(),
                    arguments: cached_args.clone(),
                    tool_call_id: tool_call_id.clone(),
                };

                // Rebuild the workflow node for this run — the cache stores
                // decisions across runs, so the current workflow needs the
                // replayed action as a node. The produced node id is
                // appended to the cached entry's lineage so selective-delete
                // can evict the right cross-run rows later.
                let produced_node_id_on_replay = self
                    .add_workflow_node(&cached_tool, &cached_args, mcp_tools, annotations_by_tool)
                    .await;
                if let Some(entry) = self.cache.entries.get_mut(&current_key) {
                    if let Some(node_id) = produced_node_id_on_replay {
                        entry.produced_node_ids.push(node_id);
                    }
                    entry.hit_count += 1;
                }

                // Reconstruct transcript so the LLM sees the full action
                // history, not just the raw result text.
                messages.push(Message::assistant_tool_calls(vec![
                    clickweave_llm::ToolCall {
                        id: tool_call_id.clone(),
                        call_type: clickweave_llm::CallType::Function,
                        function: clickweave_llm::FunctionCall {
                            name: cached_tool.clone(),
                            arguments: cached_args.clone(),
                        },
                    },
                ]));
                messages.push(Message::tool_result(&tool_call_id, &result_text));

                self.emit_event(AgentEvent::StepCompleted {
                    step_index,
                    tool_name: cached_tool.clone(),
                    summary: crate::agent::prompt::truncate_summary(&result_text, 120),
                })
                .await;

                // Auto-CDP-connect after a cached launch_app /
                // focus_window replay. State-transition tools already
                // fall through above (branch 4c) today, but the hook
                // stays here so behaviour parity with the live path
                // survives any future cache-filter relaxation, and so
                // that a cached `quit_app` still clears CDP state.
                self.maybe_cdp_connect(&cached_tool, &cached_args, &result_text, mcp)
                    .await;

                let step = AgentStep {
                    index: step_index,
                    elements: elements.to_vec(),
                    command,
                    outcome: StepOutcome::Success(result_text.clone()),
                    page_url: self.state.current_url.clone(),
                };
                self.state.steps.push(step);
                self.state.consecutive_errors = 0;
                self.consecutive_errors = 0;
                *last_failure = None;
                *previous_result = Some(result_text);
                *last_cache_key = Some(current_key);

                // Destructive-cap accounting: the cached replay counts toward
                // the streak just like a live tool call. State-transition
                // tools (the common destructive case) already fall through at
                // branch 4c, so this guards the narrow tail where a cached
                // non-transition tool carries `destructive_hint == Some(true)`.
                if matches!(
                    self.maybe_halt_on_destructive_cap(&cached_tool, annotations_by_tool),
                    CapStatus::CapReached
                ) {
                    self.emit_destructive_cap_hit().await;
                    return ReplayResult::Break;
                }
                ReplayResult::Continue
            }
            Ok(result) => {
                // Branch 9a: tool returned is_error=true.
                let err_text = extract_result_text(&result);
                debug!(
                    error = %err_text,
                    "state-spine: cached decision returned error, falling through to LLM"
                );
                *last_cache_key = None;
                ReplayResult::FellThrough
            }
            Err(e) => {
                // Branch 9b: the call itself failed.
                debug!(
                    error = %e,
                    "state-spine: cached decision execution failed, falling through to LLM"
                );
                *last_cache_key = None;
                ReplayResult::FellThrough
            }
        }
    }
}

/// Parse a raw LLM response `Message` into an `AgentTurn`.
///
/// The state-spine wire format is "0..N mutations + 1 action", but real
/// backends return the action via OpenAI-style `tool_calls`. Task 3a.1
/// implements a minimum parser: the **first** `tool_calls[0]` is mapped to
/// an `AgentAction`; mutations stay empty (the harness-owned state mutation
/// path for real LLM output is covered by a later task). Pseudo-tool names
/// `agent_done` / `agent_replan` dispatch to the matching `AgentAction`
/// variant; everything else becomes `AgentAction::ToolCall`.
///
/// Text-only replies (no `tool_calls`) map to `AgentAction::AgentReplan`
/// with the assistant's raw text as the reason — the LLM "forgot" to call a
/// tool, so re-observe on the next iteration with the text as context
/// instead of aborting the run.
///
/// `TODO(task-3a.2)`: extend to read a structured `{ mutations, action }`
/// JSON envelope when the prompt spine asks the LLM for one.
pub fn parse_agent_turn(message: &Message) -> anyhow::Result<AgentTurn> {
    if let Some(tool_call) = message.tool_calls.as_ref().and_then(|tcs| tcs.first()) {
        let name = &tool_call.function.name;
        let args = tool_call.function.arguments.clone();

        let action = match name.as_str() {
            "agent_done" => {
                let summary = args
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("Goal completed")
                    .to_string();
                AgentAction::AgentDone { summary }
            }
            "agent_replan" => {
                let reason = args
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown reason")
                    .to_string();
                AgentAction::AgentReplan { reason }
            }
            _ => AgentAction::ToolCall {
                tool_name: name.clone(),
                arguments: args,
                tool_call_id: tool_call.id.clone(),
            },
        };

        return Ok(AgentTurn {
            mutations: Vec::new(),
            action,
        });
    }

    // Text-only response: treat as a replan request so the run re-observes
    // next turn instead of aborting. Mirrors the legacy "no tool call"
    // recovery hook.
    let reason = message
        .content_text()
        .map(str::to_owned)
        .unwrap_or_else(|| "LLM returned no tool call and no text".to_string());
    Ok(AgentTurn {
        mutations: Vec::new(),
        action: AgentAction::AgentReplan { reason },
    })
}

/// Adapter that turns any `&dyn Mcp` into the `ToolExecutor` trait expected
/// by `run_turn`. Kept private to `runner.rs` — the plan names this
/// `McpToolExecutor` so later tasks can grep for the anchor.
struct McpToolExecutor<'a, M: Mcp + ?Sized> {
    mcp: &'a M,
}

#[async_trait::async_trait]
impl<M: Mcp + ?Sized> ToolExecutor for McpToolExecutor<'_, M> {
    async fn call_tool(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<String, String> {
        let result = self
            .mcp
            .call_tool(tool_name, Some(arguments.clone()))
            .await
            .map_err(|e| e.to_string())?;
        let text = result
            .content
            .iter()
            .filter_map(|c| c.as_text())
            .collect::<Vec<_>>()
            .join("\n");
        if result.is_error == Some(true) {
            Err(text)
        } else {
            Ok(text)
        }
    }
}

impl StateRunner {
    /// Top-level observe → compose → LLM → parse → apply → dispatch →
    /// compact control loop. Task 3a.1 ships the minimum skeleton; later
    /// tasks (flagged by `TODO(task-3a.N)` markers inline) wire cache
    /// replay, VLM verification, approval, loop detection,
    /// consecutive-destructive cap, workflow-graph emission, CDP
    /// auto-connect, synthetic `focus_window` skip, recovery strategy,
    /// and boundary `StepRecord` writes.
    ///
    /// The signature mirrors `AgentRunner::run` so `run_agent_workflow`
    /// can pivot onto `StateRunner` without a caller-side change. Crate-
    /// private because the `Mcp` trait is `pub(crate)`; the public entry
    /// point stays [`crate::agent::run_agent_workflow`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run<B, M>(
        mut self,
        llm: &B,
        mcp: &M,
        goal: String,
        workflow: clickweave_core::Workflow,
        mcp_tools: Vec<Value>,
        anchor_node_id: Option<uuid::Uuid>,
    ) -> anyhow::Result<(AgentState, AgentCache)>
    where
        B: ChatBackend + ?Sized,
        M: Mcp + ?Sized,
    {
        use crate::agent::context::{CompactBudget, compact};
        use crate::agent::prompt::{build_system_prompt, build_user_turn_message};

        // Reset the visible state tuple to match the freshly-provided
        // workflow. `AgentState::new(workflow)` wipes steps/terminal_reason
        // so the same `StateRunner` could in theory be reused across runs,
        // though `self` is consumed below.
        self.state = AgentState::new(workflow);
        self.state.last_node_id = anchor_node_id;

        // Build the system prompt from the raw openai-shaped tool list.
        // `build_system_prompt` expects `clickweave_mcp::Tool`; the raw
        // `Vec<Value>` is already openai-shape, so extract the minimum
        // fields each tool entry carries.
        //
        // D18 (Task 3.5): the system prompt is stable across runs —
        // variant context + prior-turn log are pre-composed into `goal`
        // at the caller seam (`build_goal_block`) so they land in
        // `messages[1]`, preserving the `messages[0]` cache prefix.
        let tool_list_for_prompt = openai_tools_to_mcp_tool_list(&mcp_tools);
        let system_text = build_system_prompt(&tool_list_for_prompt);

        // `goal` already carries the prior-turn log + variant-context
        // composed by `build_goal_block` at the Tauri seam. Feed it
        // straight into the user turn so messages[1] is the single
        // run-specific slot.
        let initial_user = build_user_turn_message(&self.world_model, &self.task_state, 0, &goal);

        let mut messages = vec![Message::system(system_text), Message::user(initial_user)];

        // Add the pseudo-tools so the LLM sees the full action vocabulary.
        // Seed once per run and never mutate — mid-run tool-list changes
        // invalidate every prior prompt-cache prefix.
        let tools: Vec<Value> = mcp_tools
            .iter()
            .cloned()
            .chain([
                crate::agent::prompt::agent_done_tool(),
                crate::agent::prompt::agent_replan_tool(),
            ])
            .collect();

        // Annotations index is seeded once per run so the cache-replay
        // gate, permission-policy evaluation, and (Task 3a.4) destructive
        // cap see the same `read_only_hint` / `destructive_hint` view.
        let annotations_by_tool = build_annotations_index(&mcp_tools);

        let budget = CompactBudget::default();
        let mut previous_result: Option<String> = None;
        // Tracks the cache key of the previous successful replay so the
        // next iteration can detect a same-key repeat and fall through to
        // the LLM instead of thrashing on one cached decision.
        let mut last_cache_key: Option<String> = None;
        // Reserved for Task 3a.4 loop detection: `(tool_name, arguments,
        // error)` from the last failing live call. Carried here so the
        // cache-replay success path can clear it (parity with legacy).
        let mut last_failure: Option<(String, Value, String)> = None;

        for _step_index in 0..self.config.max_steps {
            if self.state.completed {
                break;
            }

            // 1. Observe — fetch elements + detect page transition.
            let elements = self.fetch_elements(mcp).await;

            // 1a. Cache-replay gate. `is_replay_eligible` enforces D17
            // (Phase::Exploring, empty subgoal stack, no watch slots);
            // `try_replay_cache` layers in the per-entry stale-on-read
            // guards, approval gating, and live MCP re-dispatch.
            if self.is_replay_eligible() {
                let replay = self
                    .try_replay_cache(
                        &goal,
                        &elements,
                        self.state.steps.len(),
                        &mcp_tools,
                        &annotations_by_tool,
                        mcp,
                        &mut messages,
                        &mut previous_result,
                        &mut last_cache_key,
                        &mut last_failure,
                    )
                    .await;
                match replay {
                    ReplayResult::Continue => continue,
                    ReplayResult::Break => break,
                    ReplayResult::FellThrough => {}
                }
            }
            // No pre-step CDP maybe-connect — legacy also defers the
            // decision to the post-tool hook (`maybe_cdp_connect` after
            // a successful `launch_app` / `focus_window`). CDP tools the
            // LLM picks before a connection exists return a "not
            // connected" MCP error that the recovery strategy absorbs.

            // 2. Compose the per-turn user message with the state block +
            // the previous tool body as the observation, then compact the
            // history before the LLM call.
            let step_obs = previous_result.clone().unwrap_or_default();
            let step_msg = build_user_turn_message(
                &self.world_model,
                &self.task_state,
                self.step_index,
                &step_obs,
            );
            messages.push(Message::user(step_msg));
            messages = compact(messages, &budget);

            // 3. LLM call.
            let response = llm
                .chat(&messages, Some(&tools))
                .await
                .context("Agent LLM call failed")?;
            let choice = response
                .choices
                .into_iter()
                .next()
                .context("No choices in LLM response")?;

            // 4. Parse the LLM response into an AgentTurn. Task 3a.1's
            // parser maps tool_calls[0] to the action; mutations stay empty.
            let turn = parse_agent_turn(&choice.message)?;

            // 4a'. Synthetic `focus_window` skip. When the MCP surface +
            // app kind lets us suppress the focus-stealing MCP call
            // (Native + full AX toolset, Electron/Chrome with live CDP,
            // or `allow_focus_window = false`), we emit a synthetic
            // `Success` outcome whose body matches the legacy sentinel
            // strings and advance the loop without dispatching. The
            // runner records a step and a `StepCompleted` event so the
            // call stays visible to the transcript / UI, but the
            // workflow-graph filter (`is_synthetic_focus_skip`) keeps
            // it out of the recorded workflow. Port of the legacy
            // `AgentRunner::execute_response`'s focus_window guard.
            if let AgentAction::ToolCall {
                tool_name,
                arguments,
                tool_call_id,
            } = &turn.action
                && tool_name == "focus_window"
                && let Some(reason) = self.should_skip_focus_window(arguments, mcp)
            {
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "focus_window".to_string(),
                    summary: reason.sub_action_summary().to_string(),
                })
                .await;
                let skip_body = reason.llm_message().to_string();
                debug!(
                    tool = "focus_window",
                    app = arguments
                        .get("app_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                    reason = skip_body,
                    "state-spine: suppressing focus_window",
                );
                let step_idx_for_event = self.state.steps.len();
                self.state.steps.push(AgentStep {
                    index: step_idx_for_event,
                    elements: elements.clone(),
                    command: AgentCommand::ToolCall {
                        tool_name: tool_name.clone(),
                        arguments: arguments.clone(),
                        tool_call_id: tool_call_id.clone(),
                    },
                    outcome: StepOutcome::Success(skip_body.clone()),
                    page_url: self.state.current_url.clone(),
                });
                self.state.consecutive_errors = 0;
                self.consecutive_errors = 0;
                last_failure = None;
                previous_result = Some(skip_body.clone());
                self.emit_event(AgentEvent::StepCompleted {
                    step_index: step_idx_for_event,
                    tool_name: "focus_window".to_string(),
                    summary: crate::agent::prompt::truncate_summary(&skip_body, 120),
                })
                .await;
                append_assistant_and_tool_result(
                    &mut messages,
                    &choice.message,
                    previous_result.as_deref(),
                );
                continue;
            }

            // 4a. Permission policy + approval gate for live `ToolCall`
            // actions. Mirrors the legacy `AgentRunner::execute_response`
            // pre-dispatch policy check. The cache-replay path has its
            // own identical gate at `try_replay_cache`; observation
            // tools bypass approval entirely on both paths.
            if let AgentAction::ToolCall {
                tool_name,
                arguments,
                tool_call_id,
            } = &turn.action
            {
                let needs_approval = !is_observation_tool(tool_name, &annotations_by_tool);
                if needs_approval {
                    match self.policy_for(tool_name, arguments, &annotations_by_tool) {
                        PermissionAction::Deny => {
                            warn!(tool = %tool_name, "state-spine: tool denied by permission policy");
                            let err_msg =
                                format!("Tool `{}` denied by permission policy", tool_name);
                            self.state.steps.push(AgentStep {
                                index: self.state.steps.len(),
                                elements: elements.clone(),
                                command: AgentCommand::ToolCall {
                                    tool_name: tool_name.clone(),
                                    arguments: arguments.clone(),
                                    tool_call_id: tool_call_id.clone(),
                                },
                                outcome: StepOutcome::Error(err_msg.clone()),
                                page_url: self.state.current_url.clone(),
                            });
                            self.state.consecutive_errors += 1;
                            self.consecutive_errors = self.state.consecutive_errors;
                            previous_result = Some(err_msg);
                            append_assistant_and_tool_result(
                                &mut messages,
                                &choice.message,
                                previous_result.as_deref(),
                            );
                            continue;
                        }
                        PermissionAction::Allow => {
                            // Policy pre-authorised this tool — skip the
                            // approval prompt entirely.
                            debug!(
                                tool = %tool_name,
                                "state-spine: permission policy allowed tool — skipping approval"
                            );
                        }
                        PermissionAction::Ask => {
                            match self
                                .request_approval(tool_name, arguments, self.state.steps.len(), "")
                                .await
                            {
                                Some(ApprovalResult::Rejected) => {
                                    // Operator rejected: record a Replan
                                    // step and re-observe next iteration
                                    // — matches the cache-replay branch
                                    // and the legacy `StepOutcome::Replan`
                                    // return from `execute_response`.
                                    self.state.steps.push(AgentStep {
                                        index: self.state.steps.len(),
                                        elements: elements.clone(),
                                        command: AgentCommand::ToolCall {
                                            tool_name: tool_name.clone(),
                                            arguments: arguments.clone(),
                                            tool_call_id: tool_call_id.clone(),
                                        },
                                        outcome: StepOutcome::Replan(
                                            "User rejected action".to_string(),
                                        ),
                                        page_url: self.state.current_url.clone(),
                                    });
                                    previous_result =
                                        Some("Replan: user rejected action".to_string());
                                    append_assistant_and_tool_result(
                                        &mut messages,
                                        &choice.message,
                                        previous_result.as_deref(),
                                    );
                                    continue;
                                }
                                Some(ApprovalResult::Unavailable) => {
                                    warn!("state-spine: approval system unavailable — terminating");
                                    self.state.terminal_reason =
                                        Some(TerminalReason::ApprovalUnavailable);
                                    break;
                                }
                                // Approved or no gate configured — proceed.
                                Some(ApprovalResult::Approved) | None => {}
                            }
                        }
                    }
                }
            }

            // 5. Apply mutations + dispatch the action via run_turn.
            //    `previous_errors` captures the error counter from the
            //    iteration just before the new turn; a drop from >0 to 0
            //    after `run_turn` signals the `Recovering -> Executing`
            //    transition that Task 3a.6.5 persists as a
            //    `BoundaryKind::RecoverySucceeded` record (D8).
            let previous_errors = self.consecutive_errors;
            let executor = McpToolExecutor { mcp };
            let (outcome, warnings, milestones_appended) = self.run_turn(&turn, &executor).await;
            for w in warnings {
                tracing::warn!(warning = %w, "state-spine: mutation warning");
            }

            // 5a. SubgoalCompleted boundary writes — one `StepRecord` per
            //     successfully popped subgoal (D8). Performed before the
            //     outcome match so the record reflects the task_state /
            //     world_model immediately after the mutation burst, matching
            //     the semantic "the milestone just landed".
            if milestones_appended > 0 {
                self.write_subgoal_completed_records(milestones_appended, &turn)
                    .await;
            }

            // 5b. RecoverySucceeded boundary write — a tool success that
            //     cleared the consecutive-error streak (D8). Only fires on
            //     the exact `Recovering -> Executing` transition: the
            //     previous turn had errors, this turn brought the counter
            //     to zero. Tool calls that never errored (previous_errors
            //     == 0) and repeated-error turns (consecutive_errors > 0)
            //     are both skipped.
            if previous_errors > 0
                && self.consecutive_errors == 0
                && matches!(outcome, TurnOutcome::ToolSuccess { .. })
            {
                self.write_recovery_succeeded_record(&turn, &outcome).await;
            }

            // 6. Map the TurnOutcome into AgentStep + TerminalReason.
            match outcome {
                TurnOutcome::ToolSuccess {
                    tool_name,
                    tool_body,
                } => {
                    let (command, step_outcome) = match &turn.action {
                        AgentAction::ToolCall {
                            arguments,
                            tool_call_id,
                            ..
                        } => (
                            AgentCommand::ToolCall {
                                tool_name: tool_name.clone(),
                                arguments: arguments.clone(),
                                tool_call_id: tool_call_id.clone(),
                            },
                            StepOutcome::Success(tool_body.clone()),
                        ),
                        // run_turn only returns ToolSuccess for ToolCall
                        // actions; the other arms are unreachable here.
                        _ => unreachable!("ToolSuccess outcome implies ToolCall action"),
                    };
                    let step_idx_for_event = self.state.steps.len();
                    self.state.steps.push(AgentStep {
                        index: step_idx_for_event,
                        elements: elements.clone(),
                        command,
                        outcome: step_outcome,
                        page_url: self.state.current_url.clone(),
                    });
                    previous_result = Some(tool_body.clone());
                    // Clear the loop-detection tracker on any success.
                    last_failure = None;
                    // Emit the live StepCompleted event so subscribers see a
                    // successful turn (cache-replay has its own emission in
                    // `try_replay_cache`).
                    self.emit_event(AgentEvent::StepCompleted {
                        step_index: step_idx_for_event,
                        tool_name: tool_name.clone(),
                        summary: crate::agent::prompt::truncate_summary(&tool_body, 120),
                    })
                    .await;
                    // Destructive-cap accounting on the live path. Mirrors
                    // `AgentRunner::handle_step_outcome`'s cap branch. The
                    // cache-replay path has the same guard inline.
                    if matches!(
                        self.maybe_halt_on_destructive_cap(&tool_name, &annotations_by_tool),
                        CapStatus::CapReached
                    ) {
                        self.emit_destructive_cap_hit().await;
                        break;
                    }

                    // Workflow-graph emission. Non-observation tools become
                    // nodes on `state.workflow`; the first node chains from
                    // `state.last_node_id` (seeded by `anchor_node_id` at
                    // the top of `run`). Cache writes stamp the produced
                    // node id into the cache entry's `produced_node_ids`
                    // lineage so selective-delete can evict the right rows
                    // later.
                    let tool_arguments = match &turn.action {
                        AgentAction::ToolCall { arguments, .. } => arguments.clone(),
                        _ => unreachable!("ToolSuccess outcome implies ToolCall action"),
                    };
                    let produced_node_id = self
                        .add_workflow_node(
                            &tool_name,
                            &tool_arguments,
                            &mcp_tools,
                            &annotations_by_tool,
                        )
                        .await;

                    // Cache write. Mirrors the legacy filter: only cache
                    // action tools, never observation / AX dispatch /
                    // state-transition tools, and only when the page
                    // fingerprint is non-empty.
                    if self.config.use_cache
                        && !is_observation_tool(&tool_name, &annotations_by_tool)
                        && !is_ax_dispatch_tool(&tool_name)
                        && !is_state_transition_tool(&tool_name)
                        && !elements.is_empty()
                    {
                        match produced_node_id {
                            Some(node_id) => {
                                self.cache.store_with_node(
                                    &goal,
                                    &elements,
                                    tool_name.clone(),
                                    tool_arguments.clone(),
                                    node_id,
                                );
                            }
                            None => {
                                self.cache.store(
                                    &goal,
                                    &elements,
                                    tool_name.clone(),
                                    tool_arguments.clone(),
                                );
                            }
                        }
                    }
                    // Auto-connect CDP after a successful `launch_app`
                    // / `focus_window` (Electron / Chrome targets only;
                    // native apps short-circuit inside the helper).
                    // Keeps `cdp_state` in lock-step with `quit_app`
                    // too. Synthetic focus_window skips never reach
                    // this arm — they short-circuit before tool
                    // dispatch above (4a') — so the hook does not need
                    // an is-synthetic guard here.
                    self.maybe_cdp_connect(&tool_name, &tool_arguments, &tool_body, mcp)
                        .await;
                }
                TurnOutcome::ToolError { tool_name, error } => {
                    let (command, step_outcome, tool_arguments) = match &turn.action {
                        AgentAction::ToolCall {
                            arguments,
                            tool_call_id,
                            ..
                        } => (
                            AgentCommand::ToolCall {
                                tool_name: tool_name.clone(),
                                arguments: arguments.clone(),
                                tool_call_id: tool_call_id.clone(),
                            },
                            StepOutcome::Error(error.clone()),
                            arguments.clone(),
                        ),
                        _ => unreachable!("ToolError outcome implies ToolCall action"),
                    };
                    let step_idx_for_event = self.state.steps.len();
                    self.state.steps.push(AgentStep {
                        index: step_idx_for_event,
                        elements: elements.clone(),
                        command,
                        outcome: step_outcome,
                        page_url: self.state.current_url.clone(),
                    });
                    self.state.consecutive_errors = self.consecutive_errors;
                    previous_result = Some(error.clone());

                    // Emit StepFailed so subscribers see the failing turn;
                    // the cache-replay policy-deny branch emits the same
                    // event for its synthetic errors.
                    self.emit_event(AgentEvent::StepFailed {
                        step_index: step_idx_for_event,
                        tool_name: tool_name.clone(),
                        error: error.clone(),
                    })
                    .await;

                    // Loop detection: if the identical (tool, args) call
                    // came back with the identical error on two successive
                    // turns, halt instead of burning the max-errors budget.
                    let looped = matches!(
                        last_failure.as_ref(),
                        Some((prev_tool, prev_args, prev_err))
                            if prev_tool == &tool_name
                                && prev_args == &tool_arguments
                                && prev_err == &error
                    );
                    if looped {
                        warn!(
                            tool = %tool_name,
                            error = %error,
                            "state-spine: identical failing tool call repeated — aborting"
                        );
                        self.state.terminal_reason =
                            Some(TerminalReason::LoopDetected { tool_name, error });
                        break;
                    }
                    last_failure = Some((tool_name, tool_arguments, error));

                    // Recovery strategy: `Abort` halts with MaxErrorsReached;
                    // `Continue` falls through to the next iteration which
                    // re-observes. Legacy placed this wiring in 3a.6's scope
                    // but the 3a.4 test matrix requires MaxErrorsReached as
                    // a terminal reason, so the whole hook lands together
                    // here.
                    let action = recovery_strategy(
                        self.state.consecutive_errors,
                        self.config.max_consecutive_errors,
                    );
                    if matches!(action, RecoveryAction::Abort) {
                        warn!(
                            errors = self.state.consecutive_errors,
                            "state-spine: too many consecutive errors — aborting"
                        );
                        self.state.terminal_reason = Some(TerminalReason::MaxErrorsReached {
                            consecutive_errors: self.state.consecutive_errors,
                        });
                        break;
                    }
                }
                TurnOutcome::Done { summary } => {
                    // Post-`agent_done` VLM verification. A NO verdict
                    // halts the run and surfaces a disagreement event so
                    // the user can adjudicate; a YES verdict (or any
                    // verification error — no backend, screenshot failure,
                    // empty reply, call failure) falls through to normal
                    // completion. Verification must never tank the run.
                    let disagreement = self.verify_completion(&goal, &summary, mcp).await;
                    if let Some((screenshot_b64, vlm_reasoning)) = disagreement {
                        warn!(
                            "state-spine: VLM disagreed with agent_done — halting for user review"
                        );
                        self.emit_event(AgentEvent::CompletionDisagreement {
                            screenshot_b64,
                            vlm_reasoning: vlm_reasoning.clone(),
                            agent_summary: summary.clone(),
                        })
                        .await;
                        self.state.terminal_reason = Some(TerminalReason::CompletionDisagreement {
                            agent_summary: summary.clone(),
                            vlm_reasoning,
                        });
                        // Leave `state.completed` as `false` — the run
                        // halts pending user decision instead of
                        // re-planning automatically.
                        break;
                    }

                    self.state.completed = true;
                    self.state.summary = Some(summary.clone());
                    self.state.terminal_reason = Some(TerminalReason::Completed {
                        summary: summary.clone(),
                    });
                    self.emit_event(AgentEvent::GoalComplete { summary }).await;
                    break;
                }
                TurnOutcome::Replan { reason } => {
                    // Replan does not add a step; the next iteration
                    // re-observes. Record the reason as the observation
                    // for the next turn so the LLM sees why it was asked
                    // to replan.
                    previous_result = Some(format!("replan: {}", reason));
                }
            }

            // 7. Append the assistant message + tool result onto the
            // transcript so the next iteration's LLM call sees the
            // full turn.
            append_assistant_and_tool_result(
                &mut messages,
                &choice.message,
                previous_result.as_deref(),
            );
        }

        // Post-loop: populate the terminal reason if the loop fell out of
        // max_steps without completing.
        if !self.state.completed && self.state.terminal_reason.is_none() {
            self.state.terminal_reason = Some(TerminalReason::MaxStepsReached {
                steps_executed: self.state.steps.len(),
            });
        }

        // Terminal boundary write (D8 / Task 3a.6.5). Every exit path from
        // the loop above sets `state.terminal_reason` before breaking —
        // plus the post-loop MaxStepsReached fallback right above — so a
        // single write here covers `Completed`, `MaxStepsReached`,
        // `MaxErrorsReached`, `ApprovalUnavailable`, `CompletionDisagreement`,
        // `ConsecutiveDestructiveCap`, and `LoopDetected` uniformly. A
        // run without any terminal_reason is a bug (no known code path
        // produces it), so the match_ is exhaustive on `Some`.
        if self.state.terminal_reason.is_some() {
            self.write_terminal_record().await;
        }

        Ok((self.state, self.cache))
    }
}

/// Translate the openai-shaped `Vec<Value>` tool list (produced by
/// `Mcp::tools_as_openai`) into the `clickweave_mcp::Tool` shape the
/// prompt-spine builder needs. Keeps the openai format as the source of
/// truth for dispatch while letting the prompt builder operate on a typed
/// view.
fn openai_tools_to_mcp_tool_list(tools: &[Value]) -> Vec<clickweave_mcp::Tool> {
    tools
        .iter()
        .filter_map(|t| {
            let fun = t.get("function")?;
            let name = fun.get("name").and_then(Value::as_str)?.to_string();
            let description = fun
                .get("description")
                .and_then(Value::as_str)
                .map(String::from);
            let input_schema = fun
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let annotations = fun.get("annotations").cloned();
            Some(clickweave_mcp::Tool {
                name,
                description,
                input_schema,
                annotations,
            })
        })
        .collect()
}

/// Append the assistant's response and its tool result onto the transcript,
/// mirroring the legacy `AgentRunner::append_assistant_message`.
///
/// When the assistant returned `tool_calls`, the transcript gets the
/// assistant message (tool_calls only) plus a matching `tool_result`. When
/// the assistant returned plain text, only the assistant message is
/// appended.
fn append_assistant_and_tool_result(
    messages: &mut Vec<Message>,
    assistant: &Message,
    previous_result: Option<&str>,
) {
    if let Some(tc) = assistant
        .tool_calls
        .as_ref()
        .and_then(|calls| calls.first())
    {
        messages.push(Message::assistant_tool_calls(vec![tc.clone()]));
        messages.push(Message::tool_result(
            &tc.id,
            previous_result.unwrap_or("ok"),
        ));
    } else if let Some(text) = assistant.content_text() {
        messages.push(Message::assistant(text));
    }
}

#[cfg(test)]
mod builder_tests {
    use super::*;
    use clickweave_llm::{ChatBackend, ChatOptions, ChatResponse, Message};
    use serde_json::Value;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    /// Minimal stub implementing `ChatBackend` so we can confirm the
    /// blanket `DynChatBackend` impl lets us stash one behind `Arc<dyn>`.
    #[derive(Default)]
    struct YesVlmStub;
    impl ChatBackend for YesVlmStub {
        fn model_name(&self) -> &str {
            "yes-vlm"
        }
        async fn chat_with_options(
            &self,
            _messages: &[Message],
            _tools: Option<&[Value]>,
            _options: &ChatOptions,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                id: "t".into(),
                choices: vec![clickweave_llm::Choice {
                    index: 0,
                    message: Message::assistant("YES"),
                    finish_reason: Some("stop".into()),
                }],
                usage: None,
            })
        }
    }

    #[test]
    fn with_events_stores_sender() {
        let (tx, _rx) = mpsc::channel::<AgentEvent>(8);
        let r = StateRunner::new_for_test("g".to_string()).with_events(tx);
        assert!(r.event_tx.is_some());
    }

    #[test]
    fn with_approval_stores_gate() {
        let (tx, _rx) = mpsc::channel::<(ApprovalRequest, oneshot::Sender<bool>)>(8);
        let r = StateRunner::new_for_test("g".to_string()).with_approval(tx);
        assert!(r.approval_gate.is_some());
    }

    #[test]
    fn with_vision_stores_backend_as_arc_dyn() {
        let vlm: Arc<dyn DynChatBackend> = Arc::new(YesVlmStub);
        let r = StateRunner::new_for_test("g".to_string()).with_vision(vlm);
        assert!(r.vision.is_some());
    }

    #[test]
    fn with_permissions_replaces_default_policy() {
        let policy = PermissionPolicy::default();
        let r = StateRunner::new_for_test("g".to_string()).with_permissions(policy);
        // Confirm the field is populated — the default policy is Copy-
        // like and doesn't diverge from the constructor default, so the
        // guarantee here is "no panic, no drop".
        let _ = &r.permissions;
    }

    #[test]
    fn with_verification_artifacts_dir_stores_path() {
        let r = StateRunner::new_for_test("g".to_string())
            .with_verification_artifacts_dir(PathBuf::from("/tmp/artifacts"));
        assert_eq!(
            r.verification_artifacts_dir.as_deref(),
            Some(std::path::Path::new("/tmp/artifacts"))
        );
    }

    #[test]
    fn into_cache_returns_empty_cache_by_default() {
        let r = StateRunner::new_for_test("g".to_string());
        let cache = r.into_cache();
        assert!(cache.entries.is_empty());
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
mod turn_application_tests {
    use super::*;
    use crate::agent::task_state::TaskStateMutation;

    #[test]
    fn mutations_apply_in_order_before_action() {
        let mut r = StateRunner::new_for_test("g".to_string());
        let turn = AgentTurn {
            mutations: vec![
                TaskStateMutation::PushSubgoal {
                    text: "a".to_string(),
                },
                TaskStateMutation::PushSubgoal {
                    text: "b".to_string(),
                },
            ],
            action: AgentAction::AgentDone {
                summary: "done".to_string(),
            },
        };
        let warnings = r.apply_mutations(&turn.mutations);
        assert!(warnings.is_empty());
        assert_eq!(r.task_state.subgoal_stack.len(), 2);
        assert_eq!(r.task_state.subgoal_stack[1].text, "b");
    }

    #[test]
    fn invalid_mutation_produces_warning_but_others_still_apply() {
        let mut r = StateRunner::new_for_test("g".to_string());
        let muts = vec![
            TaskStateMutation::PushSubgoal {
                text: "a".to_string(),
            },
            TaskStateMutation::RefuteHypothesis { index: 99 },
            TaskStateMutation::PushSubgoal {
                text: "b".to_string(),
            },
        ];
        let warnings = r.apply_mutations(&muts);
        assert_eq!(warnings.len(), 1);
        assert_eq!(r.task_state.subgoal_stack.len(), 2);
    }
}

#[cfg(test)]
mod continuity_tests {
    use super::*;

    #[test]
    fn take_ax_snapshot_success_populates_last_native_ax_snapshot() {
        let mut r = StateRunner::new_for_test("g".to_string());
        r.step_index = 5;
        let body = "uid=a1g3 button \"OK\"\n  uid=a2g3 textbox";
        r.update_continuity_after_tool_success("take_ax_snapshot", body);
        let ax = r.world_model.last_native_ax_snapshot.as_ref().unwrap();
        assert_eq!(ax.value.captured_at_step, 5);
        assert!(ax.value.element_count >= 2);
        assert!(ax.value.ax_tree_text.contains("uid=a1g3"));
    }

    #[test]
    fn take_screenshot_success_populates_last_screenshot_ref() {
        let mut r = StateRunner::new_for_test("g".to_string());
        r.step_index = 4;
        let body = r#"{"screenshot_id":"ss-abc","width":1440,"height":900}"#;
        r.update_continuity_after_tool_success("take_screenshot", body);
        let s = r.world_model.last_screenshot.as_ref().unwrap();
        assert_eq!(s.value.screenshot_id, "ss-abc");
        assert_eq!(s.value.captured_at_step, 4);
    }

    #[test]
    fn non_snapshot_tool_does_not_touch_continuity() {
        let mut r = StateRunner::new_for_test("g".to_string());
        r.update_continuity_after_tool_success("cdp_click", "ok");
        assert!(r.world_model.last_native_ax_snapshot.is_none());
        assert!(r.world_model.last_screenshot.is_none());
    }
}

#[cfg(test)]
mod ax_enrichment_tests {
    use super::*;
    use clickweave_core::{
        AxClickParams, AxSelectParams, AxSetValueParams, AxTarget, McpToolCallParams, NodeType,
    };

    fn runner_with_snapshot(body: &str) -> StateRunner {
        use crate::agent::world_model::{AxSnapshotData, Fresh, FreshnessSource};
        let mut r = StateRunner::new_for_test("g".to_string());
        r.world_model.last_native_ax_snapshot = Some(Fresh {
            value: AxSnapshotData {
                snapshot_id: "a1g1".to_string(),
                element_count: 3,
                captured_at_step: 0,
                ax_tree_text: body.to_string(),
            },
            written_at: 0,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        });
        r
    }

    #[test]
    fn enrich_ax_click_resolved_uid_to_descriptor() {
        let r = runner_with_snapshot("uid=a5g2 AXButton \"Continue\"\n");
        let mut nt = NodeType::AxClick(AxClickParams {
            target: AxTarget::ResolvedUid("a5g2".into()),
            ..Default::default()
        });
        r.enrich_ax_descriptor(&mut nt);
        match nt {
            NodeType::AxClick(p) => assert_eq!(
                p.target,
                AxTarget::Descriptor {
                    role: "AXButton".into(),
                    name: "Continue".into(),
                    parent_name: None,
                }
            ),
            _ => panic!("expected AxClick"),
        }
    }

    #[test]
    fn upgrade_preserves_parent_name_for_outline_rows() {
        // NSOutlineView rows often share (role, name) across sections, so
        // the parent anchor is what makes the descriptor unambiguous.
        let snapshot = concat!(
            "uid=a1g1 AXOutline \"Sidebar\"\n",
            "  uid=a2g1 AXGroup \"Network\"\n",
            "    uid=a3g1 AXRow \"Wi-Fi\"\n",
        );
        let r = runner_with_snapshot(snapshot);
        let mut nt = NodeType::AxSelect(AxSelectParams {
            target: AxTarget::ResolvedUid("a3g1".into()),
            ..Default::default()
        });
        r.enrich_ax_descriptor(&mut nt);
        match nt {
            NodeType::AxSelect(p) => assert_eq!(
                p.target,
                AxTarget::Descriptor {
                    role: "AXRow".into(),
                    name: "Wi-Fi".into(),
                    parent_name: Some("Network".into()),
                }
            ),
            _ => panic!("expected AxSelect"),
        }
    }

    #[test]
    fn enrich_preserves_value_on_ax_set_value() {
        let r = runner_with_snapshot("uid=a10g1 AXTextField \"Email\"\n");
        let mut nt = NodeType::AxSetValue(AxSetValueParams {
            target: AxTarget::ResolvedUid("a10g1".into()),
            value: "preserved".into(),
            ..Default::default()
        });
        r.enrich_ax_descriptor(&mut nt);
        match nt {
            NodeType::AxSetValue(p) => {
                assert_eq!(p.value, "preserved");
                assert_eq!(
                    p.target,
                    AxTarget::Descriptor {
                        role: "AXTextField".into(),
                        name: "Email".into(),
                        parent_name: None,
                    }
                );
            }
            _ => panic!("expected AxSetValue"),
        }
    }

    #[test]
    fn enrich_is_noop_when_uid_not_in_snapshot() {
        let r = runner_with_snapshot("uid=a1g1 AXButton \"Other\"\n");
        let mut nt = NodeType::AxClick(AxClickParams {
            target: AxTarget::ResolvedUid("a99g9".into()),
            ..Default::default()
        });
        r.enrich_ax_descriptor(&mut nt);
        match nt {
            NodeType::AxClick(p) => assert_eq!(p.target, AxTarget::ResolvedUid("a99g9".into())),
            _ => panic!("expected AxClick"),
        }
    }

    #[test]
    fn enrich_is_noop_for_non_ax_nodes() {
        let r = runner_with_snapshot("uid=a1g1 AXButton \"X\"\n");
        let mut nt = NodeType::McpToolCall(McpToolCallParams {
            tool_name: "click".into(),
            arguments: serde_json::json!({}),
        });
        r.enrich_ax_descriptor(&mut nt);
        assert!(matches!(nt, NodeType::McpToolCall(_)));
    }

    #[test]
    fn enrich_is_noop_when_no_snapshot_captured() {
        let r = StateRunner::new_for_test("g".to_string());
        let mut nt = NodeType::AxClick(AxClickParams {
            target: AxTarget::ResolvedUid("a5g2".into()),
            ..Default::default()
        });
        r.enrich_ax_descriptor(&mut nt);
        match nt {
            NodeType::AxClick(p) => assert_eq!(p.target, AxTarget::ResolvedUid("a5g2".into())),
            _ => panic!("expected AxClick"),
        }
    }
}

#[cfg(test)]
mod storage_persistence_tests {
    use super::*;
    use crate::agent::step_record::{BoundaryKind, StepRecord, WorldModelSnapshot};
    use std::sync::{Arc, Mutex};

    fn sample_record(step_index: usize, boundary: BoundaryKind) -> StepRecord {
        StepRecord {
            step_index,
            boundary_kind: boundary,
            world_model_snapshot: WorldModelSnapshot::from_world_model(&WorldModel::default()),
            task_state_snapshot: TaskState::new("goal".to_string()),
            action_taken: serde_json::json!({"kind":"agent_done","summary":"done"}),
            outcome: serde_json::json!({"kind":"completed"}),
            timestamp: chrono::Utc::now(),
        }
    }

    #[test]
    fn write_step_record_is_noop_when_no_storage_attached() {
        // No storage means no panic, no events file, nothing persisted.
        let r = StateRunner::new_for_test("g".to_string());
        r.write_step_record(&sample_record(0, BoundaryKind::Terminal));
    }

    #[test]
    fn write_step_record_appends_to_events_jsonl_when_storage_attached() {
        let tmp = tempfile::tempdir().unwrap();
        let mut storage = clickweave_core::storage::RunStorage::new(tmp.path(), "test-workflow");
        let exec_dir = storage.begin_execution().expect("begin_execution");
        let storage = Arc::new(Mutex::new(storage));

        let r = StateRunner::new_for_test("g".to_string()).with_storage(storage.clone());
        r.write_step_record(&sample_record(1, BoundaryKind::SubgoalCompleted));
        r.write_step_record(&sample_record(2, BoundaryKind::Terminal));

        let events_path = tmp
            .path()
            .join(".clickweave")
            .join("runs")
            .join("test-workflow")
            .join(&exec_dir)
            .join("events.jsonl");
        let contents = std::fs::read_to_string(&events_path)
            .unwrap_or_else(|e| panic!("read {:?} failed: {}", events_path, e));
        let subgoal: Vec<_> = contents
            .lines()
            .filter(|l| l.contains("\"boundary_kind\":\"subgoal_completed\""))
            .collect();
        assert_eq!(subgoal.len(), 1);
        let terminal: Vec<_> = contents
            .lines()
            .filter(|l| l.contains("\"boundary_kind\":\"terminal\""))
            .collect();
        assert_eq!(terminal.len(), 1);
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

#[cfg(test)]
mod resolve_cdp_target_tests {
    //! Ported verbatim from the legacy `resolve_cdp_target_tests`
    //! for Task 3a.7.d. The legacy tests targeted
    //! `AgentRunner::<B>::resolve_cdp_target`; here they call
    //! `StateRunner::resolve_cdp_target` directly (no backend type
    //! parameter on the new runner's associated fn).
    use super::*;
    use crate::executor::Mcp;
    use clickweave_mcp::ToolCallResult;

    /// MCP stub that panics on any call. Every test in this module
    /// exercises paths (structured response, arguments-only) that must
    /// not reach MCP — the panic proves those paths don't regress to
    /// making extra round-trips.
    struct UnusedMcp;

    impl Mcp for UnusedMcp {
        async fn call_tool(
            &self,
            _name: &str,
            _arguments: Option<Value>,
        ) -> anyhow::Result<ToolCallResult> {
            panic!("resolve_cdp_target reached MCP on a fast-path case");
        }
        fn has_tool(&self, _name: &str) -> bool {
            false
        }
        fn tools_as_openai(&self) -> Vec<Value> {
            Vec::new()
        }
        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    async fn resolve(arguments: Value, result_text: &str) -> Option<(String, Option<String>)> {
        StateRunner::resolve_cdp_target(&arguments, result_text, &UnusedMcp).await
    }

    #[tokio::test]
    async fn structured_response_wins_over_pid_argument() {
        let arguments = serde_json::json!({ "pid": 16024 });
        let result_text = serde_json::json!({
            "app_name": "Signal",
            "pid": 16024,
            "bundle_id": "org.whispersystems.signal-desktop",
            "kind": "ElectronApp",
        })
        .to_string();
        let resolved = resolve(arguments, &result_text).await;
        assert_eq!(
            resolved,
            Some(("Signal".to_string(), Some("ElectronApp".to_string())))
        );
    }

    #[tokio::test]
    async fn plain_text_response_falls_back_to_arguments_app_name() {
        let arguments = serde_json::json!({ "app_name": "Signal" });
        let resolved = resolve(arguments, "Window focused successfully").await;
        assert_eq!(resolved, Some(("Signal".to_string(), None)));
    }

    #[tokio::test]
    async fn empty_app_name_in_structured_response_is_ignored() {
        let arguments = serde_json::json!({ "app_name": "Chrome" });
        let result_text = serde_json::json!({ "app_name": "", "pid": 0 }).to_string();
        let resolved = resolve(arguments, &result_text).await;
        assert_eq!(resolved, Some(("Chrome".to_string(), None)));
    }

    /// MCP stub that returns a fixed multi-text-block `list_apps` response.
    /// Pins the contract that the `pid → list_apps` CDP resolution path
    /// parses only the first text block: regression guard for a past bug
    /// where joining blocks with `\n` broke serde_json parsing whenever a
    /// server returned a JSON payload plus trailing prose.
    struct MultiBlockListAppsMcp;

    impl Mcp for MultiBlockListAppsMcp {
        async fn call_tool(
            &self,
            name: &str,
            _arguments: Option<Value>,
        ) -> anyhow::Result<ToolCallResult> {
            assert_eq!(name, "list_apps");
            Ok(ToolCallResult {
                content: vec![
                    clickweave_mcp::ToolContent::Text {
                        text: r#"[{"name":"Signal","pid":16024}]"#.to_string(),
                    },
                    clickweave_mcp::ToolContent::Text {
                        text: "(rendered from cached process table)".to_string(),
                    },
                ],
                is_error: None,
            })
        }
        fn has_tool(&self, name: &str) -> bool {
            name == "list_apps"
        }
        fn tools_as_openai(&self) -> Vec<Value> {
            Vec::new()
        }
        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn pid_resolves_to_app_name_even_with_trailing_prose_block() {
        let arguments = serde_json::json!({ "pid": 16024 });
        let resolved = StateRunner::resolve_cdp_target(
            &arguments,
            "Window focused successfully",
            &MultiBlockListAppsMcp,
        )
        .await;
        assert_eq!(resolved, Some(("Signal".to_string(), None)));
    }
}

#[cfg(test)]
mod focus_skip_tests {
    //! Ported verbatim from the focus_window skip guard section of the
    //! legacy runner's observation-union tests for Task 3a.7.d. Exercises
    //! `StateRunner::should_skip_focus_window` and its two sister
    //! predicates (`is_synthetic_focus_skip`, `mcp_has_toolset`) against
    //! the same matrix of kind / toolset / CDP-liveness / policy cases
    //! the legacy `AgentRunner` suite pinned.
    use super::*;
    use clickweave_mcp::ToolCallResult;

    /// Minimal `Mcp` stub used to exercise the focus_window skip guard.
    /// Only `has_tool` is consulted by
    /// [`StateRunner::should_skip_focus_window`] — `call_tool` /
    /// `tools_as_openai` / `refresh_server_tool_list` are never reached
    /// in these unit tests but must exist to satisfy the trait bound.
    struct ToolsetStub {
        tools: Vec<String>,
    }

    impl ToolsetStub {
        fn with(tools: &[&str]) -> Self {
            Self {
                tools: tools.iter().map(|s| s.to_string()).collect(),
            }
        }
    }

    impl crate::executor::Mcp for ToolsetStub {
        async fn call_tool(
            &self,
            _name: &str,
            _arguments: Option<Value>,
        ) -> anyhow::Result<ToolCallResult> {
            unimplemented!("focus_window skip guard does not dispatch tools")
        }

        fn has_tool(&self, name: &str) -> bool {
            self.tools.iter().any(|t| t == name)
        }

        fn tools_as_openai(&self) -> Vec<Value> {
            Vec::new()
        }

        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Fresh runner pre-seeded with one app/kind hint for guard tests.
    fn runner_with_kind(app_name: &str, kind: &str) -> StateRunner {
        let mut runner = StateRunner::new_for_test("test-goal".to_string());
        runner.record_app_kind(app_name, kind);
        runner
    }

    const FULL_AX_TOOLSET: &[&str] = &["take_ax_snapshot", "ax_click", "ax_set_value", "ax_select"];

    #[test]
    fn mcp_has_toolset_requires_every_member() {
        // Missing even one member blocks the guard. The guard only fires
        // when the full macOS AX dispatch toolset is present; on Windows
        // and on older MCP servers the set is incomplete and
        // focus_window still matters.
        let mcp_full = ToolsetStub::with(FULL_AX_TOOLSET);
        assert!(mcp_has_toolset(&mcp_full, FULL_AX_TOOLSET));

        for (i, missing) in FULL_AX_TOOLSET.iter().enumerate() {
            let partial: Vec<&str> = FULL_AX_TOOLSET
                .iter()
                .enumerate()
                .filter_map(|(j, t)| (j != i).then_some(*t))
                .collect();
            let mcp = ToolsetStub::with(&partial);
            assert!(
                !mcp_has_toolset(&mcp, FULL_AX_TOOLSET),
                "toolset without {} must not count as full AX toolset",
                missing,
            );
        }
    }

    #[test]
    fn should_skip_focus_window_fires_for_known_native_with_full_ax_toolset() {
        // Baseline happy path: MCP exposes the full AX toolset AND we've
        // already seen that the target is Native — suppress focus_window
        // to keep the user's foreground undisturbed.
        let runner = runner_with_kind("Calculator", "Native");
        let mcp = ToolsetStub::with(FULL_AX_TOOLSET);
        let args = serde_json::json!({"app_name": "Calculator"});
        assert_eq!(
            runner.should_skip_focus_window(&args, &mcp),
            Some(FocusSkipReason::AxAvailable),
        );
    }

    #[test]
    fn should_skip_focus_window_defers_for_electron_or_chrome_without_live_cdp() {
        // Broader contract (see `should_skip_focus_window`): Electron /
        // Chrome apps DO qualify for the skip, but only after CDP is
        // live for that exact app. When no CDP session is bound yet,
        // the first `focus_window` call often precedes `cdp_connect`
        // and may be needed to bring the window front so the debug
        // port is discoverable. Without CDP live, the guard must defer
        // regardless of which dispatch toolset the MCP server exposes.
        //
        // NOTE: this test previously asserted that Electron / Chrome
        // apps were NEVER skipped. That narrower contract was relaxed
        // when CDP dispatch became the dominant path for these apps.
        // The test now covers the pre-CDP-connect half of the broader
        // contract; the post-CDP-connect half is covered by
        // `should_skip_focus_window_fires_for_electron_with_live_cdp`.
        // AX + CDP toolsets both present — the only thing missing is
        // the live CDP session, which is the point.
        let mcp = ToolsetStub::with(&[
            "take_ax_snapshot",
            "ax_click",
            "ax_set_value",
            "ax_select",
            "cdp_find_elements",
            "cdp_click",
        ]);
        for kind in ["ElectronApp", "ChromeBrowser"] {
            let runner = runner_with_kind("VSCode", kind);
            let args = serde_json::json!({"app_name": "VSCode"});
            assert!(
                runner.should_skip_focus_window(&args, &mcp).is_none(),
                "focus_window must NOT be skipped for kind={} without a live CDP session",
                kind,
            );
        }
    }

    /// Seed a runner with a kind hint AND an active CDP session bound
    /// to the same app — the on-the-wire state the agent reaches after
    /// `launch_app` + successful `cdp_connect`. Delegates to
    /// [`StateRunner::seed_cdp_live_for_test`] so the "post-`on_cdp_connected`
    /// state shape" has a single source of truth.
    fn runner_with_kind_and_cdp(app_name: &str, kind: &str) -> StateRunner {
        let mut runner = StateRunner::new_for_test("test-goal".to_string());
        runner.seed_cdp_live_for_test(app_name, kind);
        runner
    }

    const FULL_CDP_TOOLSET: &[&str] = &["cdp_find_elements", "cdp_click"];

    #[test]
    fn should_skip_focus_window_fires_for_electron_with_live_cdp() {
        // CDP dispatch operates on backgrounded windows without stealing
        // focus, so once a session is live for the exact app, the real
        // `focus_window` is redundant and the guard must fire.
        let runner = runner_with_kind_and_cdp("Signal", "ElectronApp");
        let mcp = ToolsetStub::with(FULL_CDP_TOOLSET);
        let args = serde_json::json!({"app_name": "Signal"});
        assert_eq!(
            runner.should_skip_focus_window(&args, &mcp),
            Some(FocusSkipReason::CdpLive),
        );
    }

    #[test]
    fn should_skip_focus_window_fires_for_chrome_browser_with_live_cdp() {
        // Same contract as the Electron path — ChromeBrowser targets
        // go through CDP and must be suppressed when a session is live.
        let runner = runner_with_kind_and_cdp("Google Chrome", "ChromeBrowser");
        let mcp = ToolsetStub::with(FULL_CDP_TOOLSET);
        let args = serde_json::json!({"app_name": "Google Chrome"});
        assert_eq!(
            runner.should_skip_focus_window(&args, &mcp),
            Some(FocusSkipReason::CdpLive),
        );
    }

    #[test]
    fn should_skip_focus_window_defers_for_electron_when_cdp_not_connected() {
        // Kind hint + full CDP toolset but NO live session — the first
        // focus_window often precedes cdp_connect and may itself be
        // what brings the window front so the debug port is findable.
        // The guard must defer here.
        let runner = runner_with_kind("Signal", "ElectronApp");
        let mcp = ToolsetStub::with(FULL_CDP_TOOLSET);
        let args = serde_json::json!({"app_name": "Signal"});
        assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
    }

    #[test]
    fn should_skip_focus_window_defers_for_electron_when_cdp_tools_missing() {
        // CDP is live but the MCP server does not advertise the CDP
        // dispatch toolset (older server, stripped build). Without
        // cdp_find_elements / cdp_click the agent cannot drive the
        // target via CDP, so coordinate-based tools — which DO need
        // focus — are the likely fallback. The guard must defer.
        let runner = runner_with_kind_and_cdp("Signal", "ElectronApp");
        // Only cdp_find_elements, missing cdp_click.
        let mcp = ToolsetStub::with(&["cdp_find_elements"]);
        let args = serde_json::json!({"app_name": "Signal"});
        assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
    }

    #[test]
    fn should_skip_focus_window_defers_when_cdp_bound_to_other_app() {
        // A live CDP session bound to a different app must not authorize
        // a skip for this one — the name scope of `is_connected_to` is
        // load-bearing.
        let mut runner = StateRunner::new_for_test("test-goal".to_string());
        runner.record_app_kind("Signal", "ElectronApp");
        runner.cdp_state.set_connected("Slack", 0);
        let mcp = ToolsetStub::with(FULL_CDP_TOOLSET);
        let args = serde_json::json!({"app_name": "Signal"});
        assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
    }

    #[test]
    fn should_skip_focus_window_defers_when_kind_unknown() {
        // First-ever focus: no prior probe / structured response, so we
        // can't classify the app. The task is explicit about erring on
        // the side of executing focus_window normally in this case —
        // breaking Electron / Windows workflows is strictly worse than
        // a single preserved focus-steal on the first call.
        let runner = StateRunner::new_for_test("test-goal".to_string());
        let mcp = ToolsetStub::with(FULL_AX_TOOLSET);
        let args = serde_json::json!({"app_name": "MysteryApp"});
        assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
    }

    #[test]
    fn should_skip_focus_window_defers_when_ax_toolset_incomplete() {
        // Windows / older MCP servers surface only a partial toolset.
        // Without ax_click / ax_set_value / ax_select, the agent cannot
        // drive the target via AX and `focus_window` is still required.
        let runner = runner_with_kind("Calculator", "Native");
        // Only take_ax_snapshot — no dispatch primitives.
        let mcp = ToolsetStub::with(&["take_ax_snapshot"]);
        let args = serde_json::json!({"app_name": "Calculator"});
        assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
    }

    #[test]
    fn should_skip_focus_window_requires_app_name_in_args() {
        // window_id / pid-only focus_window variants are ambiguous; we
        // can't map them to a recorded kind, so the guard must not
        // fire. resolve_cdp_target's list_apps / list_windows path
        // still runs the real tool, which is the correct behavior.
        let runner = runner_with_kind("Calculator", "Native");
        let mcp = ToolsetStub::with(FULL_AX_TOOLSET);
        let args = serde_json::json!({"window_id": 42});
        assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
    }

    #[test]
    fn is_synthetic_focus_skip_matches_only_the_sentinels() {
        // Post-step bookkeeping gates CDP auto-connect and workflow-node
        // creation on this predicate — it must be tight enough that a
        // real focus_window success never masquerades as a skip, yet
        // match every FocusSkipReason variant so none of the runner's
        // suppressions leak into the workflow graph.
        for reason in FocusSkipReason::ALL {
            assert!(
                StateRunner::is_synthetic_focus_skip("focus_window", reason.llm_message()),
                "focus_window + {:?} message must register as synthetic skip",
                reason,
            );
            assert!(
                !StateRunner::is_synthetic_focus_skip("launch_app", reason.llm_message()),
                "non-focus_window tool with {:?} message must not register",
                reason,
            );
        }
        // Different result text — a real MCP success must not be
        // treated as skipped.
        assert!(!StateRunner::is_synthetic_focus_skip(
            "focus_window",
            "Window focused successfully",
        ));
    }

    #[test]
    fn should_skip_focus_window_respects_allow_focus_window_policy() {
        // Policy takes precedence over every kind / toolset branch: when
        // `allow_focus_window == false`, the predicate must return the
        // policy sentinel even for cases that would otherwise defer
        // (unknown kind, missing toolset, missing app_name, CDP-not-live).
        // The returned skip text is the LLM-facing nudge toward AX / CDP
        // dispatch primitives.
        let mut runner = StateRunner::new(
            "test-goal".to_string(),
            AgentConfig {
                allow_focus_window: false,
                ..Default::default()
            },
        );
        let mcp_empty = ToolsetStub::with(&[]);

        // 1. Unknown app kind, empty toolset — would normally defer.
        let args_named = serde_json::json!({"app_name": "MysteryApp"});
        assert_eq!(
            runner.should_skip_focus_window(&args_named, &mcp_empty),
            Some(FocusSkipReason::PolicyDisabled),
        );

        // 2. Missing app_name (window_id / pid-only form) — the kind /
        // toolset branches always defer here, but policy overrides.
        let args_windowed = serde_json::json!({"window_id": 42});
        assert_eq!(
            runner.should_skip_focus_window(&args_windowed, &mcp_empty),
            Some(FocusSkipReason::PolicyDisabled),
        );

        // 3. Electron kind hint but no live CDP session — normally
        // defers because the first focus_window often precedes
        // cdp_connect. Policy overrides.
        runner.record_app_kind("Signal", "ElectronApp");
        let args_electron = serde_json::json!({"app_name": "Signal"});
        assert_eq!(
            runner.should_skip_focus_window(&args_electron, &mcp_empty),
            Some(FocusSkipReason::PolicyDisabled),
        );

        // 4. Default config still behaves as before — sanity check the
        // feature is truly opt-in and the unknown-kind defer path is
        // preserved.
        let default_runner = StateRunner::new_for_test("test-goal".to_string());
        assert!(
            default_runner
                .should_skip_focus_window(&args_named, &mcp_empty)
                .is_none(),
            "default policy (allow_focus_window=true) must preserve the \
             existing defer-for-unknown-kind behavior",
        );
    }

    #[test]
    fn record_app_kind_overwrites_previous_value_for_same_app() {
        // Apps can transition between kinds across runs (e.g. a Chrome
        // profile that used to be launched plain and is now launched
        // with --remote-debugging-port). The latest hint must win so
        // the guard reflects the current lifecycle, not history.
        let mut runner = StateRunner::new_for_test("test-goal".to_string());
        runner.record_app_kind("Calculator", "Native");
        runner.record_app_kind("Calculator", "ElectronApp");
        let mcp = ToolsetStub::with(FULL_AX_TOOLSET);
        let args = serde_json::json!({"app_name": "Calculator"});
        // Electron now — guard must NOT fire.
        assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
    }
}

/// Test-only re-exports for Task 3a.6 unit tests that need access to the
/// otherwise-private CDP classifier helpers. Keeps the helpers private on
/// the production surface while letting the integration tests exercise
/// them directly.
#[cfg(test)]
pub(crate) mod test_support {
    use serde_json::Value;

    use super::{FocusSkipReason, StateRunner};
    use crate::executor::Mcp;

    pub(crate) fn call_should_skip_focus_window<M: Mcp + ?Sized>(
        runner: &StateRunner,
        arguments: &Value,
        mcp: &M,
    ) -> Option<FocusSkipReason> {
        runner.should_skip_focus_window(arguments, mcp)
    }

    pub(crate) async fn call_maybe_cdp_connect<M: Mcp + ?Sized>(
        runner: &mut StateRunner,
        tool_name: &str,
        arguments: &Value,
        result_text: &str,
        mcp: &M,
    ) {
        runner
            .maybe_cdp_connect(tool_name, arguments, result_text, mcp)
            .await;
    }
}
