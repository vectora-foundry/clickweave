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
    StepOutcome, TerminalReason,
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
    /// same run do not overwrite each other. Mirrors
    /// `AgentRunner::verification_count` (`loop_runner.rs:189`).
    pub verification_count: u32,
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
    /// `last_native_ax_snapshot` body. Port of
    /// `loop_runner.rs::enrich_ax_descriptor` — D15 moves the source of
    /// truth off the transcript onto `WorldModel`.
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
    /// Minimum port of `AgentRunner::fetch_elements` for the Task 3a.1
    /// skeleton: calls `cdp_find_elements` when the tool is available,
    /// parses the response into `CdpFindElementMatch`es, updates
    /// `state.current_url`, and returns the parsed matches. Errors and
    /// missing-tool paths return an empty vec so the rest of the loop
    /// degrades gracefully.
    ///
    /// TODO(task-3a.4): surface schema-drift parse failures through a
    /// `Warning` event so the operator can tell an empty page apart from a
    /// wire-format drift.
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
    pub async fn run_turn<E: ToolExecutor + ?Sized>(
        &mut self,
        turn: &AgentTurn,
        executor: &E,
    ) -> (TurnOutcome, Vec<String>) {
        // 1. Apply mutations first — phase inference reads the stack/watch state.
        let warnings = self.apply_mutations(&turn.mutations);

        // 2. Observe: drain pending events + re-infer phase.
        self.observe();

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

        (outcome, warnings)
    }
}

/// Control signal returned from [`StateRunner::try_replay_cache`].
///
/// Mirrors `loop_runner::ReplayResult` semantics: `Continue` means the
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

/// Observation tools whose cached entries are stale on read. Mirrors
/// `loop_runner::OBSERVATION_TOOLS` — duplicated here because the legacy
/// list is a private `const` on `AgentRunner`, and lifting it to a shared
/// module is out of scope for Task 3a.2 (refactoring pass owned by 3b).
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
/// `take_ax_snapshot`. See `loop_runner::AX_DISPATCH_TOOLS`.
const AX_DISPATCH_TOOLS: &[&str] = &["ax_click", "ax_set_value", "ax_select"];

/// Tools that transition app / window / CDP state. Their cache key
/// reflects the pre-state, so replay would fire the transition a second
/// time on unchanged elements. See `loop_runner::STATE_TRANSITION_TOOLS`.
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
fn is_observation_tool(
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

fn is_ax_dispatch_tool(tool_name: &str) -> bool {
    AX_DISPATCH_TOOLS.contains(&tool_name)
}

fn is_state_transition_tool(tool_name: &str) -> bool {
    STATE_TRANSITION_TOOLS.contains(&tool_name)
}

/// Build an index from tool name → MCP annotations from the openai-
/// shaped tool list. Tools without an `annotations` block produce the
/// default (all-`None`) struct. Mirrors `loop_runner::build_annotations_index`.
fn build_annotations_index(mcp_tools: &[Value]) -> HashMap<String, ToolAnnotations> {
    let mut index = HashMap::with_capacity(mcp_tools.len());
    for tool in mcp_tools {
        let name = tool
            .get("function")
            .and_then(|f| f.get("name"))
            .or_else(|| tool.get("name"))
            .and_then(|v| v.as_str());
        let Some(name) = name else {
            continue;
        };
        index.insert(name.to_string(), ToolAnnotations::from_tool_json(tool));
    }
    index
}

/// Join all text content from a `ToolCallResult` into a single string —
/// this is the body the LLM sees in the `tool_result` message.
fn extract_result_text(result: &clickweave_mcp::ToolCallResult) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(result.content.len());
    for content in &result.content {
        match content {
            clickweave_mcp::ToolContent::Text { text } => parts.push(text.clone()),
            clickweave_mcp::ToolContent::Image { mime_type, .. } => {
                parts.push(format!("[image: {}]", mime_type));
            }
            clickweave_mcp::ToolContent::Unknown(_) => {
                parts.push("[unknown content]".to_string());
            }
        }
    }
    parts.join("\n")
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

    /// Prompt the operator for approval of a tool action. Port of
    /// `AgentRunner::request_approval` (`loop_runner.rs:1525-1565`). Returns
    /// `None` when no approval gate is configured (auto-approve).
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
    /// the VLM. Port of `AgentRunner::verify_completion`
    /// (`loop_runner.rs:1580-1660`).
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

        // StateRunner does not yet track a `CdpState` — Task 3a.6 adds
        // auto-CDP-connect plumbing that will populate `connected_app`.
        // Until then, the scope falls back to full-screen capture, which
        // every MCP surface accepts.
        let scope = pick_completion_screenshot_scope(None);
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
    /// instead of asking the LLM. Port of
    /// `AgentRunner::try_replay_cache` (`loop_runner.rs:748-1007`) —
    /// preserves every branch of the legacy semantics per D11.
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
        // `mcp_tools` is threaded through for Task 3a.5's
        // `add_workflow_node` — the tool-to-NodeType mapping consults the
        // advertised tool schemas. Unused in the 3a.2 stub but kept in the
        // signature so the 3a.5 wiring is a parameter-name change only.
        _mcp_tools: &[Value],
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

                // TODO(task-3a.5): rebuild workflow node + emit NodeAdded /
                // EdgeAdded for the replayed call. Until 3a.5 lands the
                // cross-run lineage (cache.produced_node_ids) is not
                // augmented; the replay still bumps hit_count below so
                // the JSON field changes in a D11-compatible way.
                let produced_node_id_on_replay: Option<uuid::Uuid> = None;
                if let Some(node_id) = produced_node_id_on_replay
                    && let Some(entry) = self.cache.entries.get_mut(&current_key)
                {
                    entry.produced_node_ids.push(node_id);
                }
                if let Some(entry) = self.cache.entries.get_mut(&current_key) {
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
                    summary: crate::agent::prompt_spine::truncate_summary(&result_text, 120),
                })
                .await;

                // TODO(task-3a.6): auto-CDP-connect after a cached
                // launch_app / focus_window replay. State-transition
                // tools already fall through above (branch 4c), so the
                // only way this hook matters today is if the write-side
                // filter ever relaxes — kept as an explicit marker so
                // 3a.6 can grep for it.

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

                // TODO(task-3a.4): destructive-cap accounting for cached
                // replays — mirrors `AgentRunner::maybe_halt_on_destructive_cap`.
                // Without the cap, successive destructive replays would
                // not halt via this path until the live-LLM tail reaches
                // the same guard. State-transition tools (the common
                // destructive case) already fall through at branch 4c so
                // this gap is narrow.
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
/// `TODO(task-3a.2)`: extend to read a structured `{ mutations, action }`
/// JSON envelope when the prompt spine asks the LLM for one.
pub fn parse_agent_turn(message: &Message) -> anyhow::Result<AgentTurn> {
    let tool_calls = message
        .tool_calls
        .as_ref()
        .and_then(|tcs| tcs.first())
        .context(
            "LLM response had no tool_calls; state-spine requires one action per turn. \
             TODO(task-3a.4): map a text-only response to AgentReplan with the raw text.",
        )?;

    let name = &tool_calls.function.name;
    let args = tool_calls.function.arguments.clone();

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
            tool_call_id: tool_calls.id.clone(),
        },
    };

    Ok(AgentTurn {
        mutations: Vec::new(),
        action,
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
        match self.mcp.call_tool(tool_name, Some(arguments.clone())).await {
            Ok(result) if result.is_error != Some(true) => {
                let text = result
                    .content
                    .iter()
                    .filter_map(|c| c.as_text())
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(text)
            }
            Ok(result) => {
                let text = result
                    .content
                    .iter()
                    .filter_map(|c| c.as_text())
                    .collect::<Vec<_>>()
                    .join("\n");
                Err(text)
            }
            Err(e) => Err(format!("{}", e)),
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
        variant_context: Option<&str>,
        mcp_tools: Vec<Value>,
        anchor_node_id: Option<uuid::Uuid>,
        prior_turns: &[crate::agent::prior_turns::PriorTurn],
    ) -> anyhow::Result<(AgentState, AgentCache)>
    where
        B: ChatBackend + ?Sized,
        M: Mcp + ?Sized,
    {
        use crate::agent::context_spine::{CompactBudget, compact};
        use crate::agent::prior_turns::build_goal_with_prior_turns;
        use crate::agent::prompt_spine::{build_system_prompt, build_user_turn_message};

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
        let tool_list_for_prompt = openai_tools_to_mcp_tool_list(&mcp_tools);
        let mut system_text = build_system_prompt(&tool_list_for_prompt);
        if let Some(ctx) = variant_context {
            system_text.push_str(&format!("\n\nVariant context: {}", ctx));
        }

        // Compose the goal with inlined prior-turn log. Keeps messages[1]
        // (the goal slot) stable across compaction (D12).
        let composed_goal = build_goal_with_prior_turns(&goal, prior_turns, 1000);
        let initial_user =
            build_user_turn_message(&self.world_model, &self.task_state, 0, &composed_goal);

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
            // TODO(task-3a.6): pre-step CDP maybe-connect here.

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

            // 4a. Permission policy + approval gate for live `ToolCall`
            // actions. Mirrors `AgentRunner::execute_response`'s pre-
            // dispatch policy check (`loop_runner.rs:1964-2013`). The
            // cache-replay path has its own identical gate at
            // `try_replay_cache`; observation tools bypass approval
            // entirely on both paths.
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
            let executor = McpToolExecutor { mcp };
            let (outcome, warnings) = self.run_turn(&turn, &executor).await;
            for w in warnings {
                tracing::warn!(warning = %w, "state-spine: mutation warning");
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
                    self.state.steps.push(AgentStep {
                        index: self.state.steps.len(),
                        elements: elements.clone(),
                        command,
                        outcome: step_outcome,
                        page_url: self.state.current_url.clone(),
                    });
                    previous_result = Some(tool_body);
                    // TODO(task-3a.5): add_workflow_node (NodeAdded /
                    // EdgeAdded emission) goes here.
                    // TODO(task-3a.4): destructive-cap accounting goes here.
                    // TODO(task-3a.6): auto_connect_cdp + synthetic
                    // focus_window skip go here.
                }
                TurnOutcome::ToolError { tool_name, error } => {
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
                            StepOutcome::Error(error.clone()),
                        ),
                        _ => unreachable!("ToolError outcome implies ToolCall action"),
                    };
                    self.state.steps.push(AgentStep {
                        index: self.state.steps.len(),
                        elements: elements.clone(),
                        command,
                        outcome: step_outcome,
                        page_url: self.state.current_url.clone(),
                    });
                    self.state.consecutive_errors = self.consecutive_errors;
                    previous_result = Some(error);
                    // TODO(task-3a.4): loop detection (same-tool/same-error
                    // short-circuit) goes here.
                    // TODO(task-3a.6): recovery strategy goes here.
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
                    self.state.terminal_reason = Some(TerminalReason::Completed { summary });
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

        // TODO(task-3a.6.5): write the terminal `StepRecord` through the
        // shared `RunStorage` handle here.

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
/// mirroring `loop_runner::AgentRunner::append_assistant_message`.
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
    if let Some(tool_calls) = &assistant.tool_calls {
        if let Some(tc) = tool_calls.first() {
            messages.push(Message::assistant_tool_calls(vec![tc.clone()]));
            let result_text = previous_result.unwrap_or("ok");
            messages.push(Message::tool_result(&tc.id, result_text));
        }
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
