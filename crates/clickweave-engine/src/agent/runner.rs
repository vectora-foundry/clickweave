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

use std::collections::{HashMap, VecDeque};
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
use crate::agent::skills::{
    RecordedStep, RetrievedSkill, SkillContext, SkillFrame, SkillIndex, SkillStore,
    SubgoalSignature,
};
use crate::agent::task_state::{Milestone, SubgoalId, TaskState, TaskStateMutation};
use crate::agent::types::{
    AgentCommand, AgentConfig, AgentEvent, AgentState, AgentStep, ApprovalRequest, RunnerOutput,
    StepOutcome, TerminalReason, WorldModelDiff,
};
use crate::agent::world_model::{
    CdpElementInventorySummary, InvalidationEvent, ObservedElement, WorldModel,
};
use crate::executor::Mcp;

#[derive(Debug, Default)]
pub(crate) struct CdpPageObservation {
    pub page_url: String,
    pub page_fingerprint: String,
    pub inventory: Vec<CdpElementInventorySummary>,
}

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
    /// Replay a procedural skill listed in the previous turn's
    /// `<applicable_skills>` block. The harness expands the skill's
    /// recorded action sketch through the same dispatch helper as live
    /// tool calls so the safety surface is identical.
    InvokeSkill {
        skill_id: String,
        version: u32,
        parameters: serde_json::Value,
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
/// control loop exercises plus compatibility fields needed by the public
/// `run_agent_workflow` seam. Fields the live tests don't touch are covered by
/// the module-wide `#![allow(dead_code)]`.
pub struct StateRunner {
    // --- Core state-spine fields ---
    pub world_model: WorldModel,
    pub task_state: TaskState,
    pub step_index: usize,
    pub consecutive_errors: usize,
    pub last_replan_step: Option<usize>,
    pub pending_events: Vec<InvalidationEvent>,

    // --- Compatibility fields ---
    // Carried so the public seam can change without silently dropping
    // what callers rely on today.
    pub config: AgentConfig,
    pub state: AgentState,
    pub workflow: clickweave_core::Workflow,
    pub last_node_id: Option<uuid::Uuid>,
    pub recent_destructive_tools: Vec<String>,

    // --- Collaborators (builder-style) ---
    pub storage: Option<std::sync::Arc<std::sync::Mutex<clickweave_core::storage::RunStorage>>>,
    pub run_id: uuid::Uuid,
    /// Live event channel. When `None` the runner runs silently.
    pub event_tx: Option<mpsc::Sender<RunnerOutput>>,
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

    /// World-model field signatures captured by the top-level `run` loop
    /// before it mirrors the observe-phase CDP results into
    /// `world_model.elements` / `world_model.cdp_page`. When `Some`,
    /// `run_turn` uses this as the baseline for its `WorldModelChanged`
    /// diff so direct-observation writes (which happen outside
    /// `run_turn`) still surface in `changed_fields`. When `None`, the
    /// test/unit caller path is in effect and `run_turn` falls back to
    /// snapshotting signatures itself immediately before `observe()`.
    pub(crate) turn_pre_signatures: Option<Vec<(&'static str, Option<usize>)>>,

    // --- Spec 2 episodic-memory fields (Phase 3) ---
    pub(crate) episodic_ctx: crate::agent::episodic::EpisodicContext,
    pub(crate) episodic_store: Option<std::sync::Arc<crate::agent::episodic::SqliteEpisodicStore>>,
    pub(crate) episodic_global: Option<std::sync::Arc<crate::agent::episodic::SqliteEpisodicStore>>,
    pub(crate) episodic_writer: Option<crate::agent::episodic::EpisodicWriter>,
    pub(crate) recovering_snapshot: Option<crate::agent::episodic::types::RecoveringEntrySnapshot>,
    pub(crate) recovery_actions_accumulator: Vec<crate::agent::episodic::types::CompactAction>,
    pub(crate) last_failed_tool_name: Option<String>,
    pub(crate) last_failed_error_kind: Option<String>,
    /// Cached events.jsonl path for the active execution; resolved
    /// lazily when retrieval needs to populate
    /// `RecoveringEntrySnapshot::events_jsonl_ref`.
    pub(crate) episodic_events_ref: Option<String>,
    /// Authoritative gate for D24 run-start retrieval: set true the
    /// first time `try_retrieve_episodic` reaches its trigger-decision
    /// slot, regardless of whether retrieval returned hits. Decoupled
    /// from `step_index` so synthetic-skip / policy-deny / approval-reject
    /// paths cannot let `step_index == 0` re-fire
    /// run-start retrieval after the run has already taken actions.
    pub(crate) episodic_run_start_retrieved: bool,

    // --- Spec 3 procedural-skills fields (Phase 3) ---
    /// Boundary metadata threaded in from the Tauri layer (project +
    /// global skills directories, project id, master enable flag). Phase
    /// 3 reads these to construct the `SkillIndex` and gate
    /// extraction / retrieval. A `disabled` context turns every skill
    /// hook into a no-op.
    pub(crate) skill_ctx: SkillContext,
    /// Per-run skill index, shared with the file-watcher consumer.
    /// Built once at runner construction and rebuilt across runs only
    /// (never mid-run — file events flip individual entries via the
    /// watcher consumer).
    pub(crate) skill_index: Arc<parking_lot::RwLock<SkillIndex>>,
    /// On-disk store backing `skill_index`. Carried as an `Arc` so the
    /// extractor (Phase 3) and watcher consumer (Phase 2) can share the
    /// recently-written-tolerance table without duplicating writes.
    pub(crate) skill_store: Arc<SkillStore>,
    /// Optional in-memory accumulator of every successful tool call this
    /// run, keyed by step. Drained by `maybe_extract_skill` at every
    /// `CompleteSubgoal` boundary against the `[push_idx..]` window.
    /// Cleared at run-terminal so the runner can in theory be reused.
    pub(crate) recorded_steps: Vec<RecordedStep>,
    /// Snapshot of `world_model` taken just after `observe()` at the
    /// top of the current loop iteration. Used as the `world_model_pre`
    /// when a successful tool dispatch produces a `RecordedStep`.
    pub(crate) pre_dispatch_snapshot: Option<crate::agent::step_record::WorldModelSnapshot>,
    /// Stack of `recorded_steps.len()` indices captured at every
    /// `PushSubgoal` mutation. Each `CompleteSubgoal` pops the top so
    /// the extractor can address the action sketch by step range
    /// (`recorded_steps[push_idx..]`). Mirrors `task_state.subgoal_stack`
    /// in depth.
    pub(crate) push_idx_stack: Vec<usize>,
    /// Stack of subgoal signatures captured at `PushSubgoal` time. The
    /// extractor must key the skill by the state that made the subgoal
    /// applicable, not by the later post-completion world model.
    pub(crate) push_signature_stack: Vec<SubgoalSignature>,
    /// `SubgoalId`s generated by the most recent batch of mutations.
    /// Populated inside `apply_mutations` and consumed by the retrieval
    /// hook in the outer loop. Cleared on every fresh batch — it must
    /// not span turns.
    pub(crate) last_pushed_subgoal_ids: Vec<SubgoalId>,
    /// `(push_idx, milestone)` queue drained by
    /// `write_subgoal_completed_records` so each completed-subgoal
    /// extraction has both the action-sketch start index and the
    /// milestone payload available without re-walking
    /// `task_state.milestones`.
    pub(crate) completed_subgoal_extraction_queue:
        Vec<(usize, Milestone, SubgoalSignature, Vec<uuid::Uuid>)>,
    /// Workflow node ids emitted via `AgentEvent::NodeAdded`, tracked
    /// per active subgoal frame. A produced node belongs to every open
    /// frame so nested subgoals keep their local lineage while parent
    /// subgoals still include all nodes produced during their lifetime.
    pub(crate) produced_node_ids_stack: Vec<Vec<uuid::Uuid>>,
    /// Top-k applicable skills surfaced for the next user turn.
    /// Populated by the retrieval hook on `push_subgoal`, consumed +
    /// cleared by `build_user_turn_message`'s caller at the next
    /// iteration.
    pub(crate) pending_applicable_skills: Vec<RetrievedSkill>,
    /// Optional eval-only override for the stable system-prompt header.
    /// Production callers leave this as `None` and use the file-backed
    /// default in `prompts/agent_system.md`.
    pub(crate) agent_system_prompt_override: Option<String>,
    /// Frame held while the runner is waiting on an LLM fallback turn
    /// during a skill replay. Phase 3 always leaves this `None`; Phase
    /// 4 lands the real consumer.
    pub(crate) suspended_skill_frame: Option<SkillFrame>,
    /// Join handle for the file-watcher consumer task spawned at run
    /// start. Aborted at run-terminal so the consumer doesn't outlive
    /// the runner. `None` when skills are disabled or the watcher
    /// failed to spawn.
    pub(crate) skill_watcher_handle: Option<tokio::task::JoinHandle<()>>,
}

impl StateRunner {
    pub fn new(goal: String, config: AgentConfig) -> Self {
        Self::new_with_episodic(
            goal,
            config,
            crate::agent::episodic::EpisodicContext::disabled(),
        )
    }

    /// Construct a runner with an explicit Spec 2 [`EpisodicContext`].
    ///
    /// Production callers go through this constructor; the legacy
    /// [`Self::new`] is preserved for the many integration tests that
    /// don't care about episodic memory and pass the disabled context
    /// implicitly.
    ///
    /// SQLite stores are opened here (they don't need the event channel
    /// or run_id), but the [`EpisodicWriter`] is deferred to
    /// [`Self::with_episodic_writer`] so it can capture the channel +
    /// run_id seeded by [`Self::with_events`] / [`Self::with_run_id`]
    /// — without those the writer's emitted events would fail the
    /// frontend's stale-run filter.
    pub fn new_with_episodic(
        goal: String,
        config: AgentConfig,
        episodic_ctx: crate::agent::episodic::EpisodicContext,
    ) -> Self {
        Self::new_with_episodic_and_skills(goal, config, episodic_ctx, SkillContext::disabled())
    }

    /// Construct a runner with both an explicit Spec 2 [`EpisodicContext`]
    /// and an explicit Spec 3 [`SkillContext`].
    ///
    /// Production callers go through this constructor once Phase 3 lands
    /// the Tauri-layer wiring; the legacy [`Self::new`] and
    /// [`Self::new_with_episodic`] are preserved for the many integration
    /// tests that don't exercise skills (they pass the disabled context
    /// implicitly).
    ///
    /// When `skill_ctx.enabled == true`, the constructor builds the
    /// `SkillIndex` from the on-disk store. When disabled (or when the
    /// build fails), the runner stores an empty index — extraction +
    /// retrieval become no-ops and the runner still runs end-to-end.
    pub fn new_with_episodic_and_skills(
        goal: String,
        config: AgentConfig,
        episodic_ctx: crate::agent::episodic::EpisodicContext,
        skill_ctx: SkillContext,
    ) -> Self {
        let workflow = clickweave_core::Workflow::default();
        let state = AgentState::new(workflow.clone());

        let (episodic_store, episodic_global) = if episodic_ctx.enabled && config.episodic_enabled {
            use crate::agent::episodic::SqliteEpisodicStore;
            let weights = config.episodic_score_weights.into();
            let halflife = config.episodic_decay_halflife_days;
            let wl = SqliteEpisodicStore::new_with_config(
                    &episodic_ctx.workflow_local_path,
                    crate::agent::episodic::EpisodeScope::WorkflowLocal,
                    weights,
                    halflife,
                    config.episodic_max_per_scope_workflow,
                )
                .map(std::sync::Arc::new)
                .map_err(|e| {
                    tracing::warn!(error = %e, "episodic: failed to open workflow-local store; disabling");
                    e
                })
                .ok();
            let global = episodic_ctx
                .global_path
                .as_ref()
                .and_then(|p| {
                    SqliteEpisodicStore::new_with_config(
                        p,
                        crate::agent::episodic::EpisodeScope::Global,
                        weights,
                        halflife,
                        config.episodic_max_per_scope_global,
                    )
                    .ok()
                })
                .map(std::sync::Arc::new);
            (wl, global)
        } else {
            (None, None)
        };

        // Spec 3: build the skill index when enabled. Failure to build
        // (e.g. unreadable directory entry) drops to an empty index so
        // the runner still runs — skills are best-effort by design.
        let embedder =
            std::sync::Arc::new(crate::agent::episodic::HashedShingleEmbedder::default());
        let skill_index = if skill_ctx.enabled {
            match SkillIndex::build(&skill_ctx, embedder.clone()) {
                Ok(idx) => idx,
                Err(err) => {
                    tracing::warn!(?err, "skills: index build failed; running with empty index");
                    SkillIndex::empty(embedder.clone())
                }
            }
        } else {
            SkillIndex::empty(embedder.clone())
        };
        let skill_store =
            std::sync::Arc::new(SkillStore::new(skill_ctx.project_skills_dir.clone()));
        let skill_index = std::sync::Arc::new(parking_lot::RwLock::new(skill_index));

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
            turn_pre_signatures: None,
            episodic_ctx,
            episodic_store,
            episodic_global,
            episodic_writer: None,
            recovering_snapshot: None,
            recovery_actions_accumulator: Vec::new(),
            last_failed_tool_name: None,
            last_failed_error_kind: None,
            episodic_events_ref: None,
            episodic_run_start_retrieved: false,
            skill_ctx,
            skill_index,
            skill_store,
            recorded_steps: Vec::new(),
            pre_dispatch_snapshot: None,
            push_idx_stack: Vec::new(),
            push_signature_stack: Vec::new(),
            last_pushed_subgoal_ids: Vec::new(),
            completed_subgoal_extraction_queue: Vec::new(),
            produced_node_ids_stack: Vec::new(),
            pending_applicable_skills: Vec::new(),
            agent_system_prompt_override: None,
            suspended_skill_frame: None,
            skill_watcher_handle: None,
        }
    }

    pub fn with_run_id(mut self, run_id: uuid::Uuid) -> Self {
        self.run_id = run_id;
        self
    }

    /// Override the stable system-prompt header. Intended for the eval
    /// harness and prompt-optimization experiments only; production runs
    /// use the checked-in default prompt file.
    pub fn with_agent_system_prompt_override(mut self, prompt: impl Into<String>) -> Self {
        self.agent_system_prompt_override = Some(prompt.into());
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
    pub fn with_events(mut self, tx: mpsc::Sender<RunnerOutput>) -> Self {
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

    /// Spawn the [`EpisodicWriter`] tied to this runner.
    ///
    /// MUST be called after [`Self::with_events`] and [`Self::with_run_id`]:
    /// the writer captures both at spawn so emitted `EpisodeWritten` /
    /// `EpisodePromoted` events carry the live `run_id` and pass the
    /// frontend's stale-run filter. Calling before either silently
    /// skips the writer (so episodic stays best-effort and the agent
    /// run still proceeds — D32).
    pub fn with_episodic_writer(mut self) -> Self {
        if !self.episodic_active() {
            return self;
        }
        let event_tx = self.event_tx.clone();
        // Pass the configured store knobs through to the writer so
        // its workflow-local + global stores honour the same
        // weights / half-life / per-scope caps the runner-side
        // retrieval stores were opened with. The default `spawn`
        // path opens both stores via `SqliteEpisodicStore::new`,
        // which hard-codes the cap to 500.
        let store_config = crate::agent::episodic::store::EpisodicStoreConfig {
            score_weights: self.config.episodic_score_weights.into(),
            decay_halflife_days: self.config.episodic_decay_halflife_days,
            max_per_scope_workflow: self.config.episodic_max_per_scope_workflow,
            max_per_scope_global: self.config.episodic_max_per_scope_global,
        };
        match crate::agent::episodic::EpisodicWriter::spawn_with_config(
            self.episodic_ctx.clone(),
            store_config,
            event_tx,
            self.run_id,
        ) {
            Ok(w) => self.episodic_writer = Some(w),
            Err(e) => tracing::warn!(error = %e, "episodic: failed to spawn writer"),
        }
        self
    }

    /// Whether the episodic memory layer is wired up and active for
    /// this runner. Cheap, side-effect-free; safe to call from hot
    /// paths.
    pub(crate) fn episodic_active(&self) -> bool {
        self.config.episodic_enabled && self.episodic_ctx.enabled && self.episodic_store.is_some()
    }

    /// Resolve the active execution's `events.jsonl` path through
    /// `RunStorage`, caching the result so repeated calls don't take
    /// the storage mutex repeatedly.
    pub(crate) fn current_events_jsonl_ref(&mut self) -> Option<String> {
        if let Some(cached) = &self.episodic_events_ref {
            return Some(cached.clone());
        }
        let storage = self.storage.as_ref()?;
        let guard = storage.lock().ok()?;
        let exec_dir = guard.execution_dir_name()?;
        let path = guard.base_path().join(exec_dir).join("events.jsonl");
        let s = path.to_string_lossy().into_owned();
        self.episodic_events_ref = Some(s.clone());
        Some(s)
    }

    /// Clone the episodic writer's channel sender, if a writer is active.
    ///
    /// The returned sender shares the same worker task as the writer owned
    /// by this runner — no second SQLite connection is opened. Callers can
    /// enqueue `WriteRequest`s (including `PromotePass`) on it even after
    /// `run` has consumed and dropped the runner, as long as they hold the
    /// sender clone. Dropping the clone releases the channel once the
    /// runner's own copy is also gone, allowing the worker to exit.
    ///
    /// Returns `None` when episodic is disabled or the writer was not yet
    /// spawned.
    pub(crate) fn writer_sender(
        &self,
    ) -> Option<tokio::sync::mpsc::Sender<crate::agent::episodic::types::WriteRequest>> {
        self.episodic_writer.as_ref().map(|w| w.sender())
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

    #[cfg(test)]
    pub fn new_for_test(goal: String) -> Self {
        // Test fixtures historically assumed the legacy `allow_focus_window =
        // true` default — flipping the production default to `false` would
        // otherwise force every focus-window unit test to opt back in. The
        // production-default behavior is covered explicitly by
        // `default_config_disables_focus_window_via_policy` below.
        let config = AgentConfig {
            allow_focus_window: true,
            ..AgentConfig::default()
        };
        Self::new(goal, config)
    }

    /// Test-only constructor that wires an enabled `SkillContext` at a
    /// caller-provided directory. Used by Phase 3 e2e tests that need
    /// to drive the extractor + retrieval loop without spinning up a
    /// full Tauri + Mcp + LLM stack.
    #[cfg(test)]
    pub(crate) fn new_for_test_with_skills(goal: String, skills_dir: std::path::PathBuf) -> Self {
        let config = AgentConfig {
            allow_focus_window: true,
            ..AgentConfig::default()
        };
        let skill_ctx = SkillContext {
            enabled: true,
            project_skills_dir: skills_dir,
            global_skills_dir: None,
            project_id: "test".into(),
        };
        Self::new_with_episodic_and_skills(
            goal,
            config,
            crate::agent::episodic::EpisodicContext::disabled(),
            skill_ctx,
        )
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

    /// Spec 2: run an episodic-memory retrieval if the trigger conditions
    /// hold (run-start or `Recovering` entry). On `Recovering` entry,
    /// also captures the [`RecoveringEntrySnapshot`] for the eventual
    /// write at the matching `Recovering -> Executing` exit.
    ///
    /// `prev_phase_at_top` is the phase as it was at the top of the
    /// outer-loop iteration before `observe()` ran, so the
    /// `Exploring/Executing -> Recovering` transition is detectable.
    pub(crate) async fn try_retrieve_episodic(
        &mut self,
        prev_phase_at_top: crate::agent::phase::Phase,
    ) -> Vec<crate::agent::episodic::RetrievedEpisode> {
        use crate::agent::episodic::signature::compute_pre_state_signature;
        use crate::agent::episodic::{
            EpisodicStore as _, RetrievalQuery, RetrievalTrigger, RetrievedEpisode,
        };
        use crate::agent::phase::Phase;

        if !self.episodic_active() {
            return Vec::new();
        }
        let store = match &self.episodic_store {
            Some(s) => s.clone(),
            None => return Vec::new(),
        };

        // D24: run-start retrieval fires once per run, full stop.
        // `episodic_run_start_retrieved` is the authoritative gate (not
        // `step_index == 0`, which lied on synthetic-skip / policy-deny /
        // approval-reject paths because none of those
        // ticked the counter). Marked consumed on first reach so a
        // zero-hit retrieval still counts as "the run-start slot was
        // used" and can never fire a second time.
        let trigger = if !self.episodic_run_start_retrieved {
            self.episodic_run_start_retrieved = true;
            RetrievalTrigger::RunStart
        } else if prev_phase_at_top != Phase::Recovering
            && self.task_state.phase == Phase::Recovering
        {
            RetrievalTrigger::RecoveringEntry
        } else {
            return Vec::new();
        };

        let active_slots: Vec<crate::agent::task_state::WatchSlotName> =
            self.task_state.watch_slots.iter().map(|s| s.name).collect();
        let sig = compute_pre_state_signature(&self.world_model, &active_slots);

        // Capture snapshot at retrieval time so the eventual
        // write uses the same signature.
        if matches!(trigger, RetrievalTrigger::RecoveringEntry) {
            use crate::agent::episodic::types::{RecoveringEntrySnapshot, TriggeringError};
            use crate::agent::step_record::WorldModelSnapshot;
            let events_ref = self.current_events_jsonl_ref();
            let snap = WorldModelSnapshot::from_world_model(&self.world_model);
            self.recovering_snapshot = Some(RecoveringEntrySnapshot {
                entered_at_step: self.step_index,
                world_model_at_entry: snap,
                task_state_at_entry: self.task_state.clone(),
                triggering_error: TriggeringError {
                    failed_tool: self.last_failed_tool_name.clone().unwrap_or_default(),
                    error_kind: self.last_failed_error_kind.clone().unwrap_or_default(),
                    consecutive_errors_at_entry: self.consecutive_errors as u32,
                    step_index: self.step_index,
                },
                workflow_hash: self.episodic_ctx.workflow_hash.clone(),
                pre_state_signature: sig.clone(),
                active_watch_slots: active_slots.clone(),
                events_jsonl_ref: events_ref,
            });
            self.recovery_actions_accumulator.clear();
        }

        let subgoal_owned = self.task_state.subgoal_stack.last().map(|s| s.text.clone());
        let goal_owned = self.task_state.goal.clone();
        let workflow_hash = self.episodic_ctx.workflow_hash.clone();
        let now = chrono::Utc::now();

        let q = RetrievalQuery {
            trigger,
            pre_state_signature: &sig,
            goal: &goal_owned,
            subgoal_text: subgoal_owned.as_deref(),
            workflow_hash: &workflow_hash,
            now,
        };

        let k_each = self.config.retrieved_episodes_k.max(1) * 2;
        let mut wl_hits: Vec<RetrievedEpisode> =
            store.retrieve(&q, k_each).await.unwrap_or_default();

        let g_cap = self.config.episodic_global_cap_per_retrieval.max(1) * 2;
        let mut g_hits: Vec<RetrievedEpisode> = match &self.episodic_global {
            Some(g) => g.retrieve(&q, g_cap).await.unwrap_or_default(),
            None => Vec::new(),
        };

        for h in &mut wl_hits {
            h.score_breakdown.final_score *= self.config.episodic_workflow_priority_multiplier;
        }
        g_hits.truncate(self.config.episodic_global_cap_per_retrieval);

        let mut merged: Vec<RetrievedEpisode> = wl_hits.into_iter().chain(g_hits).collect();
        merged.sort_by(|a, b| {
            crate::agent::episodic::embedder::nan_safe_desc(
                a.score_breakdown.final_score,
                b.score_breakdown.final_score,
            )
        });
        merged.truncate(self.config.retrieved_episodes_k);

        // Emit `EpisodesRetrieved` whenever the retrieval pass returned
        // at least one candidate. Frontends use this to surface the
        // `<retrieved_recoveries>` block before the LLM call lands.
        if !merged.is_empty() {
            use crate::agent::episodic::EpisodeScope;
            let workflow_count = merged
                .iter()
                .filter(|r| matches!(r.scope, EpisodeScope::WorkflowLocal))
                .count();
            let global_count = merged.len() - workflow_count;
            let event = AgentEvent::EpisodesRetrieved {
                run_id: self.run_id,
                trigger,
                count: merged.len(),
                episode_ids: merged
                    .iter()
                    .map(|r| r.episode.episode_id.clone())
                    .collect(),
                scope_breakdown: crate::agent::types::ScopeBreakdown {
                    workflow: workflow_count,
                    global: global_count,
                },
            };
            self.emit_event(event).await;
        }

        merged
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

    /// Apply the batch of task-state mutations from an `AgentTurn`, in
    /// order. Invalid mutations become warnings but do not abort the pass —
    /// subsequent mutations and the action still run. Matches the
    /// error-path table in the spec.
    ///
    /// PushSubgoal / CompleteSubgoal route through the per-mutation
    /// helpers on `TaskState` so the runner can capture the generated
    /// `SubgoalId` (Spec 3 retrieval hook) and the matching push-side
    /// `recorded_steps` index (Spec 3 extractor) without re-walking
    /// the mutation slice. `last_pushed_subgoal_ids` is cleared at the
    /// top of every batch — the retrieval hook reads it once per turn.
    pub fn apply_mutations(&mut self, muts: &[TaskStateMutation]) -> Vec<String> {
        let mut warnings = Vec::new();
        self.last_pushed_subgoal_ids.clear();

        for m in muts {
            match m {
                TaskStateMutation::PushSubgoal { text } => {
                    self.push_idx_stack.push(self.recorded_steps.len());
                    self.push_signature_stack.push(
                        crate::agent::skills::signature::compute_subgoal_signature(
                            text,
                            &self.world_model,
                        ),
                    );
                    let id = self.task_state.apply_push_subgoal(text, self.step_index);
                    self.last_pushed_subgoal_ids.push(id);
                    self.produced_node_ids_stack.push(Vec::new());
                }
                TaskStateMutation::CompleteSubgoal { summary } => {
                    let push_idx = self.push_idx_stack.pop().unwrap_or(0);
                    let push_sig = self.push_signature_stack.pop();
                    let produced_node_ids = self.produced_node_ids_stack.pop().unwrap_or_default();
                    match self
                        .task_state
                        .apply_complete_subgoal(summary, self.step_index)
                    {
                        Ok(milestone) => {
                            let pre_state_sig = push_sig.unwrap_or_else(|| {
                                crate::agent::skills::signature::compute_subgoal_signature(
                                    &milestone.text,
                                    &self.world_model,
                                )
                            });
                            self.completed_subgoal_extraction_queue.push((
                                push_idx,
                                milestone,
                                pre_state_sig,
                                produced_node_ids,
                            ));
                        }
                        Err(e) => warnings.push(format!("{}", e)),
                    }
                }
                other => {
                    if let Err(e) = self.task_state.apply(other, self.step_index) {
                        warnings.push(format!("{}", e));
                    }
                }
            }
        }
        warnings
    }

    fn record_produced_node_id(&mut self, node_id: uuid::Uuid) {
        for produced_node_ids in &mut self.produced_node_ids_stack {
            produced_node_ids.push(node_id);
        }
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

    /// Queue invalidation events that the just-executed tool implies for
    /// the world model. Pure-observation tools (`take_ax_snapshot`,
    /// `take_screenshot`, `cdp_find_elements`, etc.) are no-ops here;
    /// state-transition tools queue the matching event so the next
    /// `observe()` call drops fields that the tool may have invalidated.
    ///
    /// Categories:
    /// - **Focus shift** (`focus_window`): drops focused-app, window list,
    ///   element surface, modal/dialog, screenshot, AX snapshot.
    /// - **App lifecycle** (`launch_app`, `quit_app`): same as focus shift.
    /// - **CDP navigation** (`cdp_navigate`, `cdp_new_page`,
    ///   `cdp_select_page`): drops the CDP page state, element surface,
    ///   and modal/dialog presence.
    ///
    /// Snapshot-staleness invalidation is event-driven from a separate
    /// top-of-loop hook (`queue_snapshot_stale_if_aged`), since it
    /// depends on the current step counter, not the tool that just ran.
    pub fn queue_invalidations_for_tool_success(&mut self, tool_name: &str, arguments: &Value) {
        if FOCUS_CHANGING_TOOLS.contains(&tool_name) {
            self.queue_invalidation(InvalidationEvent::FocusChanging {
                tool: tool_name.to_string(),
            });
        }
        if APP_LIFECYCLE_TOOLS.contains(&tool_name) {
            self.queue_invalidation(InvalidationEvent::AppLifecycle {
                tool: tool_name.to_string(),
            });
        }
        if CDP_NAVIGATION_TOOLS.contains(&tool_name) {
            let new_url = arguments
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            self.queue_invalidation(InvalidationEvent::CdpNavigation { new_url });
        }
    }

    /// Queue per-snapshot `SnapshotStale` events for any snapshot
    /// (`last_native_ax_snapshot` or `last_screenshot`) whose own age
    /// has crossed its `ttl_steps`. Called at the top of every loop
    /// iteration before `observe()` so the apply-events pass drops
    /// bodies that have aged out without the LLM re-capturing.
    ///
    /// One event per stale field — never a shared `age_steps` value
    /// across both fields. A fresh screenshot must not be invalidated
    /// just because the AX snapshot is stale.
    pub fn queue_snapshot_stale_if_aged(&mut self) {
        use crate::agent::world_model::SnapshotKind;
        if let Some(ax) = &self.world_model.last_native_ax_snapshot
            && let Some(ttl) = ax.ttl_steps
        {
            let age = (self.step_index.saturating_sub(ax.written_at)) as u32;
            if age > ttl {
                self.queue_invalidation(InvalidationEvent::SnapshotStale {
                    kind: SnapshotKind::NativeAx,
                    age_steps: age,
                });
            }
        }
        if let Some(ss) = &self.world_model.last_screenshot
            && let Some(ttl) = ss.ttl_steps
        {
            let age = (self.step_index.saturating_sub(ss.written_at)) as u32;
            if age > ttl {
                self.queue_invalidation(InvalidationEvent::SnapshotStale {
                    kind: SnapshotKind::Screenshot,
                    age_steps: age,
                });
            }
        }
    }

    /// After a successful tool call, refresh the world model's identity
    /// fields that the tool just captured. Non-snapshot tools are no-ops.
    pub fn update_continuity_after_tool_success(&mut self, tool_name: &str, body: &str) {
        use crate::agent::world_model::{
            AxSnapshotData, Fresh, FreshnessSource, ObservedElement, ScreenshotRef,
            parse_ax_snapshot, parse_ocr_matches,
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
                // Mirror parsed AX elements into the source-agnostic
                // element surface so the renderer prints them alongside
                // (or instead of) CDP elements. Native-only paths
                // depend on this — without it the LLM never sees the
                // a-prefixed uid vocabulary in `<world_model>`.
                if !parsed.is_empty() {
                    let observed: Vec<ObservedElement> =
                        parsed.into_iter().map(ObservedElement::Ax).collect();
                    self.world_model.elements = Some(Fresh {
                        value: observed,
                        written_at: self.step_index,
                        source: FreshnessSource::DirectObservation,
                        ttl_steps: Some(8),
                    });
                }
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
            "find_text" => {
                // OCR results from `find_text` populate the
                // source-agnostic element surface as `ObservedElement::Ocr`
                // when the response is parseable. Parse failures are
                // tolerated silently — `find_text` has multiple legacy
                // body shapes, so a non-OCR-shaped body is normal.
                if let Ok(matches) = parse_ocr_matches(body)
                    && !matches.is_empty()
                {
                    let observed: Vec<ObservedElement> =
                        matches.into_iter().map(ObservedElement::Ocr).collect();
                    self.world_model.elements = Some(Fresh {
                        value: observed,
                        written_at: self.step_index,
                        source: FreshnessSource::DirectObservation,
                        ttl_steps: Some(2),
                    });
                }
            }
            _ => {}
        }
    }

    /// Fetch compact CDP page inventory from the current page via MCP.
    ///
    /// This deliberately calls `cdp_summarize_page`, not
    /// `cdp_find_elements`: the top-of-loop observation should tell the model
    /// which page and element categories exist without injecting a transient
    /// page-wide DOM list into every prompt. Explicit target candidates enter
    /// the transcript only when the agent asks for `cdp_find_elements`, and
    /// ambiguous matches can be expanded with `cdp_get_element_context`.
    pub(crate) async fn fetch_cdp_page_summary<M: Mcp + ?Sized>(
        &mut self,
        mcp: &M,
    ) -> CdpPageObservation {
        if !mcp.has_tool("cdp_summarize_page") {
            // No CDP surface this turn — clear the sticky URL so the
            // next-turn state-block mirror does not render a stale page.
            self.state.current_url = String::new();
            return CdpPageObservation::default();
        }
        match mcp
            .call_tool("cdp_summarize_page", Some(serde_json::json!({})))
            .await
        {
            Ok(result) if result.is_error != Some(true) => {
                let text = crate::cdp_lifecycle::extract_text(&result);
                match serde_json::from_str::<clickweave_core::cdp::CdpPageSummaryResponse>(&text) {
                    Ok(parsed) => {
                        self.state.current_url = parsed.page_url.clone();
                        let page_fingerprint = crate::agent::transition::page_inventory_fingerprint(
                            &parsed.page_url,
                            &parsed.inventory,
                        );
                        return CdpPageObservation {
                            page_url: parsed.page_url,
                            page_fingerprint,
                            inventory: parsed
                                .inventory
                                .into_iter()
                                .map(CdpElementInventorySummary::from)
                                .collect(),
                        };
                    }
                    Err(parse_err) => {
                        tracing::debug!(
                            error = %parse_err,
                            "state-spine: failed to parse cdp_summarize_page response"
                        );
                        self.emit_event(AgentEvent::Warning {
                            message: format!(
                                "cdp_summarize_page response failed to parse: {} — continuing without CDP page summary",
                                parse_err
                            ),
                        })
                        .await;
                        // Parse failure — clear the sticky URL so a later
                        // turn does not keep rendering the previous page.
                        self.state.current_url = String::new();
                    }
                }
            }
            Ok(_) => {
                // MCP returned `is_error=true` or a non-Ok result — treat
                // as "no fresh observation" and drop the sticky URL.
                self.state.current_url = String::new();
            }
            Err(e) => {
                tracing::debug!(error = %e, "state-spine: cdp_summarize_page call failed");
                self.state.current_url = String::new();
            }
        }
        CdpPageObservation::default()
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

    /// Persist one `BoundaryKind::SubgoalCompleted` record per
    /// milestone appended during the current turn. Called from the
    /// outer loop in [`Self::run`] right after the mutation apply
    /// counts a positive `outer_milestones_appended` — before any
    /// early-exit branch (synthetic focus skip / live policy-deny /
    /// live approval-reject), so the boundary record fires whether or
    /// not the action eventually goes through `run_turn`. Records the
    /// turn's batched mutations as `action_taken` so the subgoal
    /// summaries are recoverable from `events.jsonl` without a
    /// separate transcript lookup. Emits one
    /// `AgentEvent::BoundaryRecordWritten` per persisted record.
    async fn write_subgoal_completed_records(&mut self, count: usize, turn: &AgentTurn) {
        let action_taken =
            serde_json::to_value(&turn.mutations).unwrap_or_else(|_| serde_json::json!([]));
        let milestone_start = self.task_state.milestones.len().saturating_sub(count);
        for i in 0..count {
            let milestone_text = self
                .task_state
                .milestones
                .get(milestone_start + i)
                .map(|m| m.text.clone());
            self.persist_boundary_record(
                crate::agent::step_record::BoundaryKind::SubgoalCompleted,
                action_taken.clone(),
                serde_json::json!({"kind": "subgoal_completed"}),
                milestone_text,
            )
            .await;
        }

        // Spec 3: drain the extraction queue populated by
        // `apply_mutations`. Each completed-subgoal milestone has both
        // its push-side `recorded_steps` index, the milestone payload,
        // and the node lineage for that subgoal frame available without
        // re-walking `task_state.milestones`.
        let queue = std::mem::take(&mut self.completed_subgoal_extraction_queue);
        if !queue.is_empty() && self.skill_ctx.enabled && self.config.skills_enabled {
            let workflow_hash = self.episodic_ctx.workflow_hash.clone();
            let run_id = self.run_id;
            let step_index = self.state.steps.len();

            for (push_idx, milestone, pre_state_sig, produced_node_ids) in queue {
                let action_sequence = if push_idx < self.recorded_steps.len() {
                    self.recorded_steps[push_idx..].to_vec()
                } else {
                    Vec::new()
                };
                match crate::agent::skills::extractor::maybe_extract_skill(
                    &milestone,
                    &action_sequence,
                    pre_state_sig,
                    &self.world_model,
                    &self.skill_index,
                    &self.skill_store,
                    &self.skill_ctx,
                    run_id,
                    &workflow_hash,
                    step_index,
                    &produced_node_ids,
                )
                .await
                {
                    Ok(crate::agent::skills::MaybeExtracted::Inserted {
                        skill_id,
                        version,
                        ..
                    })
                    | Ok(crate::agent::skills::MaybeExtracted::Merged {
                        skill_id, version, ..
                    }) => {
                        let (state, scope) = self
                            .skill_index
                            .read()
                            .get(&skill_id, version)
                            .map(|s| (s.state, s.scope))
                            .unwrap_or((
                                crate::agent::skills::SkillState::Draft,
                                crate::agent::skills::SkillScope::ProjectLocal,
                            ));
                        self.emit_event(AgentEvent::SkillExtracted {
                            run_id: self.run_id,
                            skill_id,
                            version,
                            state,
                            scope,
                        })
                        .await;
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!(?err, "skills: extraction failed; continuing");
                    }
                }
            }
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
            None,
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
            None,
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
        milestone_text: Option<String>,
    ) {
        let record = self.build_step_record(boundary_kind.clone(), action_taken, outcome);
        self.write_step_record(&record);
        self.emit_event(AgentEvent::BoundaryRecordWritten {
            run_id: self.run_id,
            boundary_kind,
            step_index: record.step_index,
            milestone_text,
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
    /// is wrapped by the LLM loop + compaction in [`Self::run_inner`].
    ///
    /// Return tuple: `(outcome, warnings, milestones_appended)`.
    /// `milestones_appended` counts `CompleteSubgoal` mutations that
    /// successfully popped a subgoal off the stack during this turn.
    /// In the live runner the outer loop applies mutations *before*
    /// calling `run_turn` (so `run_turn` receives an action-only turn
    /// and the count returned here is `0`); the count is meaningful
    /// for integration tests that drive `run_turn` directly with
    /// non-empty mutation batches.
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
        //    If `run()` captured signatures before its observe-phase
        //    mirror (`fetch_cdp_page_summary` → `world_model.cdp_page`)
        //    use that baseline so direct-observation writes also surface
        //    in `changed_fields`; otherwise (unit/test callers) fall back
        //    to snapshotting here.
        let pre_signatures = self
            .turn_pre_signatures
            .take()
            .unwrap_or_else(|| self.world_model.field_signatures());
        let prev_phase = self.task_state.phase;
        self.observe();
        if prev_phase != self.task_state.phase {
            self.emit_event(AgentEvent::TaskStateChanged {
                run_id: self.run_id,
                task_state: self.task_state.clone(),
            })
            .await;
        }
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
                    self.queue_invalidations_for_tool_success(tool_name, arguments);
                    self.consecutive_errors = 0;
                    TurnOutcome::ToolSuccess {
                        tool_name: tool_name.clone(),
                        tool_body: body,
                    }
                }
                Err(error) => {
                    self.consecutive_errors += 1;
                    let stale_cdp_uid = is_stale_cdp_uid_error(tool_name, &error);
                    if stale_cdp_uid {
                        self.world_model.elements = None;
                    }
                    let error = if stale_cdp_uid {
                        build_stale_cdp_uid_nudge(&error)
                    } else {
                        error
                    };
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
            AgentAction::InvokeSkill {
                skill_id,
                version,
                parameters,
            } => {
                // Phase 4: validate the skill exists + parameter
                // shape + emit `SkillInvoked`. The per-step expansion
                // (Task 4.3 follow-up) hasn't landed yet, so this arm
                // returns a replan that names the resolved skill so
                // the next LLM turn has a clear breadcrumb. Errors at
                // lookup / validation time produce an `InvalidArgs`-
                // shaped replan instead of panicking so a malformed
                // `invoke_skill` call can't take the run down.
                match self
                    .dispatch_skill(skill_id, *version, parameters.clone())
                    .await
                {
                    Ok(frame) => TurnOutcome::Replan {
                        reason: format!(
                            "skill {}@v{} resolved with {} parameter(s); replay engine pending — falling back to LLM",
                            frame.skill.id,
                            frame.skill.version,
                            frame.params.as_object().map(|m| m.len()).unwrap_or(0),
                        ),
                    },
                    Err(reason) => TurnOutcome::Replan { reason },
                }
            }
        };

        // `step_index` is owned by the outer-loop call sites that record
        // an `AgentStep` (via `advance_recorded_step_index`). `run_turn`
        // intentionally does not advance it — early-continue paths
        // (synthetic focus skip, policy deny, approval reject) record
        // their own steps without going through
        // `run_turn`, and prior to this fix the divergent advancement
        // let `step_index == 0` re-fire D24 run-start retrieval after
        // the run had already taken actions.

        (outcome, warnings, milestones_appended)
    }

    /// Advance the recorded-step counter. Single owner of `step_index`
    /// updates. Call after every `self.state.steps.push(...)` site so
    /// `step_index` matches `state.steps.len()` and the prompt's
    /// rendered step number stays in sync with what the run has
    /// actually executed.
    pub(crate) fn advance_recorded_step_index(&mut self) {
        self.step_index += 1;
    }

    /// Emit a per-step `WorldModelChanged` event for an early-exit step
    /// path that recorded an `AgentStep` without going through
    /// `run_turn`. Live policy-deny, live approval-reject, and the synthetic
    /// `focus_window` skip all record steps but skip `run_turn`
    /// entirely; without this hook, the `turn_pre_signatures` baseline
    /// would be carried into the next iteration and the
    /// `WorldModelChanged` diff would span multiple recorded steps.
    ///
    /// Consumes the current baseline (top-of-loop snapshot) and
    /// re-seeds it with the post-step signatures so the next iteration
    /// sees a fresh baseline keyed to the just-recorded step.
    pub(crate) async fn emit_world_model_changed_for_recorded_step(&mut self) {
        let pre_signatures = self
            .turn_pre_signatures
            .take()
            .unwrap_or_else(|| self.world_model.field_signatures());
        let post_signatures = self.world_model.field_signatures();
        let diff = diff_world_model_signatures(&pre_signatures, &post_signatures);
        self.emit_event(AgentEvent::WorldModelChanged {
            run_id: self.run_id,
            diff,
        })
        .await;
        self.turn_pre_signatures = Some(post_signatures);
    }

    /// Record a permission-policy denial as the current "last failure"
    /// so any subsequent `Recovering`-entry snapshot captures a real
    /// `(failed_tool, error_kind)` pair instead of the empty defaults.
    /// `error_kind` is the stable string `"policy_denied"` so episodic
    /// retrieval can group denied-tool recoveries by failure family
    /// without parsing the human-readable message.
    pub(crate) fn record_policy_deny_failure(&mut self, tool_name: &str) {
        self.last_failed_tool_name = Some(tool_name.to_string());
        self.last_failed_error_kind = Some("policy_denied".to_string());
    }

    /// Mirror of `record_policy_deny_failure`'s clear half. Called by
    /// every recovery-success path (live ToolSuccess in `run_turn`,
    /// synthetic focus-window skip) so a prior
    /// deny / tool-error doesn't bleed into a later Recovering snapshot
    /// after the agent has demonstrably recovered.
    pub(crate) fn clear_last_failure_tracking(&mut self) {
        self.last_failed_tool_name = None;
        self.last_failed_error_kind = None;
    }

    /// Bump the success-side repeat-action tracker for one dispatched
    /// non-observation tool call. Returns the no-progress nudge string
    /// when the streak crosses [`REPEAT_ACTION_THRESHOLD`], `None`
    /// otherwise. Caller installs the nudge into `previous_result` so
    /// the next turn renders it as the observation; the warning event
    /// is emitted here.
    ///
    /// Called by the live `ToolSuccess` arm so repeated live dispatches
    /// contribute to the same streak count.
    async fn track_repeat_action(
        &mut self,
        tool_name: &str,
        tool_arguments: &Value,
        tool_body: &str,
        annotations_by_tool: &HashMap<String, ToolAnnotations>,
        last_action: &mut Option<LastActionProgress>,
        recent_actions: &mut VecDeque<ActionProgressSignature>,
    ) -> Option<String> {
        if is_observation_tool(tool_name, annotations_by_tool) {
            return None;
        }
        let context_signature = stable_no_progress_context_signature(&self.world_model);
        if last_action
            .as_ref()
            .is_some_and(|last| last.context_signature != context_signature)
        {
            *last_action = None;
            recent_actions.clear();
        }
        let signature = ActionProgressSignature {
            tool_name: tool_name.to_string(),
            arguments: tool_arguments.clone(),
            context_signature: context_signature.clone(),
        };
        if recent_actions.len() == ACTION_CYCLE_WINDOW {
            recent_actions.pop_front();
        }
        recent_actions.push_back(signature);
        let same_as_last = matches!(
            last_action.as_ref(),
            Some(last)
                if last.tool_name == tool_name
                    && last.arguments == *tool_arguments
                    && last.context_signature == context_signature
        );
        let count = if same_as_last {
            last_action.as_ref().map(|last| last.count).unwrap_or(0) + 1
        } else {
            1
        };
        *last_action = Some(LastActionProgress {
            tool_name: tool_name.to_string(),
            arguments: tool_arguments.clone(),
            context_signature,
            count,
        });
        if count < REPEAT_ACTION_THRESHOLD {
            if let Some(cycle) = detect_repeated_action_cycle(recent_actions) {
                let cycle_summary = cycle.join(" -> ");
                warn!(
                    cycle = %cycle_summary,
                    "state-spine: repeated action cycle detected — injecting no-progress nudge"
                );
                self.emit_event(AgentEvent::Warning {
                    message: format!(
                        "{}: repeated action cycle `{}`",
                        NO_PROGRESS_WARNING_PREFIX, cycle_summary
                    ),
                })
                .await;
                return Some(build_action_cycle_nudge(&cycle_summary, tool_body));
            }
            return None;
        }
        warn!(
            tool = %tool_name,
            count,
            "state-spine: repeat-action threshold reached — injecting no-progress nudge"
        );
        self.emit_event(AgentEvent::Warning {
            message: format!(
                "{}: `{}` repeated {} turns in a row",
                NO_PROGRESS_WARNING_PREFIX, tool_name, count
            ),
        })
        .await;
        Some(build_no_progress_nudge(tool_name, count, tool_body))
    }

    async fn track_post_text_submit_search(
        &mut self,
        tool_name: &str,
        tool_arguments: &Value,
        tool_body: &str,
        pending: &mut Option<TextSubmitSearchProgress>,
    ) -> Option<String> {
        if is_text_composition_tool(tool_name) {
            *pending = Some(TextSubmitSearchProgress {
                context_signature: stable_no_progress_context_signature(&self.world_model),
                count: 0,
            });
            return None;
        }

        if tool_name != "cdp_find_elements" {
            if !OBSERVATION_TOOLS.contains(&tool_name) {
                *pending = None;
            }
            return None;
        }

        if !is_send_submit_cdp_search(tool_arguments) {
            return None;
        }

        let Some(progress) = pending.as_mut() else {
            return None;
        };
        let context_signature = stable_no_progress_context_signature(&self.world_model);
        if progress.context_signature != context_signature {
            *pending = None;
            return None;
        }

        if cdp_find_elements_has_matches(tool_body) != Some(false) {
            progress.count = 0;
            return None;
        }

        progress.count += 1;
        if progress.count < TEXT_SUBMIT_SEARCH_THRESHOLD {
            return None;
        }

        warn!(
            count = progress.count,
            "state-spine: repeated post-text send search detected — injecting no-progress nudge"
        );
        self.emit_event(AgentEvent::Warning {
            message: format!(
                "{}: repeated send/submit search after composing text",
                NO_PROGRESS_WARNING_PREFIX
            ),
        })
        .await;
        Some(build_post_text_submit_nudge(progress.count, tool_body))
    }
}

/// Result of requesting user approval for a tool action. Shared by both
/// policy evaluation and the live dispatch path.
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
    /// Electron / Chrome target where CDP isn't live yet but the MCP
    /// server advertises `cdp_connect`. The post-tool hook's
    /// `auto_connect_cdp` will fire on its own; the real `focus_window`
    /// is unnecessary and would only steal foreground in the meantime.
    CdpAttachable,
    /// Operator flipped [`AgentConfig::allow_focus_window`] to `false`;
    /// every focus_window is dropped regardless of kind or toolset.
    PolicyDisabled,
}

#[derive(Debug, Clone)]
struct RunningAppInfo {
    name: String,
    pid: Option<i32>,
    kind: Option<String>,
}

impl FocusSkipReason {
    const ALL: [Self; 4] = [
        Self::AxAvailable,
        Self::CdpLive,
        Self::CdpAttachable,
        Self::PolicyDisabled,
    ];

    /// Result text returned to the LLM in the synthetic
    /// `StepOutcome::Success`. Must not drift from the strings the tests
    /// pin — they encode the agent→LLM skip-contract.
    pub(crate) const fn llm_message(self) -> &'static str {
        match self {
            Self::AxAvailable => {
                "skipped focus_window: AX tools available; window focus not required"
            }
            Self::CdpLive => "skipped focus_window: CDP already live; focus not required",
            Self::CdpAttachable => {
                "focus_window skipped: CDP-attachable target; auto-connect will fire. \
                 Use cdp_* tools after the connection lands."
            }
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
            Self::CdpAttachable => "skipped: CDP-attachable target; auto-connect will fire",
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

/// True when a model-authored `launch_app` call has no launch-only
/// arguments. Native-devtools brings an already-running app to the
/// foreground in this shape, so the no-focus policy must verify the
/// process state before dispatching it.
fn launch_app_has_launch_only_args(arguments: &Value) -> bool {
    match arguments.get("args") {
        Some(Value::Array(args)) => !args.is_empty(),
        Some(Value::String(args)) => !args.trim().is_empty(),
        Some(Value::Null) | None => false,
        Some(_) => true,
    }
}

fn force_background_launch_app(action: &mut AgentAction, allow_focus_window: bool) {
    if allow_focus_window {
        return;
    }
    let AgentAction::ToolCall {
        tool_name,
        arguments,
        ..
    } = action
    else {
        return;
    };
    if tool_name != "launch_app"
        || arguments
            .get("background")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return;
    }
    if let Value::Object(map) = arguments {
        map.insert("background".to_string(), Value::Bool(true));
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

/// Coordinate-based primitives that move the cursor and steal focus.
/// `coordinate_primitive_blocked` rejects these when a structured
/// surface (CDP page or Native AX dispatch) is wired for the focused
/// app. Mirrors the `Coordinate` arm of
/// [`crate::agent::prompt::classify_tool_family`] but kept narrower:
/// `find_text` / `find_image` / `element_at_point` are coordinate
/// *observations*, not actions, so they pass through.
fn is_coordinate_primitive(name: &str) -> bool {
    matches!(
        name,
        "click" | "type_text" | "press_key" | "move_mouse" | "scroll" | "drag"
    )
}

/// Observation tools that do not become workflow action nodes. Mirrors
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
    "cdp_summarize_page",
    "cdp_find_elements",
    "cdp_get_element_context",
    "cdp_wait_for_page_change",
    "android_list_devices",
];

/// AX dispatch tools whose uid arguments are scoped to one
/// `take_ax_snapshot`. See the legacy `AX_DISPATCH_TOOLS`.
const AX_DISPATCH_TOOLS: &[&str] = &["ax_click", "ax_set_value", "ax_select"];

/// Tools that transition app / window / CDP state. They are unsafe for
/// skill replay because replaying them against unchanged elements would
/// fire the transition a second time. See the legacy `STATE_TRANSITION_TOOLS`.
const STATE_TRANSITION_TOOLS: &[&str] = &[
    "launch_app",
    "focus_window",
    "quit_app",
    "cdp_connect",
    "cdp_disconnect",
];

/// Tools whose successful dispatch shifts which window has keyboard /
/// element focus. `observe()` drains a `FocusChanging` event for each.
const FOCUS_CHANGING_TOOLS: &[&str] = &["focus_window", "launch_app", "quit_app"];

/// Tools that cross an app-process boundary (start or end an app).
/// In addition to focus, these invalidate window list, screenshot, and
/// AX-snapshot continuity records.
const APP_LIFECYCLE_TOOLS: &[&str] = &["launch_app", "quit_app"];

/// Tools whose success implies a navigation in the active CDP page,
/// invalidating page state and the element surface.
const CDP_NAVIGATION_TOOLS: &[&str] = &["cdp_navigate", "cdp_new_page", "cdp_select_page"];

/// Number of consecutive successful dispatches of the same
/// `(tool_name, arguments)` tuple — over non-observation tools — that
/// trigger a no-progress nudge. The first repeat that crosses the
/// threshold injects the nudge; subsequent repeats keep injecting it
/// until the LLM picks a different action and the counter resets.
const REPEAT_ACTION_THRESHOLD: u32 = 3;

/// Largest repeated action-cycle body to detect. Observation tools are
/// excluded before they reach this window.
const ACTION_CYCLE_MAX_PATTERN_LEN: usize = 3;
const ACTION_CYCLE_WINDOW: usize = ACTION_CYCLE_MAX_PATTERN_LEN * 2;

const TEXT_SUBMIT_SEARCH_THRESHOLD: u32 = 3;

/// Prefix on the synthetic observation injected back to the LLM when
/// the repeat-action detector fires. Anchors the test assertion that
/// the nudge actually reached `previous_result`.
pub(crate) const NO_PROGRESS_NUDGE_PREFIX: &str = "[NO-PROGRESS NUDGE]";

/// Prefix on the `AgentEvent::Warning` message emitted alongside the
/// nudge. Anchors the test assertion that subscribers see the event.
pub(crate) const NO_PROGRESS_WARNING_PREFIX: &str = "no-progress";

pub(crate) const NO_ACTION_MUTATION_ONLY_PREFIX: &str = "[NO ACTION DISPATCHED]";

const NO_ACTION_MUTATION_ONLY_REASON: &str = "[NO ACTION DISPATCHED] You emitted only task-state mutation pseudo-tools. The harness updated the task state, but no MCP/environment action ran: no click, fill, typing, navigation, or selection happened. Do not infer that the UI changed; choose a real action next or emit agent_replan with a new tactic.";

pub(crate) const UNVERIFIED_SIDE_EFFECT_PREFIX: &str = "[UNVERIFIED SIDE EFFECT]";

const UNVERIFIED_SIDE_EFFECT_COMPLETION_BLOCKED_REASON: &str = "[UNVERIFIED SIDE EFFECT] The previous action may have changed external state, but its return value is not proof that the requested state is active. Verify the intended state with a structured observation or typed dispatch before calling complete_subgoal or agent_done.";

pub(crate) const STALE_CDP_UID_PREFIX: &str = "[STALE CDP UID]";

#[derive(Debug, Clone)]
struct LastActionProgress {
    tool_name: String,
    arguments: Value,
    context_signature: String,
    count: u32,
}

#[derive(Debug, Clone, PartialEq)]
struct ActionProgressSignature {
    tool_name: String,
    arguments: Value,
    context_signature: String,
}

#[derive(Debug, Clone)]
struct TextSubmitSearchProgress {
    context_signature: String,
    count: u32,
}

fn reset_no_progress_tracking(
    last_action: &mut Option<LastActionProgress>,
    recent_actions: &mut VecDeque<ActionProgressSignature>,
) {
    *last_action = None;
    recent_actions.clear();
}

fn stable_no_progress_context_signature(world_model: &WorldModel) -> String {
    let focused_app = world_model.focused_app.as_ref().map(|fresh| {
        serde_json::json!({
            "name": &fresh.value.name,
            "kind": fresh.value.kind,
            "pid": fresh.value.pid,
        })
    });
    let cdp_page_url = world_model
        .cdp_page
        .as_ref()
        .map(|fresh| fresh.value.url.as_str());
    let element_surface = world_model
        .elements
        .as_ref()
        .map(|fresh| stable_element_surface_signature(&fresh.value));
    let cdp_page_fingerprint = world_model.cdp_page.as_ref().and_then(|fresh| {
        element_surface
            .is_none()
            .then_some(fresh.value.page_fingerprint.as_str())
    });
    let cdp_connect_status = world_model
        .cdp_connect_status
        .as_ref()
        .map(|fresh| fresh.value.as_str());
    let modal_present = world_model.modal_present.as_ref().map(|fresh| fresh.value);
    let dialog_present = world_model.dialog_present.as_ref().map(|fresh| fresh.value);
    let signature = serde_json::json!({
        "focused_app": focused_app,
        "cdp_page_url": cdp_page_url,
        "cdp_page_fingerprint": cdp_page_fingerprint,
        "cdp_connect_status": cdp_connect_status,
        "element_surface": element_surface,
        "modal_present": modal_present,
        "dialog_present": dialog_present,
    });
    let bytes = serde_json::to_vec(&signature).unwrap_or_default();
    blake3::hash(&bytes).to_hex().to_string()
}

fn stable_element_surface_signature(elements: &[ObservedElement]) -> String {
    let mut stable_entries: Vec<Value> = elements.iter().map(stable_observed_element_key).collect();
    stable_entries.sort_by_key(|entry| serde_json::to_string(entry).unwrap_or_default());
    let bytes = serde_json::to_vec(&stable_entries).unwrap_or_default();
    blake3::hash(&bytes).to_hex()[..16].to_string()
}

fn stable_observed_element_key(element: &ObservedElement) -> Value {
    match element {
        ObservedElement::Cdp(el) => serde_json::json!({
            "source": "cdp",
            "role": &el.role,
            "label": &el.label,
            "accessible_name": &el.accessible_name,
            "visible_text": &el.visible_text,
            "value": &el.value,
            "placeholder": &el.placeholder,
            "title": &el.title,
            "alt_text": &el.alt_text,
            "test_id": &el.test_id,
            "tag": &el.tag,
            "disabled": el.disabled,
            "parent_role": &el.parent_role,
            "parent_name": &el.parent_name,
        }),
        ObservedElement::Ax(el) => serde_json::json!({
            "source": "ax",
            "role": &el.role,
            "name": &el.name,
            "value": &el.value,
            "depth": el.depth,
            "focused": el.focused,
            "disabled": el.disabled,
            "parent_name": &el.parent_name,
        }),
        ObservedElement::Ocr(el) => serde_json::json!({
            "source": "ocr",
            "text": &el.text,
            "x_bin": el.x.div_euclid(10),
            "y_bin": el.y.div_euclid(10),
            "width_bin": el.width.div_euclid(10),
            "height_bin": el.height.div_euclid(10),
        }),
    }
}

fn detect_repeated_action_cycle(
    recent_actions: &VecDeque<ActionProgressSignature>,
) -> Option<Vec<String>> {
    for pattern_len in 2..=ACTION_CYCLE_MAX_PATTERN_LEN {
        let needed = pattern_len * 2;
        if recent_actions.len() < needed {
            continue;
        }
        let len = recent_actions.len();
        let first: Vec<_> = recent_actions
            .iter()
            .skip(len - needed)
            .take(pattern_len)
            .collect();
        let second: Vec<_> = recent_actions
            .iter()
            .skip(len - pattern_len)
            .take(pattern_len)
            .collect();
        let has_distinct_actions = first.iter().skip(1).any(|sig| *sig != first[0]);
        if has_distinct_actions && first == second {
            return Some(first.iter().map(|sig| sig.tool_name.clone()).collect());
        }
    }
    None
}

/// Build the no-progress nudge body. Pure function so the prompt copy
/// stays out of the inner loop and can be exercised independently.
fn build_no_progress_nudge(tool: &str, count: u32, prev_body: &str) -> String {
    format!(
        "{prefix} You have issued `{tool}` with the same arguments {count} turns in a row in the same stable app/page context, but the task is not advancing. Stop repeating this call. Either (1) switch dispatch family — if `<world_model>` has a `cdp_page` block, use CDP query/expand/action tools (e.g. `cdp_find_elements`, `cdp_get_element_context`, `cdp_click`, `cdp_fill`, `cdp_type_text`); if it has an AX tree, take a fresh `take_ax_snapshot` and use `ax_*` tools — or (2) push a narrower subgoal via `push_subgoal` and try a different tactic, or (3) emit `agent_replan`.\n\nPrevious tool body:\n{prev_body}",
        prefix = NO_PROGRESS_NUDGE_PREFIX,
    )
}

fn build_action_cycle_nudge(cycle_summary: &str, prev_body: &str) -> String {
    format!(
        "{prefix} You are in a repeated action cycle in the same stable app/page context: `{cycle_summary}`. The task is not advancing. Do not run the same cycle again. Change the `cdp_find_elements` query, expand a candidate with `cdp_get_element_context`, verify the active context, or emit `agent_replan`.\n\nPrevious tool body:\n{prev_body}",
        prefix = NO_PROGRESS_NUDGE_PREFIX,
    )
}

fn build_post_text_submit_nudge(count: u32, prev_body: &str) -> String {
    format!(
        "{prefix} You already wrote text into a textbox/editor, then searched for Send/Submit {count} times in the same stable page context without finding a matching control. Stop repeating send-button searches. If the focused editor should submit on Enter, call `cdp_press_key` with `{{\"key\":\"Enter\"}}`; otherwise use `cdp_get_element_context` around the composer controls or emit `agent_replan`.\n\nPrevious tool body:\n{prev_body}",
        prefix = NO_PROGRESS_NUDGE_PREFIX,
    )
}

fn is_text_composition_tool(tool_name: &str) -> bool {
    matches!(tool_name, "cdp_fill" | "cdp_type_text")
}

fn is_send_submit_cdp_search(arguments: &Value) -> bool {
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    query.contains("send") || query.contains("submit")
}

fn cdp_find_elements_has_matches(tool_body: &str) -> Option<bool> {
    let parsed: Value = serde_json::from_str(tool_body).ok()?;
    let matches = parsed.get("matches")?.as_array()?;
    Some(!matches.is_empty())
}

fn cdp_evaluate_script_function(arguments: &Value) -> Option<&str> {
    arguments.get("function").and_then(Value::as_str)
}

fn cdp_evaluate_script_has_side_effect(function: &str) -> bool {
    let f = function.to_ascii_lowercase();
    [
        ".click(",
        ".dispatch_event(",
        ".dispatchevent(",
        ".submit(",
        ".focus(",
        ".blur(",
        ".scroll",
        ".setattribute(",
        ".removeattribute(",
        ".value =",
        ".checked =",
        ".selected =",
        ".innerhtml =",
        ".textcontent =",
        "localstorage.setitem(",
        "sessionstorage.setitem(",
        "window.location",
        "location.href",
        "location =",
        "history.pushstate(",
        "history.replacestate(",
    ]
    .iter()
    .any(|needle| f.contains(needle))
}

fn is_side_effectful_cdp_evaluate_script(tool_name: &str, arguments: &Value) -> bool {
    tool_name == "cdp_evaluate_script"
        && cdp_evaluate_script_function(arguments).is_some_and(cdp_evaluate_script_has_side_effect)
}

fn is_unverified_side_effect_action(
    tool_name: &str,
    arguments: &Value,
    annotations_by_tool: &HashMap<String, ToolAnnotations>,
) -> bool {
    if tool_name == "cdp_evaluate_script" {
        return is_side_effectful_cdp_evaluate_script(tool_name, arguments);
    }

    annotations_by_tool
        .get(tool_name)
        .is_some_and(|annotations| {
            annotations.open_world_hint == Some(true) && annotations.destructive_hint == Some(true)
        })
}

fn build_unverified_side_effect_nudge(tool_body: &str) -> String {
    format!(
        "{prefix} The last action may have changed external state, but its return value is not proof that the requested state is active. Before completing a subgoal or the whole goal, verify with structured state: use a typed dispatch when a stable target exists, or run a focused observation that proves the intended active context.\n\nPrevious action result:\n{tool_body}",
        prefix = UNVERIFIED_SIDE_EFFECT_PREFIX,
    )
}

fn previous_result_is_unverified_side_effect(previous_result: Option<&str>) -> bool {
    previous_result.is_some_and(|body| body.starts_with(UNVERIFIED_SIDE_EFFECT_PREFIX))
}

fn guard_completion_after_unverified_side_effect(
    previous_result: Option<&str>,
    turn: &mut AgentTurn,
) -> bool {
    if !previous_result_is_unverified_side_effect(previous_result) {
        return false;
    }

    let before = turn.mutations.len();
    turn.mutations
        .retain(|m| !matches!(m, TaskStateMutation::CompleteSubgoal { .. }));
    let stripped_complete = before != turn.mutations.len();

    let blocked_done = matches!(turn.action, AgentAction::AgentDone { .. });
    if blocked_done {
        turn.action = AgentAction::AgentReplan {
            reason: UNVERIFIED_SIDE_EFFECT_COMPLETION_BLOCKED_REASON.to_string(),
        };
    }

    stripped_complete || blocked_done
}

fn is_stale_cdp_uid_error(tool_name: &str, error: &str) -> bool {
    tool_name.starts_with("cdp_")
        && (error.contains("No node with given id found")
            || error.contains("could not be resolved to a DOM node")
            || error.contains("element is not attached")
            || error.contains("stale element"))
}

fn build_stale_cdp_uid_nudge(error: &str) -> String {
    format!(
        "{prefix} The CDP element id from a previous observation is no longer valid. No click, fill, selection, or typing happened. Rediscover the target with `cdp_find_elements` before the next `cdp_click`/`cdp_fill`; do not reuse prior `d<N>` ids.\n\nOriginal error:\n{error}",
        prefix = STALE_CDP_UID_PREFIX,
    )
}

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
// `crate::agent::world_model` can verify dispatch classification without
// reaching through `StateRunner`'s private API (Task 3a.7.d).
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

/// Compress a tool-arguments JSON value into a short string suitable
/// for the episodic [`CompactAction::brief_args`] field. Capped at
/// 120 chars (a multi-byte-safe truncation) so a giant blob argument
/// can never bloat the writer's payload.
fn brief_summarize_args(arguments: &Value) -> String {
    let s = serde_json::to_string(arguments).unwrap_or_default();
    if s.len() <= 120 {
        return s;
    }
    let cut = (0..=117)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    format!("{}...", &s[..cut])
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
    fn skill_frame_to_single_step_action(frame: &crate::agent::skills::SkillFrame) -> AgentAction {
        match frame.skill.action_sketch.as_slice() {
            [crate::agent::skills::ActionSketchStep::ToolCall { tool, args, .. }] => {
                match crate::agent::skills::substitution::substitute_value(
                    args,
                    &frame.params,
                    &frame.captured,
                ) {
                    Ok(arguments) => AgentAction::ToolCall {
                        tool_name: tool.clone(),
                        arguments,
                        tool_call_id: format!(
                            "skill-{}-v{}-step-{}",
                            frame.skill.id, frame.skill.version, frame.next_step
                        ),
                    },
                    Err(err) => AgentAction::AgentReplan {
                        reason: format!("skill replay substitution failed: {err}"),
                    },
                }
            }
            [] => AgentAction::AgentReplan {
                reason: format!(
                    "skill {}@v{} has no replay steps",
                    frame.skill.id, frame.skill.version
                ),
            },
            [_] => AgentAction::AgentReplan {
                reason: format!(
                    "skill {}@v{} contains a non-tool replay step; full replay is not available yet",
                    frame.skill.id, frame.skill.version
                ),
            },
            steps => AgentAction::AgentReplan {
                reason: format!(
                    "skill {}@v{} has {} replay steps; full multi-step replay is not available yet",
                    frame.skill.id,
                    frame.skill.version,
                    steps.len()
                ),
            },
        }
    }

    /// Look up the named skill, validate parameters against its
    /// schema, and emit `AgentEvent::SkillInvoked`. Returns the live
    /// [`SkillFrame`] on success or a human-readable replan reason on
    /// failure (unknown skill, draft skill, invalid parameters).
    ///
    /// Phase 4 lands the lookup-and-validate half of `dispatch_skill`.
    /// The per-step expansion through the live dispatch helper —
    /// including sub-skill recursion, the `Loop` arm, and the
    /// LLM-fallback path on divergence — is staged for the follow-up
    /// pass. See the Phase 4 deferred-items list in the handoff for
    /// the resume seam. Until that lands, the outer-loop
    /// `AgentAction::InvokeSkill` arm degrades to a replan whose reason
    /// names the skill that was about to run, so a live invocation
    /// produces a clear bail-out rather than a silent no-op.
    pub(crate) async fn dispatch_skill(
        &mut self,
        skill_id: &str,
        version: u32,
        parameters: serde_json::Value,
    ) -> Result<crate::agent::skills::SkillFrame, String> {
        use crate::agent::skills::replay::{SkillFrame, validate_parameters};
        use crate::agent::skills::types::SkillState;

        let skill = match self.skill_index.read().get(skill_id, version) {
            Some(s) if !matches!(s.state, SkillState::Draft) => s,
            Some(_) => {
                return Err(format!(
                    "skill {skill_id}@v{version} is in draft state and cannot be invoked"
                ));
            }
            None => {
                return Err(format!("unknown skill: {skill_id}@v{version}"));
            }
        };

        let validated_params = match validate_parameters(&parameters, &skill.parameter_schema) {
            Ok(p) => p,
            Err(e) => return Err(format!("invalid skill parameters: {e}")),
        };

        let parameter_count = validated_params
            .as_object()
            .map(|m| m.len() as u32)
            .unwrap_or(0);
        self.emit_event(AgentEvent::SkillInvoked {
            run_id: self.run_id,
            skill_id: skill_id.to_string(),
            version,
            parameter_count,
        })
        .await;

        // Stamp `last_invoked_at` so the index reflects the attempt
        // even when the per-step expansion hasn't landed yet.
        self.skill_index
            .write()
            .mark_invoked(skill_id, version, chrono::Utc::now());

        Ok(SkillFrame::new(skill, validated_params))
    }

    /// Best-effort send of an [`AgentEvent`] through the configured
    /// channel. No-op when the channel is unset or closed — event
    /// emission must never fail the run.
    async fn emit_event(&self, event: AgentEvent) {
        let Some(tx) = &self.event_tx else { return };
        if tx.is_closed() {
            return;
        }
        if let Err(e) = tx.send(RunnerOutput::Event(event)).await {
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

    /// Compute the per-turn `<tools_in_scope>` subset from the current
    /// world-model state. No focused app yet → empty `Vec` → caller
    /// renders no block, so the LLM falls back to the system prompt's
    /// full `Available tools:` listing.
    fn compute_tools_in_scope(&self, advertised_tool_names: &[String]) -> Vec<String> {
        crate::agent::prompt::tools_in_scope(
            self.world_model.focused_app_kind(),
            self.world_model.is_cdp_attached(),
            advertised_tool_names,
        )
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
            // Pre-CDP-connect: kind is Electron/Chrome and the server
            // can attach via `cdp_connect`. The post-tool hook's
            // `auto_connect_cdp` will discover the debug port (or
            // quit + relaunch with one) on its own, so a preceding
            // `focus_window` is unnecessary and only steals foreground.
            Some("ElectronApp" | "ChromeBrowser") if mcp.has_tool("cdp_connect") => {
                Some(FocusSkipReason::CdpAttachable)
            }
            _ => None,
        }
    }

    /// Return the app target whose CDP session should be acquired after a
    /// synthetic `focus_window` skip. `CdpAttachable` always promises this
    /// path. `PolicyDisabled` also needs it for background Electron/Chrome
    /// work: suppressing the focus steal must not suppress the app-scoped
    /// CDP lifecycle that would otherwise attach to the target.
    fn cdp_target_for_skipped_focus_window<M: Mcp + ?Sized>(
        &self,
        reason: FocusSkipReason,
        arguments: &Value,
        mcp: &M,
    ) -> Option<(String, Option<String>)> {
        if !mcp.has_tool("cdp_connect") {
            return None;
        }
        let app_name = arguments.get("app_name").and_then(Value::as_str)?;
        if self.cdp_state.is_connected_to(app_name, 0) {
            return None;
        }
        let kind_hint = self.known_app_kinds.get(app_name).cloned();
        match reason {
            FocusSkipReason::CdpAttachable => Some((app_name.to_string(), kind_hint)),
            FocusSkipReason::PolicyDisabled => match kind_hint.as_deref() {
                Some("Native") => None,
                Some("ElectronApp" | "ChromeBrowser" | "electron_app" | "chrome_browser") => {
                    Some((app_name.to_string(), kind_hint))
                }
                // Unknown kind: let `auto_connect_cdp` probe. Native apps
                // short-circuit there; Electron/Chrome targets get an
                // app-scoped debug session without a foreground focus steal.
                None => Some((app_name.to_string(), None)),
                Some(_) => None,
            },
            _ => None,
        }
    }

    /// Under the no-focus policy, suppress a no-args `launch_app` when
    /// the target process is already running. Native-devtools treats that
    /// shape as "bring the app to the front"; for CDP-capable apps the
    /// runner can attach in the background instead.
    async fn running_app_for_no_focus_launch<M: Mcp + ?Sized>(
        &self,
        arguments: &Value,
        mcp: &M,
    ) -> Option<RunningAppInfo> {
        if self.config.allow_focus_window || !mcp.has_tool("list_apps") {
            return None;
        }
        if launch_app_has_launch_only_args(arguments) {
            return None;
        }
        let app_name = arguments.get("app_name").and_then(Value::as_str)?;
        if app_name.trim().is_empty() {
            return None;
        }

        let list_args = serde_json::json!({
            "app_name": app_name,
            "user_apps_only": true,
        });
        match mcp.call_tool("list_apps", Some(list_args)).await {
            Ok(result) if result.is_error != Some(true) => {
                let text = extract_result_text(&result);
                let entries: Vec<Value> = match serde_json::from_str(&text) {
                    Ok(entries) => entries,
                    Err(e) => {
                        debug!(
                            app = app_name,
                            error = %e,
                            "state-spine: list_apps parse failed during no-focus launch guard"
                        );
                        return None;
                    }
                };
                entries.into_iter().find_map(|entry| {
                    let name = entry.get("name").and_then(Value::as_str)?;
                    if !name.eq_ignore_ascii_case(app_name) {
                        return None;
                    }
                    let pid = entry
                        .get("pid")
                        .and_then(Value::as_i64)
                        .and_then(|pid| i32::try_from(pid).ok());
                    let kind = entry
                        .get("kind")
                        .and_then(Value::as_str)
                        .filter(|kind| !kind.trim().is_empty())
                        .map(str::to_string);
                    Some(RunningAppInfo {
                        name: name.to_string(),
                        pid,
                        kind,
                    })
                })
            }
            Ok(result) => {
                debug!(
                    app = app_name,
                    error = %extract_result_text(&result),
                    "state-spine: list_apps returned error during no-focus launch guard"
                );
                None
            }
            Err(e) => {
                debug!(
                    app = app_name,
                    error = %e,
                    "state-spine: list_apps failed during no-focus launch guard"
                );
                None
            }
        }
    }

    fn skipped_launch_result_text(info: &RunningAppInfo) -> String {
        let mut body = serde_json::Map::new();
        body.insert("app_name".to_string(), Value::String(info.name.clone()));
        body.insert(
            "message".to_string(),
            Value::String(
                "launch_app skipped: app is already running; foreground focus not required"
                    .to_string(),
            ),
        );
        if let Some(pid) = info.pid {
            body.insert("pid".to_string(), Value::Number(pid.into()));
        }
        if let Some(kind) = &info.kind {
            body.insert("kind".to_string(), Value::String(kind.clone()));
        }
        Value::Object(body).to_string()
    }

    /// Block raw model-authored CDP lifecycle operations. The agent runner
    /// owns app-scoped CDP acquisition so the model cannot attach to an
    /// unrelated app listening on a guessed port like 9222.
    fn raw_cdp_lifecycle_blocked(tool_name: &str, arguments: &Value) -> Option<String> {
        match tool_name {
            "cdp_connect" => {
                let port = arguments
                    .get("port")
                    .and_then(Value::as_u64)
                    .map(|p| format!(" Requested port was {p}."))
                    .unwrap_or_default();
                Some(format!(
                    "raw cdp_connect blocked: CDP connection lifecycle is runtime-managed. \
                     Do not guess debug ports.{port} Use launch_app or focus_window for the \
                     target Electron/Chrome app; the runner will reuse an existing \
                     --remote-debugging-port or relaunch that app with an ephemeral debug port, \
                     then attach CDP."
                ))
            }
            "cdp_disconnect" => Some(
                "raw cdp_disconnect blocked: CDP connection lifecycle is runtime-managed. \
                 The runner disconnects or reattaches when the target app changes; choose the \
                 next app action or agent_replan instead."
                    .to_string(),
            ),
            _ => None,
        }
    }

    /// Reject a coordinate-primitive tool (`click` / `type_text` /
    /// `press_key` / `move_mouse` / `scroll` / `drag`) when a structured
    /// surface is wired for the current focused app: a live CDP page, or
    /// a Native focus with the full AX dispatch toolset advertised.
    ///
    /// Defense-in-depth behind the per-turn `<tools_in_scope>` filter:
    /// the filter narrows the LLM's *advertised* tool list, but this
    /// guard rejects the *dispatched* call so a wrong-family choice
    /// (malformed turn, future replay path, future LLM regression)
    /// cannot reach MCP. Returns `Some(reason)` when the
    /// dispatch must be blocked; `None` otherwise.
    fn coordinate_primitive_blocked<M: Mcp + ?Sized>(
        &self,
        tool_name: &str,
        mcp: &M,
    ) -> Option<String> {
        use crate::agent::world_model::AppKind;
        if !is_coordinate_primitive(tool_name) {
            return None;
        }
        let Some(kind) = self.world_model.focused_app_kind() else {
            // Without a known focused-app kind we cannot tell which
            // structured surface (if any) is wired — defer to legacy
            // behavior.
            return None;
        };
        match kind {
            AppKind::ElectronApp | AppKind::ChromeBrowser
                if self.world_model.cdp_page.is_some() =>
            {
                Some(format!(
                    "coordinate primitive `{tool_name}` blocked: focused app is \
                     CDP-backed and a `cdp_page` is live in <world_model>. Coordinate \
                     clicks bypass the page's event loop and steal foreground. Use \
                     `cdp_click` / `cdp_fill` / `cdp_type_text` / `cdp_press_key` \
                     against `d<N>` uids returned by `cdp_find_elements`."
                ))
            }
            AppKind::Native if mcp_has_toolset(mcp, AX_DISPATCH_TOOLSET) => Some(format!(
                "coordinate primitive `{tool_name}` blocked: focused app is Native and \
                 AX dispatch is wired. Coordinate primitives steal focus and produce \
                 no `a<N>` uids the next turn can target. Call `take_ax_snapshot` then \
                 `ax_click` / `ax_set_value` / `ax_select` against the `a<N>` uids."
            )),
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
        // Successful connect supersedes any prior failure status — clear
        // it so the next turn's render does not show a stale error
        // alongside the now-live `cdp_page`.
        self.world_model.cdp_connect_status = None;
        crate::cdp_lifecycle::snapshot_selected_page_url(mcp, &mut self.cdp_state, app_name, 0)
            .await;
    }

    /// Record a permanent `auto_connect_cdp` failure on the world model.
    /// Called from each terminal error path in `auto_connect_cdp` so the
    /// next turn's state block surfaces the reason — without this, the
    /// LLM cannot distinguish "auto-connect hasn't fired yet" (no
    /// `cdp_page`, no status) from "auto-connect tried and failed" (no
    /// `cdp_page`, status present) and may keep waiting forever.
    fn record_cdp_connect_failure(&mut self, reason: String) {
        use crate::agent::world_model::{Fresh, FreshnessSource};
        self.world_model.cdp_connect_status = Some(Fresh {
            value: reason,
            written_at: self.step_index,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        });
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
                    self.record_cdp_connect_failure(format!(
                        "probe_app failed for {app_name}: {e}",
                    ));
                    return None;
                }
            };

            // Identify the discovered kind so we can update
            // `known_app_kinds` + `world_model.focused_app.kind` from the
            // probe result. Without this, the unstructured launch_app /
            // focus_window path leaves focused_app.kind = Native (the
            // maybe_cdp_connect default for kind_hint = None) even after
            // CDP attaches, which would route the per-turn filter to the
            // AX arm despite a live `cdp_page`.
            let discovered_kind = if probe_text.contains("ChromeBrowser") {
                "ChromeBrowser"
            } else if probe_text.contains("ElectronApp") {
                "ElectronApp"
            } else {
                debug!(
                    app = app_name,
                    "state-spine: not an Electron/Chrome app, skipping CDP"
                );
                return None;
            };
            self.record_app_kind(app_name, discovered_kind);
            if let Some(f) = self.world_model.focused_app.as_mut()
                && f.value.name == app_name
            {
                use crate::agent::world_model::AppKind;
                f.value.kind = match discovered_kind {
                    "ChromeBrowser" => AppKind::ChromeBrowser,
                    "ElectronApp" => AppKind::ElectronApp,
                    _ => unreachable!("discovered_kind is constrained above"),
                };
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
                self.record_cdp_connect_failure(format!(
                    "relaunch with debug port {port} failed for {app_name}: {err}",
                ));
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
                self.record_cdp_connect_failure(format!(
                    "cdp_connect failed after retries on port {port} for {app_name}: {last_err}",
                ));
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
            // Keep CDP state and `world_model.focused_app` in lock-step
            // with the underlying process.
            if tool_name == "quit_app"
                && let Some(name) = arguments.get("app_name").and_then(Value::as_str)
            {
                self.cdp_state.mark_app_quit(name);
                if self
                    .world_model
                    .focused_app
                    .as_ref()
                    .is_some_and(|f| f.value.name == name)
                {
                    self.world_model.focused_app = None;
                    // Status was bound to the now-departed focused app;
                    // a quit_app result should not leave its failure
                    // reason hanging on the next turn's render.
                    self.world_model.cdp_connect_status = None;
                }
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
        // Mirror the focus into `world_model.focused_app` so the per-turn
        // `<tools_in_scope>` filter sees the current focus state across
        // turns. Runs whether or not CDP attaches — the AX / pre-connect
        // arms key on focused-app kind alone.
        {
            use crate::agent::world_model::{AppKind, FocusedApp, Fresh, FreshnessSource};
            let kind = match kind_hint.as_deref() {
                Some("ElectronApp") | Some("electron_app") => AppKind::ElectronApp,
                Some("ChromeBrowser") | Some("chrome_browser") => AppKind::ChromeBrowser,
                _ => AppKind::Native,
            };
            let pid = serde_json::from_str::<Value>(result_text)
                .ok()
                .and_then(|v| v.get("pid").and_then(Value::as_i64))
                .map(|p| p as i32)
                .unwrap_or(0);
            self.world_model.focused_app = Some(Fresh {
                value: FocusedApp {
                    name: app_name.clone(),
                    kind,
                    pid,
                },
                written_at: self.step_index,
                source: FreshnessSource::DirectObservation,
                ttl_steps: None,
            });
        }
        // Clear any prior auto-connect status before the next attempt.
        // Without this, a successful focus to a different app would keep
        // showing the previous app's failure reason; auto_connect_cdp
        // either succeeds (cleared in `on_cdp_connected`), fails (set
        // in this attempt's terminal path), or short-circuits (no new
        // status is appropriate, so the old one must not survive).
        self.world_model.cdp_connect_status = None;
        if let Some(cdp_port) = self
            .auto_connect_cdp(&app_name, kind_hint.as_deref(), mcp)
            .await
        {
            self.finalize_cdp_connected(&app_name, cdp_port, mcp).await;
        }
    }

    /// Post-`auto_connect_cdp` housekeeping shared between the
    /// `maybe_cdp_connect` post-tool path and the dispatch-site
    /// `CdpAttachable` synthetic-skip path. Emits the `CdpConnected`
    /// event so the UI surfaces the connect, then refreshes the
    /// client-side tool cache so observation gates (notably
    /// `fetch_cdp_page_summary`'s `cdp_summarize_page` lookup) see the
    /// CDP tools the server surfaced post-connect.
    async fn finalize_cdp_connected<M: Mcp + ?Sized>(
        &self,
        app_name: &str,
        cdp_port: u16,
        mcp: &M,
    ) {
        self.emit_event(AgentEvent::CdpConnected {
            app_name: app_name.to_string(),
            port: cdp_port,
        })
        .await;
        if let Err(e) = mcp.refresh_server_tool_list().await {
            warn!(
                error = %e,
                "state-spine: post-CDP-connect tool-cache refresh failed",
            );
        }
    }

    /// Evaluate the permission policy for a tool call.
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
    /// `description_suffix` is appended to the human-facing description for
    /// callers that need extra context.
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
}

/// Parse a raw LLM response `Message` into an `AgentTurn` carrying
/// `0..N` task-state mutations followed by exactly one action.
///
/// We accept the turn via OpenAI-style `tool_calls`, which the LLM
/// emits as an ordered array. Each call is classified by name:
///
/// - **Mutation pseudo-tools** (`push_subgoal`, `complete_subgoal`,
///   `set_watch_slot`, `clear_watch_slot`, `record_hypothesis`,
///   `refute_hypothesis`) parse into `TaskStateMutation` values
///   regardless of position. Malformed args produce a per-call warning
///   but never abort the turn — a single bad mutation cannot poison
///   the action.
/// - **Action pseudo-tools** (`agent_done`, `agent_replan`) and any
///   other tool name become an `AgentAction`. The first action-shaped
///   call wins; subsequent action calls are dropped, since exactly one
///   action runs per turn. Mutations after the action are still
///   preserved — apply order is enforced by `apply_mutations`, not by
///   tool-call order.
///
/// If only mutations are present (the LLM forgot to choose an action),
/// the result is an `AgentReplan` with a self-describing reason so the
/// next turn re-observes instead of aborting.
///
/// Text-only replies (no `tool_calls`) also map to
/// `AgentAction::AgentReplan` with the assistant's raw text as the
/// reason — matches the legacy "no tool call" recovery hook.
pub fn parse_agent_turn(message: &Message) -> anyhow::Result<AgentTurn> {
    use crate::agent::prompt::is_mutation_tool_name;

    if let Some(tool_calls) = message.tool_calls.as_ref()
        && !tool_calls.is_empty()
    {
        let mut mutations: Vec<TaskStateMutation> = Vec::new();
        let mut action: Option<AgentAction> = None;

        for tc in tool_calls {
            let name = tc.function.name.as_str();
            let args = &tc.function.arguments;

            if is_mutation_tool_name(name) {
                match parse_mutation_call(name, args) {
                    Ok(m) => mutations.push(m),
                    Err(reason) => tracing::warn!(
                        tool = name,
                        error = %reason,
                        "state-spine: dropping malformed mutation pseudo-tool call"
                    ),
                }
                continue;
            }

            // Action — keep only the first one; exactly one action runs per turn.
            if action.is_some() {
                tracing::warn!(
                    tool = name,
                    "state-spine: ignoring extra action call after first action was claimed"
                );
                continue;
            }

            action = Some(match name {
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
                "invoke_skill" => {
                    let skill_id = args
                        .get("skill_id")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let version = args.get("version").and_then(Value::as_u64);
                    match (skill_id, version) {
                        (Some(skill_id), Some(version)) => match u32::try_from(version) {
                            Ok(version) => {
                                let parameters =
                                    args.get("parameters").cloned().unwrap_or(Value::Null);
                                AgentAction::InvokeSkill {
                                    skill_id,
                                    version,
                                    parameters,
                                }
                            }
                            Err(_) => {
                                tracing::warn!("state-spine: invoke_skill version out of range");
                                AgentAction::AgentReplan {
                                    reason: "invoke_skill version out of range".to_string(),
                                }
                            }
                        },
                        _ => {
                            tracing::warn!(
                                "state-spine: invoke_skill missing required fields — replanning"
                            );
                            AgentAction::AgentReplan {
                                reason: "invoke_skill missing required fields".to_string(),
                            }
                        }
                    }
                }
                _ => AgentAction::ToolCall {
                    tool_name: name.to_string(),
                    arguments: args.clone(),
                    tool_call_id: tc.id.clone(),
                },
            });
        }

        let action = action.unwrap_or_else(|| AgentAction::AgentReplan {
            reason: NO_ACTION_MUTATION_ONLY_REASON.to_string(),
        });

        return Ok(AgentTurn { mutations, action });
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

/// Parse a single mutation-shaped tool call (`push_subgoal`,
/// `complete_subgoal`, `set_watch_slot`, `clear_watch_slot`,
/// `record_hypothesis`, `refute_hypothesis`) into a `TaskStateMutation`.
///
/// Returns a human-readable reason on malformed arguments so the caller
/// can log per-call instead of aborting the whole turn. The strict
/// enforcement (e.g. "watch slot not set") happens later in
/// `TaskState::apply` and surfaces via `apply_mutations`'s warnings vec.
fn parse_mutation_call(name: &str, args: &Value) -> Result<TaskStateMutation, String> {
    use crate::agent::task_state::WatchSlotName;

    fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
        args.get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("missing required string field `{}`", key))
    }

    // Defer enum-tag validation to serde — `WatchSlotName` already
    // declares `#[serde(rename_all = "snake_case")]`, so the same
    // strings the pseudo-tool schema lists are accepted here without
    // a hand-maintained match arm.
    fn watch_slot_name(args: &Value) -> Result<WatchSlotName, String> {
        let raw = args
            .get("name")
            .ok_or_else(|| "missing required string field `name`".to_string())?;
        serde_json::from_value::<WatchSlotName>(raw.clone())
            .map_err(|e| format!("invalid watch slot name: {}", e))
    }

    match name {
        "push_subgoal" => Ok(TaskStateMutation::PushSubgoal {
            text: required_str(args, "text")?.to_string(),
        }),
        "complete_subgoal" => Ok(TaskStateMutation::CompleteSubgoal {
            summary: required_str(args, "summary")?.to_string(),
        }),
        "set_watch_slot" => Ok(TaskStateMutation::SetWatchSlot {
            name: watch_slot_name(args)?,
            note: required_str(args, "note")?.to_string(),
        }),
        "clear_watch_slot" => Ok(TaskStateMutation::ClearWatchSlot {
            name: watch_slot_name(args)?,
        }),
        "record_hypothesis" => Ok(TaskStateMutation::RecordHypothesis {
            text: required_str(args, "text")?.to_string(),
        }),
        "refute_hypothesis" => {
            let idx = args
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| "missing required non-negative integer field `index`".to_string())?;
            Ok(TaskStateMutation::RefuteHypothesis {
                index: idx as usize,
            })
        }
        _ => Err(format!("not a mutation pseudo-tool: `{}`", name)),
    }
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
    fn start_skill_watcher_if_enabled(&mut self) {
        if !self.skill_ctx.enabled
            || !self.config.skills_enabled
            || self.skill_watcher_handle.is_some()
        {
            return;
        }

        let mut dirs = Vec::new();
        let mut stores = Vec::new();

        let project_dir = self.skill_ctx.project_skills_dir.clone();
        if let Err(err) = std::fs::create_dir_all(&project_dir) {
            warn!(
                ?project_dir,
                ?err,
                "skills: failed to create project skills dir for watcher"
            );
            return;
        }
        dirs.push(project_dir);
        stores.push(self.skill_store.clone());

        if let Some(global_dir) = self.skill_ctx.global_skills_dir.clone() {
            if let Err(err) = std::fs::create_dir_all(&global_dir) {
                warn!(
                    ?global_dir,
                    ?err,
                    "skills: failed to create global skills dir for watcher"
                );
            } else {
                dirs.push(global_dir.clone());
                stores.push(Arc::new(SkillStore::new(global_dir)));
            }
        }

        match crate::agent::skills::watcher::SkillWatcher::spawn(dirs) {
            Ok(watcher) => {
                self.skill_watcher_handle = Some(
                    crate::agent::skills::watcher_consumer::WatcherConsumer::spawn_watcher(
                        self.skill_index.clone(),
                        stores,
                        watcher,
                    ),
                );
            }
            Err(err) => {
                warn!(
                    ?err,
                    "skills: watcher failed to start; external edits will be picked up on next run"
                );
            }
        }
    }

    /// Top-level observe → compose → LLM → parse → apply → dispatch →
    /// compact control loop. Task 3a.1 ships the minimum skeleton; later
    /// tasks (flagged by `TODO(task-3a.N)` markers inline) wire VLM
    /// verification, approval, loop detection,
    /// consecutive-destructive cap, workflow-graph emission, CDP
    /// auto-connect, synthetic `focus_window` skip, recovery strategy,
    /// and boundary `StepRecord` writes.
    ///
    /// Crate-private because the `Mcp` trait is `pub(crate)`; the public
    /// entry point stays [`crate::agent::run_agent_workflow`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run<B, M>(
        mut self,
        llm: &B,
        mcp: &M,
        goal: String,
        workflow: clickweave_core::Workflow,
        mcp_tools: Vec<Value>,
        anchor_node_id: Option<uuid::Uuid>,
    ) -> anyhow::Result<AgentState>
    where
        B: ChatBackend + ?Sized,
        M: Mcp + ?Sized,
    {
        self.start_skill_watcher_if_enabled();
        // Drain queued episodic writes on *every* exit path,
        // including the early `?` returns from chat/parse failures.
        // Without this, a recovery write queued moments before an LLM
        // failure would race the Tauri-side cleanup and never commit
        // before the writer is dropped, defeating the run-terminal
        // promotion barrier the post-loop flush already installs.
        let inner = Self::run_inner(
            &mut self,
            llm,
            mcp,
            goal,
            workflow,
            mcp_tools,
            anchor_node_id,
        );
        let result = inner.await;
        if let Some(writer) = &self.episodic_writer {
            writer.flush().await;
        }
        // Spec 3: clear the per-run scratch state so the runner could
        // in theory be reused. Files (the on-disk skill store) outlive
        // the runner — only the in-memory accumulators are dropped here.
        self.recorded_steps.clear();
        self.push_idx_stack.clear();
        self.push_signature_stack.clear();
        self.last_pushed_subgoal_ids.clear();
        self.completed_subgoal_extraction_queue.clear();
        self.produced_node_ids_stack.clear();
        self.pending_applicable_skills.clear();
        self.pre_dispatch_snapshot = None;
        if let Some(handle) = self.skill_watcher_handle.take() {
            handle.abort();
        }
        match result {
            Ok(()) => Ok(self.state),
            Err(e) => Err(e),
        }
    }

    async fn run_inner<B, M>(
        &mut self,
        llm: &B,
        mcp: &M,
        goal: String,
        workflow: clickweave_core::Workflow,
        mcp_tools: Vec<Value>,
        anchor_node_id: Option<uuid::Uuid>,
    ) -> anyhow::Result<()>
    where
        B: ChatBackend + ?Sized,
        M: Mcp + ?Sized,
    {
        use crate::agent::context::{CompactBudget, compact};
        use crate::agent::prompt::{
            UserTurnMessageInput, build_system_prompt, build_system_prompt_with_header,
            build_user_turn_message_from_input,
        };

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
        let system_text = if let Some(prompt) = self.agent_system_prompt_override.as_deref() {
            build_system_prompt_with_header(prompt, &tool_list_for_prompt)
        } else {
            build_system_prompt(&tool_list_for_prompt)
        };

        // Stable list of advertised MCP tool names for the per-turn
        // `<tools_in_scope>` filter. Computed once per run since `mcp_tools`
        // is itself stable across the loop (mid-run mutations would
        // invalidate the prompt-cache prefix).
        let advertised_tool_names: Vec<String> = tool_list_for_prompt
            .iter()
            .map(|t| t.name.clone())
            .collect();

        // `goal` already carries the prior-turn log + variant-context
        // composed by `build_goal_block` at the Tauri seam. Feed it
        // straight into the user turn so messages[1] is the single
        // run-specific slot.
        let initial_scope = self.compute_tools_in_scope(&advertised_tool_names);
        let initial_user = build_user_turn_message_from_input(UserTurnMessageInput {
            wm: &self.world_model,
            ts: &self.task_state,
            current_step: 0,
            observation_text: &goal,
            retrieved: &[],
            applicable_skills: &[],
            tools_in_scope_names: &initial_scope,
            max_elements: self.config.state_block_max_elements,
        });

        let mut messages = vec![Message::system(system_text), Message::user(initial_user)];

        // Add the pseudo-tools so the LLM sees the full state-spine
        // vocabulary: six task-state mutations plus the two action
        // pseudo-tools. Seeded once per run and never mutated — mid-run
        // tool-list changes invalidate every prior prompt-cache prefix.
        // The MCP tool set is unchanged: pseudo-tools do not dispatch
        // against `Mcp`; `parse_agent_turn` recognises them and routes
        // them into `AgentTurn.{mutations, action}` directly.
        let tools: Vec<Value> = mcp_tools
            .iter()
            .cloned()
            .chain(crate::agent::prompt::pseudo_tools())
            .collect();

        // Annotations index is seeded once per run so permission-policy
        // evaluation and the destructive cap see the same `read_only_hint`
        // / `destructive_hint` view.
        let annotations_by_tool = build_annotations_index(&mcp_tools);

        let budget = CompactBudget {
            recent_n: self.config.recent_n,
            ..CompactBudget::default()
        };
        let mut previous_result: Option<String> = None;
        // Loop detection: `(tool_name, arguments, error)` from the last
        // failing live call.
        let mut last_failure: Option<(String, Value, String)> = None;
        // No-progress detector for the success path. Failures already
        // abort on identical-error repeats via `last_failure` above;
        // this covers successful calls that don't advance the world.
        let mut last_action: Option<LastActionProgress> = None;
        let mut recent_actions: VecDeque<ActionProgressSignature> = VecDeque::new();
        let mut pending_text_submit_search: Option<TextSubmitSearchProgress> = None;

        for _step_index in 0..self.config.max_steps {
            if self.state.completed {
                break;
            }

            // 1. Observe — fetch elements + detect page transition.
            //    Capture the pre-mirror world-model signatures so the
            //    `WorldModelChanged` diff emitted by `run_turn` sees the
            //    direct-observation writes below. Only seed the
            //    baseline when it is empty: early-exit branches (policy
            //    deny, approval reject)
            //    skip `run_turn` entirely, so the baseline must persist
            //    across iterations until `run_turn.take()` consumes it.
            //    `run_turn` falls back to an internal snapshot when
            //    `None`, preserving the direct-driver test path.
            if self.turn_pre_signatures.is_none() {
                self.turn_pre_signatures = Some(self.world_model.field_signatures());
            }
            // Spec 3: snapshot the world model before this iteration's
            // dispatch so any successful tool call this turn pushes a
            // `RecordedStep` whose `world_model_pre` matches what the
            // LLM saw at decision time. Captured here before the
            // CDP-page summary mirror mutates `world_model.cdp_page`.
            self.pre_dispatch_snapshot = Some(
                crate::agent::step_record::WorldModelSnapshot::from_world_model(&self.world_model),
            );
            let CdpPageObservation {
                page_url,
                page_fingerprint,
                inventory,
            } = self.fetch_cdp_page_summary(mcp).await;

            // Mirror only the compact CDP page summary. CDP target
            // candidates are intentionally not written to
            // `world_model.elements`: the DOM is an ephemeral query
            // result and belongs in explicit `cdp_find_elements` /
            // `cdp_get_element_context` tool results, not in every
            // subsequent state block. Native AX/OCR element surfaces
            // still flow through `update_continuity_after_tool_success`.
            {
                use crate::agent::world_model::{
                    CdpPageState, Fresh, FreshnessSource, ObservedElement,
                };
                if matches!(
                    self.world_model
                        .elements
                        .as_ref()
                        .and_then(|f| f.value.first()),
                    Some(ObservedElement::Cdp(_))
                ) {
                    self.world_model.elements = None;
                }
                // `fetch_cdp_page_summary` writes the response `page_url` into
                // `state.current_url` on success and clears it on every
                // miss path (missing tool / parse failure / MCP error /
                // call failure). Mirror the URL + inventory-derived
                // fingerprint into `world_model.cdp_page` when fresh,
                // otherwise drop the stale page context entirely.
                let url = if page_url.is_empty() {
                    self.state.current_url.clone()
                } else {
                    page_url
                };
                if !url.is_empty() {
                    self.world_model.cdp_page = Some(Fresh {
                        value: CdpPageState {
                            url,
                            page_fingerprint,
                            element_inventory: inventory,
                        },
                        written_at: self.step_index,
                        source: FreshnessSource::DirectObservation,
                        ttl_steps: Some(2),
                    });
                } else {
                    self.world_model.cdp_page = None;
                }
            }
            let elements: Vec<clickweave_core::cdp::CdpFindElementMatch> = self
                .world_model
                .elements
                .as_ref()
                .map(|fresh| {
                    fresh
                        .value
                        .iter()
                        .filter_map(|element| match element {
                            crate::agent::world_model::ObservedElement::Cdp(match_) => {
                                Some(match_.clone())
                            }
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();

            // Top-of-loop observe — unconditional. Drains pending
            // invalidation events queued by the prior iteration's
            // dispatch (focus shift, navigation, app lifecycle, tool
            // failure) and re-infers `phase` so episodic retrieval and
            // prompt render both see the
            // post-event state. `run_turn` runs another `observe()`
            // after dispatch; the two passes are idempotent — events
            // are drained once per queue-and-observe cycle, so a second
            // call with no pending events is a no-op for invalidation
            // and just re-runs phase inference.
            //
            // `prev_phase_at_top` is captured *before* the observe so
            // the `Exploring/Executing -> Recovering` transition is
            // detectable here for episodic retrieval triggers.
            let prev_phase_at_top = self.task_state.phase;
            // Surface snapshot staleness as an invalidation event so
            // `observe()` drops AX / screenshot bodies that have aged
            // past their TTL even when no other event arrived.
            self.queue_snapshot_stale_if_aged();
            self.observe();
            if prev_phase_at_top != self.task_state.phase {
                self.emit_event(AgentEvent::TaskStateChanged {
                    run_id: self.run_id,
                    task_state: self.task_state.clone(),
                })
                .await;
            }

            // No pre-step CDP maybe-connect — legacy also defers the
            // decision to the post-tool hook (`maybe_cdp_connect` after
            // a successful `launch_app` / `focus_window`). CDP tools the
            // LLM picks before a connection exists return a "not
            // connected" MCP error that the recovery strategy absorbs.

            // Spec 2: episodic retrieval. `try_retrieve_episodic` returns
            // an empty vec when episodic is inactive or no trigger condition
            // fires.
            let retrieved = self.try_retrieve_episodic(prev_phase_at_top).await;

            // 2. Compose the per-turn user message with the state block +
            // the previous tool body as the observation, then compact the
            // history before the LLM call.
            let step_obs = previous_result.clone().unwrap_or_default();
            let step_scope = self.compute_tools_in_scope(&advertised_tool_names);
            // Spec 3: drain `pending_applicable_skills` once per turn —
            // the block surfaces in the next user turn after the
            // `push_subgoal` that produced it, then disappears.
            let applicable = std::mem::take(&mut self.pending_applicable_skills);
            let step_msg = build_user_turn_message_from_input(UserTurnMessageInput {
                wm: &self.world_model,
                ts: &self.task_state,
                current_step: self.step_index,
                observation_text: &step_obs,
                retrieved: &retrieved,
                applicable_skills: &applicable,
                tools_in_scope_names: &step_scope,
                max_elements: self.config.state_block_max_elements,
            });
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

            // 4. Parse the LLM response into an AgentTurn carrying any
            //    `0..N` task-state mutations followed by exactly one
            //    action.
            let mut turn = parse_agent_turn(&choice.message)?;
            if guard_completion_after_unverified_side_effect(previous_result.as_deref(), &mut turn)
            {
                warn!("state-spine: blocked completion after unverified side-effectful action");
                self.emit_event(AgentEvent::Warning {
                    message: UNVERIFIED_SIDE_EFFECT_COMPLETION_BLOCKED_REASON.to_string(),
                })
                .await;
            }

            // 4'. Apply task-state mutations BEFORE any early-exit
            //     branching. Synthetic focus-skip / live policy-deny /
            //     live approval-reject all record a step and `continue`
            //     before `run_turn` would otherwise apply them, so
            //     mutations would be silently dropped on those paths.
            //     Applying here guarantees the AgentTurn contract
            //     ("mutations apply before the action runs") regardless
            //     of which branch the action takes — and `run_turn`
            //     receives an action-only turn so it does not
            //     double-apply.
            //
            //     `outer_milestones_appended` counts CompleteSubgoal
            //     pops that landed in this turn so the
            //     SubgoalCompleted boundary write below still runs even
            //     when the action takes an early-exit branch.
            let outer_milestones_before = self.task_state.milestones.len();
            if !turn.mutations.is_empty() {
                let warnings = self.apply_mutations(&turn.mutations);
                for w in warnings {
                    tracing::warn!(warning = %w, "state-spine: mutation warning");
                }
                self.emit_event(AgentEvent::TaskStateChanged {
                    run_id: self.run_id,
                    task_state: self.task_state.clone(),
                })
                .await;
            }
            let outer_milestones_appended = self
                .task_state
                .milestones
                .len()
                .saturating_sub(outer_milestones_before);

            // 4''. SubgoalCompleted boundary writes — one StepRecord
            //      per CompleteSubgoal mutation that successfully
            //      popped a subgoal. Hoisted above the early-exit
            //      branches so a turn like `complete_subgoal` +
            //      skipped `focus_window` still produces the boundary
            //      record. Without this hoist, mutations would land on
            //      `task_state` (via 4') but the matching
            //      `BoundaryKind::SubgoalCompleted` `StepRecord` would
            //      be silently dropped whenever the action took the
            //      synthetic-skip / policy-deny / approval-reject
            //      `continue`.
            if outer_milestones_appended > 0 {
                self.write_subgoal_completed_records(outer_milestones_appended, &turn)
                    .await;
                reset_no_progress_tracking(&mut last_action, &mut recent_actions);
            }

            // 4''-bis. Spec 3: skill retrieval on `push_subgoal`.
            //          `apply_mutations` populates `last_pushed_subgoal_ids`;
            //          here we consume it once per turn and accumulate
            //          retrieved skills in `pending_applicable_skills` so
            //          the next user-turn render splices them into the
            //          `<applicable_skills>` block.
            //
            //          Runs *before* the synthetic-focus-skip / live-policy-
            //          deny / live-approval-reject early-exit branches so
            //          retrieval fires for every real `push_subgoal`
            //          regardless of which dispatch branch the action
            //          eventually takes.
            if self.skill_ctx.enabled
                && self.config.skills_enabled
                && !self.last_pushed_subgoal_ids.is_empty()
            {
                let pushed = std::mem::take(&mut self.last_pushed_subgoal_ids);
                let k = self.config.applicable_skills_k;
                for id in &pushed {
                    let Some(subgoal) = self
                        .task_state
                        .subgoal_stack
                        .iter()
                        .find(|s| s.id == *id)
                        .cloned()
                    else {
                        continue;
                    };
                    let subgoal_sig = crate::agent::skills::signature::compute_subgoal_signature(
                        &subgoal.text,
                        &self.world_model,
                    );
                    let app_sig = crate::agent::skills::signature::compute_applicability_signature(
                        &self.world_model,
                    );
                    let candidates = self.skill_index.read().lookup_at(
                        &subgoal_sig,
                        &app_sig,
                        &subgoal.text,
                        k,
                        chrono::Utc::now(),
                    );
                    self.pending_applicable_skills.extend(candidates);
                }
            }

            // 4''-ter. Spec 3: expand a resolved `invoke_skill` into
            // the first replayed tool call. Phase 4's full replay
            // engine still owns nested sub-skills, loops, capture
            // propagation, and divergence recovery; this bridge covers
            // confirmed leaf tool-call skills.
            if let AgentAction::InvokeSkill {
                skill_id,
                version,
                parameters,
            } = turn.action.clone()
            {
                turn.action = match self.dispatch_skill(&skill_id, version, parameters).await {
                    Ok(frame) => Self::skill_frame_to_single_step_action(&frame),
                    Err(reason) => AgentAction::AgentReplan { reason },
                };
            }

            // 4a. Synthetic `launch_app` skip for the no-focus policy.
            // A no-args launch of an already-running app is a focus
            // change in native-devtools. When background operation is
            // required, treat that as a successful app-state observation
            // and let the CDP lifecycle helper attach without sending the
            // foregrounding MCP call.
            if let AgentAction::ToolCall {
                tool_name,
                arguments,
                tool_call_id,
            } = &turn.action
                && tool_name == "launch_app"
                && let Some(running) = self.running_app_for_no_focus_launch(arguments, mcp).await
            {
                self.emit_event(AgentEvent::SubAction {
                    tool_name: "launch_app".to_string(),
                    summary: "skipped: app already running; focus changes disabled".to_string(),
                })
                .await;
                let skip_body = Self::skipped_launch_result_text(&running);
                debug!(
                    tool = "launch_app",
                    app = running.name,
                    "state-spine: suppressing launch_app for already-running app",
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
                self.advance_recorded_step_index();
                self.emit_world_model_changed_for_recorded_step().await;
                self.state.consecutive_errors = 0;
                self.consecutive_errors = 0;
                last_failure = None;
                self.clear_last_failure_tracking();
                self.maybe_cdp_connect(tool_name, arguments, &skip_body, mcp)
                    .await;
                previous_result = Some(skip_body.clone());
                if let Some(nudge) = self
                    .track_repeat_action(
                        tool_name,
                        arguments,
                        &skip_body,
                        &annotations_by_tool,
                        &mut last_action,
                        &mut recent_actions,
                    )
                    .await
                {
                    previous_result = Some(nudge);
                }
                self.emit_event(AgentEvent::StepCompleted {
                    step_index: step_idx_for_event,
                    tool_name: "launch_app".to_string(),
                    summary: crate::agent::prompt::truncate_summary(&skip_body, 120),
                })
                .await;
                append_assistant_and_tool_result(
                    &mut messages,
                    tool_name,
                    arguments,
                    tool_call_id,
                    previous_result.as_deref(),
                );
                continue;
            }

            force_background_launch_app(&mut turn.action, self.config.allow_focus_window);

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
                self.advance_recorded_step_index();
                self.emit_world_model_changed_for_recorded_step().await;
                self.state.consecutive_errors = 0;
                self.consecutive_errors = 0;
                last_failure = None;
                // Synthetic focus_window skip is a successful
                // observation outcome — clear failure tracking the
                // same way the live ToolSuccess path does.
                self.clear_last_failure_tracking();
                // `CdpAttachable` and the no-focus policy both require
                // app-scoped CDP acquisition. The post-tool
                // `maybe_cdp_connect` hook only runs on real ToolSuccess,
                // so synthetic skips must drive `auto_connect_cdp`
                // directly. On success the helper marks `cdp_state`
                // connected and clears `cdp_connect_status`; on failure
                // terminal paths record the reason. The actual
                // `world_model.cdp_page` write happens at the next
                // turn's `fetch_cdp_page_summary` mirror, so the finalizer
                // refreshes the MCP tool cache here.
                if let Some((app_name, kind_hint)) =
                    self.cdp_target_for_skipped_focus_window(reason, arguments, mcp)
                    && let Some(cdp_port) = self
                        .auto_connect_cdp(&app_name, kind_hint.as_deref(), mcp)
                        .await
                {
                    self.finalize_cdp_connected(&app_name, cdp_port, mcp).await;
                }
                previous_result = Some(skip_body.clone());
                // Synthetic skip is a successful dispatch from the LLM's
                // view — feed it through the same streak detector so a
                // run that keeps emitting `focus_window` against an
                // already-attached CDP target gets a no-progress nudge
                // instead of silent skips forever.
                if let Some(nudge) = self
                    .track_repeat_action(
                        tool_name,
                        arguments,
                        &skip_body,
                        &annotations_by_tool,
                        &mut last_action,
                        &mut recent_actions,
                    )
                    .await
                {
                    previous_result = Some(nudge);
                }
                self.emit_event(AgentEvent::StepCompleted {
                    step_index: step_idx_for_event,
                    tool_name: "focus_window".to_string(),
                    summary: crate::agent::prompt::truncate_summary(&skip_body, 120),
                })
                .await;
                append_assistant_and_tool_result(
                    &mut messages,
                    tool_name,
                    arguments,
                    tool_call_id,
                    previous_result.as_deref(),
                );
                continue;
            }

            // 4a-bis. Raw CDP lifecycle guard. The runner owns CDP
            // acquisition/release at app scope; a model-authored
            // `cdp_connect({"port": 9222})` can attach to any app
            // listening on that port, which is exactly the failure mode
            // this guard blocks. Surface a synthetic error and keep MCP
            // untouched.
            if let AgentAction::ToolCall {
                tool_name,
                arguments,
                tool_call_id,
            } = &turn.action
                && let Some(err_msg) = Self::raw_cdp_lifecycle_blocked(tool_name, arguments)
            {
                warn!(
                    tool = %tool_name,
                    "state-spine: raw CDP lifecycle tool blocked"
                );
                self.emit_event(AgentEvent::SubAction {
                    tool_name: tool_name.clone(),
                    summary: "blocked: CDP lifecycle is runtime-managed".to_string(),
                })
                .await;
                let step_idx_for_event = self.state.steps.len();
                self.state.steps.push(AgentStep {
                    index: step_idx_for_event,
                    elements: elements.clone(),
                    command: AgentCommand::ToolCall {
                        tool_name: tool_name.clone(),
                        arguments: arguments.clone(),
                        tool_call_id: tool_call_id.clone(),
                    },
                    outcome: StepOutcome::Error(err_msg.clone()),
                    page_url: self.state.current_url.clone(),
                });
                self.advance_recorded_step_index();
                self.emit_world_model_changed_for_recorded_step().await;
                self.state.consecutive_errors += 1;
                self.consecutive_errors = self.state.consecutive_errors;
                previous_result = Some(err_msg.clone());
                append_assistant_and_tool_result(
                    &mut messages,
                    tool_name,
                    arguments,
                    tool_call_id,
                    previous_result.as_deref(),
                );
                self.emit_event(AgentEvent::StepFailed {
                    step_index: step_idx_for_event,
                    tool_name: tool_name.clone(),
                    error: err_msg.clone(),
                })
                .await;
                let looped = matches!(
                    last_failure.as_ref(),
                    Some((prev_tool, prev_args, prev_err))
                        if prev_tool == tool_name
                            && prev_args == arguments
                            && prev_err == &err_msg
                );
                if looped {
                    warn!(
                        tool = %tool_name,
                        "state-spine: identical raw CDP lifecycle block repeated — aborting"
                    );
                    self.state.terminal_reason = Some(TerminalReason::LoopDetected {
                        tool_name: tool_name.clone(),
                        error: err_msg.clone(),
                    });
                    break;
                }
                last_failure = Some((tool_name.clone(), arguments.clone(), err_msg.clone()));
                let action = recovery_strategy(
                    self.state.consecutive_errors,
                    self.config.max_consecutive_errors,
                );
                if matches!(action, RecoveryAction::Abort) {
                    warn!(
                        errors = self.state.consecutive_errors,
                        "state-spine: too many consecutive raw CDP lifecycle blocks — aborting"
                    );
                    self.state.terminal_reason = Some(TerminalReason::MaxErrorsReached {
                        consecutive_errors: self.state.consecutive_errors,
                    });
                    break;
                }
                reset_no_progress_tracking(&mut last_action, &mut recent_actions);
                continue;
            }

            // 4a-bis. Coordinate-primitive guard. Defense-in-depth
            // behind the per-turn `<tools_in_scope>` filter: the filter
            // narrows the LLM-facing tool list, but a wrong-family
            // dispatch can still reach this point via a malformed turn
            // or future replay path. When a
            // structured surface is wired (CDP page attached, or
            // Native focus + AX dispatch toolset), reject the
            // coordinate primitive with a synthetic `StepOutcome::Error`
            // so it never hits MCP.
            if let AgentAction::ToolCall {
                tool_name,
                arguments,
                tool_call_id,
            } = &turn.action
                && let Some(err_msg) = self.coordinate_primitive_blocked(tool_name, mcp)
            {
                warn!(
                    tool = %tool_name,
                    "state-spine: coordinate primitive blocked by structured-surface guard"
                );
                self.emit_event(AgentEvent::SubAction {
                    tool_name: tool_name.clone(),
                    summary: "blocked: structured surface wired (CDP/AX)".to_string(),
                })
                .await;
                let step_idx_for_event = self.state.steps.len();
                self.state.steps.push(AgentStep {
                    index: step_idx_for_event,
                    elements: elements.clone(),
                    command: AgentCommand::ToolCall {
                        tool_name: tool_name.clone(),
                        arguments: arguments.clone(),
                        tool_call_id: tool_call_id.clone(),
                    },
                    outcome: StepOutcome::Error(err_msg.clone()),
                    page_url: self.state.current_url.clone(),
                });
                self.advance_recorded_step_index();
                self.emit_world_model_changed_for_recorded_step().await;
                self.state.consecutive_errors += 1;
                self.consecutive_errors = self.state.consecutive_errors;
                previous_result = Some(err_msg.clone());
                append_assistant_and_tool_result(
                    &mut messages,
                    tool_name,
                    arguments,
                    tool_call_id,
                    previous_result.as_deref(),
                );
                self.emit_event(AgentEvent::StepFailed {
                    step_index: step_idx_for_event,
                    tool_name: tool_name.clone(),
                    error: err_msg.clone(),
                })
                .await;
                let looped = matches!(
                    last_failure.as_ref(),
                    Some((prev_tool, prev_args, prev_err))
                        if prev_tool == tool_name
                            && prev_args == arguments
                            && prev_err == &err_msg
                );
                if looped {
                    warn!(
                        tool = %tool_name,
                        "state-spine: identical coordinate-primitive block repeated — aborting"
                    );
                    self.state.terminal_reason = Some(TerminalReason::LoopDetected {
                        tool_name: tool_name.clone(),
                        error: err_msg.clone(),
                    });
                    break;
                }
                last_failure = Some((tool_name.clone(), arguments.clone(), err_msg.clone()));
                let action = recovery_strategy(
                    self.state.consecutive_errors,
                    self.config.max_consecutive_errors,
                );
                if matches!(action, RecoveryAction::Abort) {
                    warn!(
                        errors = self.state.consecutive_errors,
                        "state-spine: too many consecutive coordinate-primitive blocks — aborting"
                    );
                    self.state.terminal_reason = Some(TerminalReason::MaxErrorsReached {
                        consecutive_errors: self.state.consecutive_errors,
                    });
                    break;
                }
                reset_no_progress_tracking(&mut last_action, &mut recent_actions);
                continue;
            }

            // 4a. Permission policy + approval gate for live `ToolCall`
            // actions. Mirrors the legacy `AgentRunner::execute_response`
            // pre-dispatch policy check. Observation tools bypass approval.
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
                            let step_idx_for_event = self.state.steps.len();
                            self.state.steps.push(AgentStep {
                                index: step_idx_for_event,
                                elements: elements.clone(),
                                command: AgentCommand::ToolCall {
                                    tool_name: tool_name.clone(),
                                    arguments: arguments.clone(),
                                    tool_call_id: tool_call_id.clone(),
                                },
                                outcome: StepOutcome::Error(err_msg.clone()),
                                page_url: self.state.current_url.clone(),
                            });
                            self.advance_recorded_step_index();
                            self.emit_world_model_changed_for_recorded_step().await;
                            // Shared with other policy-deny paths — see
                            // `record_policy_deny_failure` for rationale.
                            self.record_policy_deny_failure(tool_name);
                            self.state.consecutive_errors += 1;
                            self.consecutive_errors = self.state.consecutive_errors;
                            previous_result = Some(err_msg.clone());
                            append_assistant_and_tool_result(
                                &mut messages,
                                tool_name,
                                arguments,
                                tool_call_id,
                                previous_result.as_deref(),
                            );

                            // Parity with the `TurnOutcome::ToolError` path:
                            // emit `StepFailed`, honor loop-detection on the
                            // identical `(tool, args, error)` tuple, and
                            // respect the `recovery_strategy` so repeated
                            // policy denials hit the same `MaxErrorsReached`
                            // terminal state as real MCP errors.
                            self.emit_event(AgentEvent::StepFailed {
                                step_index: step_idx_for_event,
                                tool_name: tool_name.clone(),
                                error: err_msg.clone(),
                            })
                            .await;

                            let looped = matches!(
                                last_failure.as_ref(),
                                Some((prev_tool, prev_args, prev_err))
                                    if prev_tool == tool_name
                                        && prev_args == arguments
                                        && prev_err == &err_msg
                            );
                            if looped {
                                warn!(
                                    tool = %tool_name,
                                    "state-spine: identical policy-deny repeated — aborting"
                                );
                                self.state.terminal_reason = Some(TerminalReason::LoopDetected {
                                    tool_name: tool_name.clone(),
                                    error: err_msg.clone(),
                                });
                                break;
                            }
                            last_failure =
                                Some((tool_name.clone(), arguments.clone(), err_msg.clone()));

                            let action = recovery_strategy(
                                self.state.consecutive_errors,
                                self.config.max_consecutive_errors,
                            );
                            if matches!(action, RecoveryAction::Abort) {
                                warn!(
                                    errors = self.state.consecutive_errors,
                                    "state-spine: too many consecutive policy denials — aborting"
                                );
                                self.state.terminal_reason =
                                    Some(TerminalReason::MaxErrorsReached {
                                        consecutive_errors: self.state.consecutive_errors,
                                    });
                                break;
                            }
                            // Denied dispatch breaks the success streak — the
                            // intended action did not run, so any prior
                            // `last_action` no longer represents a consecutive
                            // chain.
                            reset_no_progress_tracking(&mut last_action, &mut recent_actions);
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
                                    // — matches the legacy `StepOutcome::Replan`
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
                                    self.advance_recorded_step_index();
                                    self.emit_world_model_changed_for_recorded_step().await;
                                    previous_result =
                                        Some("Replan: user rejected action".to_string());
                                    append_assistant_and_tool_result(
                                        &mut messages,
                                        tool_name,
                                        arguments,
                                        tool_call_id,
                                        previous_result.as_deref(),
                                    );
                                    // Rejected dispatch breaks the streak.
                                    reset_no_progress_tracking(
                                        &mut last_action,
                                        &mut recent_actions,
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

            // 5. Dispatch the action via run_turn. Mutations were
            //    already applied at step 4' above, so we forward an
            //    action-only turn — `run_turn`'s internal
            //    `apply_mutations` call becomes a no-op on the empty
            //    vec and `TaskStateChanged` is not emitted twice.
            //
            //    `previous_errors` captures the error counter from the
            //    iteration just before the new turn; a drop from >0 to
            //    0 after `run_turn` signals the
            //    `Recovering -> Executing` transition persisted as a
            //    `BoundaryKind::RecoverySucceeded` record.
            let previous_errors = self.consecutive_errors;
            let executor = McpToolExecutor { mcp };
            let action_only_turn = AgentTurn {
                mutations: Vec::new(),
                action: turn.action.clone(),
            };
            let (outcome, warnings, _run_turn_milestones) =
                self.run_turn(&action_only_turn, &executor).await;
            for w in warnings {
                tracing::warn!(warning = %w, "state-spine: mutation warning");
            }

            // 5b. RecoverySucceeded boundary write — a tool success that
            //     cleared the consecutive-error streak (D8). Only fires on
            //     the exact `Recovering -> Executing` transition: the
            //     previous turn had errors, this turn brought the counter
            //     to zero. Tool calls that never errored (previous_errors
            //     == 0) and repeated-error turns (consecutive_errors > 0)
            //     are both skipped.
            // Spec 2: per-turn bookkeeping for the episodic write/retrieve
            // path. Tool failures populate `last_failed_*` (consumed by
            // `try_retrieve_episodic` when capturing a `Recovering`-entry
            // snapshot); successes clear it. While in `Recovering`, push
            // a `CompactAction` per dispatched tool so the eventual
            // write carries the full recovery action sequence.
            if self.episodic_active() {
                match &outcome {
                    TurnOutcome::ToolError { tool_name, error } => {
                        self.last_failed_tool_name = Some(tool_name.clone());
                        self.last_failed_error_kind = Some(error.clone());
                    }
                    TurnOutcome::ToolSuccess { .. } => {
                        self.clear_last_failure_tracking();
                    }
                    _ => {}
                }
                if self.task_state.phase == crate::agent::phase::Phase::Recovering
                    && let AgentAction::ToolCall {
                        tool_name,
                        arguments,
                        ..
                    } = &turn.action
                {
                    let outcome_kind = match &outcome {
                        TurnOutcome::ToolSuccess { .. } => "ok",
                        TurnOutcome::ToolError { .. } => "error",
                        TurnOutcome::Done { .. } => "done",
                        TurnOutcome::Replan { .. } => "replan",
                    };
                    let brief_args = brief_summarize_args(arguments);
                    self.recovery_actions_accumulator.push(
                        crate::agent::episodic::types::CompactAction {
                            tool_name: tool_name.clone(),
                            brief_args,
                            outcome_kind: outcome_kind.to_string(),
                        },
                    );
                }
            }

            if previous_errors > 0
                && self.consecutive_errors == 0
                && matches!(outcome, TurnOutcome::ToolSuccess { .. })
            {
                self.write_recovery_succeeded_record(&turn, &outcome).await;

                // Spec 2: queue an episodic-memory write for this
                // recovery (D30). Best-effort — backpressure / disabled
                // writer / missing snapshot are all silent no-ops so
                // the agent loop keeps running on D32.
                if self.episodic_active()
                    && let Some(entry) = self.recovering_snapshot.take()
                    && let Some(writer) = &self.episodic_writer
                {
                    let actions = std::mem::take(&mut self.recovery_actions_accumulator);
                    let record = self.build_step_record(
                        crate::agent::step_record::BoundaryKind::RecoverySucceeded,
                        serde_json::to_value(&turn.action)
                            .unwrap_or_else(|_| serde_json::json!({})),
                        serde_json::json!({"kind": "tool_success"}),
                    );
                    let queue_result = writer
                        .queue(
                            crate::agent::episodic::types::WriteRequest::DeriveAndInsert {
                                entry: Box::new(entry),
                                recovery_success: Box::new(record),
                                recovery_actions: actions,
                            },
                        )
                        .await;
                    // Surface backpressure drops as a Warning event so
                    // consumers can distinguish "no recovery happened"
                    // from "recovery succeeded but the episodic write
                    // was dropped." D32 keeps the agent loop running
                    // either way; this is purely observability.
                    if let Err(e) = queue_result {
                        self.emit_event(AgentEvent::Warning {
                            message: format!("episodic: write dropped: backpressure ({e})"),
                        })
                        .await;
                    }
                }
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
                    self.advance_recorded_step_index();
                    // Spec 3: push a `RecordedStep` parallel to the
                    // `AgentStep` push so the extractor can read this
                    // tool dispatch back at the next CompleteSubgoal
                    // boundary. `world_model_pre` is the snapshot taken
                    // before the iteration's observe + fetch (captured
                    // at the top of the loop into `pre_dispatch_snapshot`);
                    // `world_model_post` is the live world model now
                    // that `update_continuity_after_tool_success` and
                    // queued invalidations have applied.
                    let tool_arguments_for_record = match &turn.action {
                        AgentAction::ToolCall { arguments, .. } => arguments.clone(),
                        _ => unreachable!("ToolSuccess outcome implies ToolCall action"),
                    };
                    let unverified_side_effect = is_unverified_side_effect_action(
                        &tool_name,
                        &tool_arguments_for_record,
                        &annotations_by_tool,
                    );
                    let pre_snapshot = self.pre_dispatch_snapshot.take().unwrap_or_else(|| {
                        crate::agent::step_record::WorldModelSnapshot::from_world_model(
                            &self.world_model,
                        )
                    });
                    let post_snapshot =
                        crate::agent::step_record::WorldModelSnapshot::from_world_model(
                            &self.world_model,
                        );
                    self.recorded_steps.push(RecordedStep {
                        tool_name: tool_name.clone(),
                        arguments: tool_arguments_for_record,
                        result_text: tool_body.clone(),
                        world_model_pre: pre_snapshot,
                        world_model_post: post_snapshot,
                    });
                    let unverified_side_effect_nudge = if unverified_side_effect {
                        Some(build_unverified_side_effect_nudge(&tool_body))
                    } else {
                        None
                    };
                    previous_result = Some(
                        unverified_side_effect_nudge
                            .clone()
                            .unwrap_or(tool_body.clone()),
                    );
                    // Clear the loop-detection tracker on any success.
                    last_failure = None;
                    // Emit StepCompleted so subscribers see a successful turn.
                    self.emit_event(AgentEvent::StepCompleted {
                        step_index: step_idx_for_event,
                        tool_name: tool_name.clone(),
                        summary: crate::agent::prompt::truncate_summary(&tool_body, 120),
                    })
                    .await;
                    if unverified_side_effect {
                        self.emit_event(AgentEvent::Warning {
                            message: format!(
                                "{}: `{}` result requires verification before completion",
                                UNVERIFIED_SIDE_EFFECT_PREFIX, tool_name
                            ),
                        })
                        .await;
                    }
                    // Destructive-cap accounting mirrors
                    // `AgentRunner::handle_step_outcome`'s cap branch.
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
                    // the top of `run`). Boundary extraction records produced
                    // node ids into draft-skill lineage so selective-delete
                    // can prune derived skills later.
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
                    // Spec 3: track every node produced inside each
                    // active subgoal frame. Drained at
                    // `complete_subgoal` so the extracted skill records
                    // its `produced_node_ids` lineage.
                    if let Some(node_id) = produced_node_id {
                        self.record_produced_node_id(node_id);
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

                    if let Some(nudge) = self
                        .track_post_text_submit_search(
                            &tool_name,
                            &tool_arguments,
                            &tool_body,
                            &mut pending_text_submit_search,
                        )
                        .await
                    {
                        previous_result = Some(match unverified_side_effect_nudge.as_deref() {
                            Some(side_effect_nudge) => {
                                format!("{side_effect_nudge}\n\n{nudge}")
                            }
                            None => nudge,
                        });
                    }

                    if let Some(nudge) = self
                        .track_repeat_action(
                            &tool_name,
                            &tool_arguments,
                            &tool_body,
                            &annotations_by_tool,
                            &mut last_action,
                            &mut recent_actions,
                        )
                        .await
                    {
                        previous_result = Some(match unverified_side_effect_nudge.as_deref() {
                            Some(side_effect_nudge) => {
                                format!("{side_effect_nudge}\n\n{nudge}")
                            }
                            None => nudge,
                        });
                    }
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
                    self.advance_recorded_step_index();
                    self.state.consecutive_errors = self.consecutive_errors;
                    previous_result = Some(error.clone());

                    // Emit StepFailed so subscribers see the failing turn.
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
                    // Failures use `last_failure`; clear the success-side tracker.
                    reset_no_progress_tracking(&mut last_action, &mut recent_actions);

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
                    // Replan = explicit tactic change; reset the streak.
                    reset_no_progress_tracking(&mut last_action, &mut recent_actions);
                }
            }

            // 7. Append the assistant action + its result onto the
            // transcript so the next iteration's LLM call sees what
            // ran. Mutation pseudo-tools that may have appeared in the
            // model's `tool_calls` array are deliberately omitted here:
            // they are already reflected in the `<task_state>` block
            // the next turn renders, and including them would invite
            // the LLM to expect an MCP-shaped tool result for them.
            // `AgentDone` already broke out of the loop above; that
            // path leaves no trailing transcript entry.
            match &turn.action {
                AgentAction::ToolCall {
                    tool_name,
                    arguments,
                    tool_call_id,
                } => {
                    append_assistant_and_tool_result(
                        &mut messages,
                        tool_name,
                        arguments,
                        tool_call_id,
                        previous_result.as_deref(),
                    );
                }
                AgentAction::AgentReplan { reason } => {
                    // Surface the replan as a plain assistant message
                    // rather than a synthetic tool result; the harness
                    // does not produce a tool_call_id for it and there
                    // is no MCP body to attach.
                    messages.push(Message::assistant(format!("replan: {}", reason)));
                }
                AgentAction::AgentDone { .. } => {
                    // `TurnOutcome::Done` already broke above — this
                    // arm is unreachable in practice but kept exhaustive
                    // so the matcher does not silently regress.
                }
                AgentAction::InvokeSkill { .. } => {
                    // The replay engine appends its own per-step entries
                    // through `dispatch_tool_call_through_helper`. The
                    // outer transcript site has nothing to add for the
                    // synthetic invoke_skill call itself.
                }
            }
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

        // Drain happens in the outer `run` wrapper so it covers both
        // `Ok` and early-`?` `Err` exits from this function. See the
        // post-result `writer.flush().await` in `Self::run`.
        Ok(())
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
/// Append an assistant tool-call + matching tool-result onto the
/// transcript so the next iteration's LLM call sees what was
/// dispatched. Synthesises the assistant message from the action's own
/// `(tool_call_id, tool_name, arguments)` rather than picking
/// `tool_calls.first()`: when a turn's `tool_calls` array starts with
/// mutation pseudo-tools (e.g. `push_subgoal` then `cdp_click`), the
/// "first call" is a mutation, not the action that actually ran, and
/// attaching the dispatched result to that id breaks action / result
/// causality from the LLM's point of view. Mutations are already
/// reflected in `<task_state>` at the next turn; they do not appear in
/// the transcript here.
///
/// The tool-result's `name` is stamped so `context::compact` can
/// identify stale snapshot-family bodies by the `SNAPSHOT_TOOL_NAMES`
/// set. Without this stamp, production tool-result messages leave
/// `name` unset and the snapshot-drop branch never fires for live
/// runs.
fn append_assistant_and_tool_result(
    messages: &mut Vec<Message>,
    tool_name: &str,
    arguments: &Value,
    tool_call_id: &str,
    previous_result: Option<&str>,
) {
    let tc = clickweave_llm::ToolCall {
        id: tool_call_id.to_string(),
        call_type: clickweave_llm::CallType::Function,
        function: clickweave_llm::FunctionCall {
            name: tool_name.to_string(),
            arguments: arguments.clone(),
        },
    };
    messages.push(Message::assistant_tool_calls(vec![tc]));
    let mut tool_msg = Message::tool_result(tool_call_id, previous_result.unwrap_or("ok"));
    tool_msg.name = Some(tool_name.to_string());
    messages.push(tool_msg);
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
        let (tx, _rx) = mpsc::channel::<RunnerOutput>(8);
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
        // The design's error-path table says a malformed AgentTurn
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
mod parse_agent_turn_tool_calls_tests {
    //! Tests for the live `parse_agent_turn(&Message)` parser that
    //! consumes OpenAI-shaped `tool_calls`. Distinct from the JSON
    //! envelope tests above, which exercise the `serde::Deserialize`
    //! path for `AgentTurn`.

    use super::*;
    use crate::agent::task_state::WatchSlotName;
    use clickweave_llm::{CallType, FunctionCall, Message, ToolCall};
    use serde_json::json;

    fn tc(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            call_type: CallType::Function,
            function: FunctionCall {
                name: name.to_string(),
                arguments: args,
            },
        }
    }

    #[test]
    fn maps_mcp_tool_call_to_tool_call_action_with_no_mutations() {
        let msg = Message::assistant_tool_calls(vec![tc("tc1", "cdp_click", json!({"uid": "d5"}))]);
        let turn = parse_agent_turn(&msg).unwrap();
        assert!(turn.mutations.is_empty());
        match turn.action {
            AgentAction::ToolCall { tool_name, .. } => assert_eq!(tool_name, "cdp_click"),
            _ => panic!("expected tool_call"),
        }
    }

    #[test]
    fn maps_agent_done_pseudo_tool_to_agent_done_action() {
        let msg = Message::assistant_tool_calls(vec![tc(
            "tc1",
            "agent_done",
            json!({"summary": "logged in"}),
        )]);
        let turn = parse_agent_turn(&msg).unwrap();
        match turn.action {
            AgentAction::AgentDone { summary } => assert_eq!(summary, "logged in"),
            _ => panic!("expected agent_done"),
        }
    }

    #[test]
    fn maps_invoke_skill_pseudo_tool_to_invoke_skill_action() {
        let msg = Message::assistant_tool_calls(vec![tc(
            "tc1",
            "invoke_skill",
            json!({
                "skill_id": "open_settings",
                "version": 2,
                "parameters": {"app": "Notes"}
            }),
        )]);
        let turn = parse_agent_turn(&msg).unwrap();
        match turn.action {
            AgentAction::InvokeSkill {
                skill_id,
                version,
                parameters,
            } => {
                assert_eq!(skill_id, "open_settings");
                assert_eq!(version, 2);
                assert_eq!(parameters, json!({"app": "Notes"}));
            }
            other => panic!("expected invoke_skill, got {:?}", other),
        }
    }

    #[test]
    fn invoke_skill_missing_required_fields_replans() {
        // Missing `version` — the parser cannot fabricate a sensible
        // default, so degrades to a replan instead of dispatching a
        // skill that won't resolve.
        let msg = Message::assistant_tool_calls(vec![tc(
            "tc1",
            "invoke_skill",
            json!({"skill_id": "open_settings"}),
        )]);
        let turn = parse_agent_turn(&msg).unwrap();
        assert!(matches!(turn.action, AgentAction::AgentReplan { .. }));
    }

    #[test]
    fn invoke_skill_version_overflow_replans_instead_of_wrapping() {
        let msg = Message::assistant_tool_calls(vec![tc(
            "tc1",
            "invoke_skill",
            json!({
                "skill_id": "open_settings",
                "version": u64::from(u32::MAX) + 1,
                "parameters": {}
            }),
        )]);
        let turn = parse_agent_turn(&msg).unwrap();
        match turn.action {
            AgentAction::AgentReplan { reason } => {
                assert!(reason.contains("out of range"));
            }
            other => panic!("expected replan for overflow, got {:?}", other),
        }
    }

    #[test]
    fn collects_mutations_then_takes_first_action_call() {
        let msg = Message::assistant_tool_calls(vec![
            tc("m1", "push_subgoal", json!({"text": "open login"})),
            tc(
                "m2",
                "record_hypothesis",
                json!({"text": "form has 2 fields"}),
            ),
            tc("a1", "cdp_find_elements", json!({})),
            // Extra action calls after the first action are dropped.
            tc("a2", "cdp_click", json!({"uid": "d2"})),
        ]);
        let turn = parse_agent_turn(&msg).unwrap();
        assert_eq!(turn.mutations.len(), 2);
        assert!(matches!(
            turn.mutations[0],
            TaskStateMutation::PushSubgoal { .. }
        ));
        assert!(matches!(
            turn.mutations[1],
            TaskStateMutation::RecordHypothesis { .. }
        ));
        match turn.action {
            AgentAction::ToolCall { tool_name, .. } => assert_eq!(tool_name, "cdp_find_elements"),
            _ => panic!("expected first action to win"),
        }
    }

    #[test]
    fn mutations_after_action_are_still_collected() {
        // Apply order is `apply_mutations` -> action; tool-call array
        // ordering is irrelevant. A mutation emitted after the action
        // is still picked up so the parser is robust to LLM sloppiness.
        let msg = Message::assistant_tool_calls(vec![
            tc("a1", "agent_done", json!({"summary": "done"})),
            tc("m1", "push_subgoal", json!({"text": "noted"})),
        ]);
        let turn = parse_agent_turn(&msg).unwrap();
        assert_eq!(turn.mutations.len(), 1);
        assert!(matches!(turn.action, AgentAction::AgentDone { .. }));
    }

    #[test]
    fn only_mutations_synthesizes_agent_replan() {
        // The LLM emitted state mutations but no action — surface as a
        // replan so the next turn re-observes instead of aborting.
        let msg = Message::assistant_tool_calls(vec![tc(
            "m1",
            "push_subgoal",
            json!({"text": "explore"}),
        )]);
        let turn = parse_agent_turn(&msg).unwrap();
        assert_eq!(turn.mutations.len(), 1);
        match turn.action {
            AgentAction::AgentReplan { reason } => {
                assert!(reason.starts_with(NO_ACTION_MUTATION_ONLY_PREFIX));
                assert!(reason.contains("no MCP/environment action ran"));
            }
            other => panic!("expected mutation-only replan, got {:?}", other),
        }
    }

    #[test]
    fn malformed_mutation_is_dropped_without_aborting_turn() {
        // `set_watch_slot` requires both `name` and `note`; a missing
        // field drops just that mutation while letting subsequent
        // mutations and the action through.
        let msg = Message::assistant_tool_calls(vec![
            tc("m_bad", "set_watch_slot", json!({"name": "pending_modal"})),
            tc(
                "m_good",
                "set_watch_slot",
                json!({"name": "pending_auth", "note": "captcha shown"}),
            ),
            tc("a1", "agent_replan", json!({"reason": "auth required"})),
        ]);
        let turn = parse_agent_turn(&msg).unwrap();
        assert_eq!(turn.mutations.len(), 1);
        match &turn.mutations[0] {
            TaskStateMutation::SetWatchSlot { name, .. } => {
                assert_eq!(*name, WatchSlotName::PendingAuth)
            }
            _ => panic!("expected set_watch_slot for pending_auth"),
        }
        assert!(matches!(turn.action, AgentAction::AgentReplan { .. }));
    }

    #[test]
    fn refute_hypothesis_parses_index() {
        let msg = Message::assistant_tool_calls(vec![
            tc("m1", "refute_hypothesis", json!({"index": 3})),
            tc("a1", "agent_replan", json!({"reason": "wrong"})),
        ]);
        let turn = parse_agent_turn(&msg).unwrap();
        assert!(matches!(
            turn.mutations[0],
            TaskStateMutation::RefuteHypothesis { index: 3 }
        ));
    }

    #[test]
    fn unknown_watch_slot_name_drops_mutation() {
        let msg = Message::assistant_tool_calls(vec![
            tc(
                "m1",
                "set_watch_slot",
                json!({"name": "made_up_slot", "note": "x"}),
            ),
            tc("a1", "agent_replan", json!({"reason": "ok"})),
        ]);
        let turn = parse_agent_turn(&msg).unwrap();
        assert!(turn.mutations.is_empty());
    }

    #[test]
    fn empty_tool_calls_array_falls_back_to_text_replan() {
        // `assistant_tool_calls(vec![])` with no content emits a replan
        // with the no-call sentinel reason, mirroring text-only output.
        let msg = Message::assistant_tool_calls(vec![]);
        let turn = parse_agent_turn(&msg).unwrap();
        match turn.action {
            AgentAction::AgentReplan { reason } => {
                assert!(reason.contains("no tool call") || reason.is_empty());
            }
            _ => panic!("expected agent_replan fallback"),
        }
    }
}

#[cfg(test)]
mod unverified_side_effect_guard_tests {
    use super::*;
    use crate::agent::permissions::ToolAnnotations;
    use crate::agent::task_state::TaskStateMutation;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn detects_obvious_side_effectful_eval_but_allows_read_only_eval() {
        let annotations = HashMap::new();

        assert!(is_unverified_side_effect_action(
            "cdp_evaluate_script",
            &json!({"function": "() => document.querySelector('button')?.click()"}),
            &annotations,
        ));
        assert!(!is_unverified_side_effect_action(
            "cdp_evaluate_script",
            &json!({"function": "() => Array.from(document.querySelectorAll('button')).map((b) => b.textContent)"}),
            &annotations,
        ));
    }

    #[test]
    fn uses_open_world_destructive_annotations_for_future_tools() {
        let mut annotations = HashMap::new();
        annotations.insert(
            "future_action".to_string(),
            ToolAnnotations {
                destructive_hint: Some(true),
                open_world_hint: Some(true),
                ..ToolAnnotations::default()
            },
        );

        assert!(is_unverified_side_effect_action(
            "future_action",
            &json!({}),
            &annotations,
        ));
    }

    #[test]
    fn guard_completion_after_unverified_side_effect_strips_complete_and_blocks_done() {
        let mut turn = AgentTurn {
            mutations: vec![
                TaskStateMutation::PushSubgoal {
                    text: "find target".to_string(),
                },
                TaskStateMutation::CompleteSubgoal {
                    summary: "target open".to_string(),
                },
            ],
            action: AgentAction::AgentDone {
                summary: "done".to_string(),
            },
        };

        assert!(guard_completion_after_unverified_side_effect(
            Some("[UNVERIFIED SIDE EFFECT] previous result"),
            &mut turn,
        ));
        assert_eq!(turn.mutations.len(), 1);
        assert!(matches!(
            turn.mutations[0],
            TaskStateMutation::PushSubgoal { .. }
        ));
        assert!(matches!(turn.action, AgentAction::AgentReplan { .. }));
    }
}

#[cfg(test)]
mod no_progress_guard_tests {
    use super::*;
    use crate::agent::world_model::{CdpPageState, Fresh, FreshnessSource, OcrMatch};
    use clickweave_core::cdp::CdpFindElementMatch;
    use serde_json::json;

    fn sig(
        tool_name: &str,
        arguments: serde_json::Value,
        context: &str,
    ) -> ActionProgressSignature {
        ActionProgressSignature {
            tool_name: tool_name.to_string(),
            arguments,
            context_signature: context.to_string(),
        }
    }

    #[test]
    fn detects_two_action_cycle_in_same_stable_context() {
        let recent = VecDeque::from(vec![
            sig(
                "cdp_fill",
                json!({"uid": "d1", "value": "synthetic"}),
                "ctx",
            ),
            sig("cdp_click", json!({"uid": "d2"}), "ctx"),
            sig(
                "cdp_fill",
                json!({"uid": "d1", "value": "synthetic"}),
                "ctx",
            ),
            sig("cdp_click", json!({"uid": "d2"}), "ctx"),
        ]);

        assert_eq!(
            detect_repeated_action_cycle(&recent),
            Some(vec!["cdp_fill".to_string(), "cdp_click".to_string()])
        );
    }

    #[test]
    fn detects_three_action_cycle_in_same_stable_context() {
        let recent = VecDeque::from(vec![
            sig(
                "cdp_fill",
                json!({"uid": "d-search", "value": "synthetic"}),
                "ctx",
            ),
            sig("cdp_click", json!({"uid": "d-filter"}), "ctx"),
            sig("cdp_click", json!({"uid": "d-cancel"}), "ctx"),
            sig(
                "cdp_fill",
                json!({"uid": "d-search", "value": "synthetic"}),
                "ctx",
            ),
            sig("cdp_click", json!({"uid": "d-filter"}), "ctx"),
            sig("cdp_click", json!({"uid": "d-cancel"}), "ctx"),
        ]);

        assert_eq!(
            detect_repeated_action_cycle(&recent),
            Some(vec![
                "cdp_fill".to_string(),
                "cdp_click".to_string(),
                "cdp_click".to_string(),
            ])
        );
    }

    #[test]
    fn ignores_same_pair_after_context_progress() {
        let recent = VecDeque::from(vec![
            sig(
                "cdp_fill",
                json!({"uid": "d1", "value": "synthetic"}),
                "ctx-a",
            ),
            sig("cdp_click", json!({"uid": "d2"}), "ctx-a"),
            sig(
                "cdp_fill",
                json!({"uid": "d1", "value": "synthetic"}),
                "ctx-b",
            ),
            sig("cdp_click", json!({"uid": "d2"}), "ctx-b"),
        ]);

        assert_eq!(detect_repeated_action_cycle(&recent), None);
    }

    #[test]
    fn stable_context_falls_back_to_page_fingerprint_without_elements() {
        let mut wm = WorldModel::default();
        wm.cdp_page = Some(Fresh {
            value: CdpPageState {
                url: "app://synthetic/page".to_string(),
                page_fingerprint: "count=1;hash=a".to_string(),
                element_inventory: Vec::new(),
            },
            written_at: 1,
            source: FreshnessSource::DirectObservation,
            ttl_steps: Some(2),
        });
        let before = stable_no_progress_context_signature(&wm);

        wm.cdp_page.as_mut().unwrap().value.page_fingerprint = "count=2;hash=b".to_string();
        let after = stable_no_progress_context_signature(&wm);

        assert_ne!(
            before, after,
            "CDP element-surface progress must reset no-progress tracking"
        );
    }

    fn cdp(uid: &str, role: &str, label: &str, tag: &str) -> ObservedElement {
        ObservedElement::Cdp(CdpFindElementMatch {
            uid: uid.to_string(),
            role: role.to_string(),
            label: label.to_string(),
            tag: tag.to_string(),
            disabled: false,
            parent_role: None,
            parent_name: None,
            ..Default::default()
        })
    }

    fn wm_with_cdp_elements(page_fingerprint: &str, elements: Vec<ObservedElement>) -> WorldModel {
        let mut wm = WorldModel::default();
        wm.cdp_page = Some(Fresh {
            value: CdpPageState {
                url: "app://synthetic/page".to_string(),
                page_fingerprint: page_fingerprint.to_string(),
                element_inventory: Vec::new(),
            },
            written_at: 1,
            source: FreshnessSource::DirectObservation,
            ttl_steps: Some(2),
        });
        wm.elements = Some(Fresh {
            value: elements,
            written_at: 1,
            source: FreshnessSource::DirectObservation,
            ttl_steps: Some(2),
        });
        wm
    }

    #[test]
    fn stable_context_ignores_cdp_order_and_uid_churn_when_elements_exist() {
        let before = wm_with_cdp_elements(
            "count=2;hash=uid-a",
            vec![
                cdp("d1", "textbox", "Search synthetic channels", "input"),
                cdp("d2", "button", "Cancel search", "button"),
            ],
        );
        let after = wm_with_cdp_elements(
            "count=2;hash=uid-b",
            vec![
                cdp("d9", "button", "Cancel search", "button"),
                cdp("d8", "textbox", "Search synthetic channels", "input"),
            ],
        );

        assert_eq!(
            stable_no_progress_context_signature(&before),
            stable_no_progress_context_signature(&after),
            "element order, uid churn, and derived page-fingerprint churn must not look like progress"
        );
    }

    #[test]
    fn stable_context_changes_when_semantic_element_surface_changes() {
        let before = wm_with_cdp_elements(
            "count=1;hash=a",
            vec![cdp("d1", "button", "Open synthetic item", "button")],
        );
        let after = wm_with_cdp_elements(
            "count=1;hash=b",
            vec![cdp("d1", "button", "Synthetic item open", "button")],
        );

        assert_ne!(
            stable_no_progress_context_signature(&before),
            stable_no_progress_context_signature(&after),
            "semantic element changes must still reset no-progress tracking"
        );
    }

    #[test]
    fn stable_context_changes_when_cdp_visible_text_changes() {
        let mut before_el = cdp("d1", "button", "Chat with Ljuba Isakovic", "button");
        if let ObservedElement::Cdp(el) = &mut before_el {
            el.visible_text = "Note to Self Tue Photo".to_string();
        }
        let mut after_el = before_el.clone();
        if let ObservedElement::Cdp(el) = &mut after_el {
            el.visible_text = "Note to Self Wed New message".to_string();
        }

        let before = wm_with_cdp_elements("count=1;hash=a", vec![before_el]);
        let after = wm_with_cdp_elements("count=1;hash=b", vec![after_el]);

        assert_ne!(
            stable_no_progress_context_signature(&before),
            stable_no_progress_context_signature(&after),
            "visible text changes must reset no-progress tracking even when the accessibility label is unchanged"
        );
    }

    #[test]
    fn stable_context_ignores_ocr_confidence_jitter() {
        let mut before = WorldModel::default();
        before.elements = Some(Fresh {
            value: vec![ObservedElement::Ocr(OcrMatch {
                text: "Synthetic status".to_string(),
                x: 101,
                y: 202,
                width: 98,
                height: 19,
                confidence: 0.91,
            })],
            written_at: 1,
            source: FreshnessSource::DirectObservation,
            ttl_steps: Some(2),
        });
        let mut after = before.clone();
        if let Some(elements) = after.elements.as_mut()
            && let Some(ObservedElement::Ocr(match_)) = elements.value.first_mut()
        {
            match_.x = 104;
            match_.y = 206;
            match_.confidence = 0.73;
        }

        assert_eq!(
            stable_no_progress_context_signature(&before),
            stable_no_progress_context_signature(&after),
            "small OCR coordinate jitter and confidence changes must not reset the guard"
        );
    }

    #[test]
    fn stale_cdp_uid_errors_are_recognized_and_wrapped() {
        assert!(is_stale_cdp_uid_error(
            "cdp_fill",
            "No node with given id found"
        ));
        assert!(!is_stale_cdp_uid_error(
            "ax_click",
            "No node with given id found"
        ));

        let nudge = build_stale_cdp_uid_nudge("No node with given id found");
        assert!(nudge.starts_with(STALE_CDP_UID_PREFIX));
        assert!(nudge.contains("Rediscover the target"));
        assert!(!nudge.contains("cdp_evaluate_script"));
    }

    #[test]
    fn recovery_nudges_do_not_recommend_eval_script_for_discovery() {
        let repeated = build_no_progress_nudge("cdp_click", 2, "clicked");
        let cycle = build_action_cycle_nudge("cdp_find_elements -> cdp_click", "clicked");
        let post_text = build_post_text_submit_nudge(3, r#"{"matches":[]}"#);

        assert!(repeated.contains("cdp_find_elements"));
        assert!(cycle.contains("cdp_get_element_context"));
        assert!(post_text.contains("cdp_press_key"));
        assert!(!repeated.contains("cdp_evaluate_script"));
        assert!(!cycle.contains("cdp_evaluate_script"));
        assert!(!post_text.contains("cdp_evaluate_script"));
    }

    #[test]
    fn post_text_send_search_helpers_detect_empty_send_searches() {
        assert!(is_send_submit_cdp_search(
            &serde_json::json!({"query":"Send", "role":"button"})
        ));
        assert!(is_send_submit_cdp_search(
            &serde_json::json!({"query":"send button"})
        ));
        assert!(is_send_submit_cdp_search(
            &serde_json::json!({"query":"Submit"})
        ));
        assert!(!is_send_submit_cdp_search(
            &serde_json::json!({"query":"Message", "role":"textbox"})
        ));

        assert_eq!(
            cdp_find_elements_has_matches(r#"{"matches":[],"inventory":[]}"#),
            Some(false)
        );
        assert_eq!(
            cdp_find_elements_has_matches(
                r#"{"matches":[{"uid":"d1","role":"button","label":"Send"}]}"#
            ),
            Some(true)
        );
    }
}

#[cfg(test)]
mod invalidation_wiring_tests {
    //! Direct tests for `queue_invalidations_for_tool_success` and
    //! `queue_snapshot_stale_if_aged` — both fire pending events that
    //! `observe()` drains.

    use super::*;
    use crate::agent::world_model::{
        AxSnapshotData, Fresh, FreshnessSource, InvalidationEvent, ScreenshotRef, SnapshotKind,
    };
    use serde_json::json;

    fn runner() -> StateRunner {
        StateRunner::new_for_test("test goal".to_string())
    }

    #[test]
    fn focus_window_queues_focus_changing() {
        let mut r = runner();
        r.queue_invalidations_for_tool_success("focus_window", &json!({"app_name": "Safari"}));
        assert!(matches!(
            r.pending_events.as_slice(),
            [InvalidationEvent::FocusChanging { tool }] if tool == "focus_window"
        ));
    }

    #[test]
    fn launch_app_queues_focus_and_lifecycle() {
        let mut r = runner();
        r.queue_invalidations_for_tool_success("launch_app", &json!({"app_name": "Mail"}));
        assert_eq!(r.pending_events.len(), 2);
        assert!(matches!(
            r.pending_events[0],
            InvalidationEvent::FocusChanging { .. }
        ));
        assert!(matches!(
            r.pending_events[1],
            InvalidationEvent::AppLifecycle { .. }
        ));
    }

    #[test]
    fn quit_app_queues_focus_and_lifecycle() {
        let mut r = runner();
        r.queue_invalidations_for_tool_success("quit_app", &json!({"app_name": "Mail"}));
        assert_eq!(r.pending_events.len(), 2);
    }

    #[test]
    fn cdp_navigate_queues_navigation_with_url() {
        let mut r = runner();
        r.queue_invalidations_for_tool_success(
            "cdp_navigate",
            &json!({"url": "https://example.com/login"}),
        );
        match r.pending_events.as_slice() {
            [InvalidationEvent::CdpNavigation { new_url }] => {
                assert_eq!(new_url, "https://example.com/login");
            }
            _ => panic!("expected CdpNavigation event"),
        }
    }

    #[test]
    fn cdp_select_page_queues_navigation_even_without_url() {
        let mut r = runner();
        r.queue_invalidations_for_tool_success("cdp_select_page", &json!({"page_index": 1}));
        assert!(matches!(
            r.pending_events.as_slice(),
            [InvalidationEvent::CdpNavigation { new_url }] if new_url.is_empty()
        ));
    }

    #[test]
    fn unrelated_tool_queues_nothing() {
        let mut r = runner();
        r.queue_invalidations_for_tool_success("cdp_click", &json!({"uid": "d1"}));
        assert!(r.pending_events.is_empty());
    }

    #[test]
    fn snapshot_stale_fires_only_for_aged_ax_field() {
        let mut r = runner();
        r.world_model.last_native_ax_snapshot = Some(Fresh {
            value: AxSnapshotData {
                snapshot_id: "ax-0".into(),
                element_count: 0,
                captured_at_step: 0,
                ax_tree_text: String::new(),
            },
            written_at: 0,
            source: FreshnessSource::DirectObservation,
            ttl_steps: Some(2),
        });
        r.step_index = 5; // age = 5, TTL = 2 → should fire.
        r.queue_snapshot_stale_if_aged();
        assert!(matches!(
            r.pending_events.as_slice(),
            [InvalidationEvent::SnapshotStale {
                kind: SnapshotKind::NativeAx,
                age_steps: 5,
            }]
        ));
    }

    #[test]
    fn snapshot_stale_no_op_when_within_ttl() {
        let mut r = runner();
        r.world_model.last_screenshot = Some(Fresh {
            value: ScreenshotRef {
                screenshot_id: "ss-0".into(),
                captured_at_step: 0,
            },
            written_at: 3,
            source: FreshnessSource::DirectObservation,
            ttl_steps: Some(8),
        });
        r.step_index = 5; // age = 2, TTL = 8 → no event.
        r.queue_snapshot_stale_if_aged();
        assert!(r.pending_events.is_empty());
    }

    #[test]
    fn stale_ax_does_not_invalidate_fresh_screenshot() {
        // The bug being prevented: AX captured at step 0 (TTL 2) and
        // a screenshot captured at step 4 (TTL 4). At step 5, AX is
        // stale (age 5 > TTL 2) but the screenshot is fresh
        // (age 1 < TTL 4). A single `SnapshotStale { age_steps = 5 }`
        // event would have dragged the screenshot down too; the new
        // shape queues per-kind so apply only clears AX.
        let mut r = runner();
        r.world_model.last_native_ax_snapshot = Some(Fresh {
            value: AxSnapshotData {
                snapshot_id: "ax-0".into(),
                element_count: 0,
                captured_at_step: 0,
                ax_tree_text: String::new(),
            },
            written_at: 0,
            source: FreshnessSource::DirectObservation,
            ttl_steps: Some(2),
        });
        r.world_model.last_screenshot = Some(Fresh {
            value: ScreenshotRef {
                screenshot_id: "ss-1".into(),
                captured_at_step: 4,
            },
            written_at: 4,
            source: FreshnessSource::DirectObservation,
            ttl_steps: Some(4),
        });
        r.step_index = 5;
        r.queue_snapshot_stale_if_aged();
        let queued = std::mem::take(&mut r.pending_events);
        r.world_model.apply_events(queued);
        assert!(
            r.world_model.last_native_ax_snapshot.is_none(),
            "stale AX must be cleared"
        );
        assert!(
            r.world_model.last_screenshot.is_some(),
            "fresh screenshot must survive AX going stale"
        );
    }
}

#[cfg(test)]
mod source_agnostic_elements_tests {
    //! `update_continuity_after_tool_success` mirrors AX and OCR
    //! results into the source-agnostic `world_model.elements` field
    //! so the renderer can print them uniformly.

    use super::*;
    use crate::agent::world_model::ObservedElement;

    fn runner() -> StateRunner {
        StateRunner::new_for_test("test goal".to_string())
    }

    #[test]
    fn take_ax_snapshot_populates_elements_with_ax_variants() {
        let mut r = runner();
        let body = "uid=a1g3 button \"Login\"\n  uid=a2g3 textbox \"Email\"\n";
        r.update_continuity_after_tool_success("take_ax_snapshot", body);
        let els = r.world_model.elements.as_ref().expect("elements populated");
        assert!(!els.value.is_empty(), "expected parsed AX elements");
        assert!(
            els.value
                .iter()
                .all(|e| matches!(e, ObservedElement::Ax(_))),
            "all elements must be Ax-variant"
        );
    }

    #[test]
    fn take_ax_snapshot_with_empty_body_does_not_overwrite_elements() {
        let mut r = runner();
        // Pre-populate a CDP elements surface; an empty AX snapshot
        // should not clobber it (no `Ax` elements parsed).
        let cdp_match = clickweave_core::cdp::CdpFindElementMatch {
            uid: "d1".into(),
            role: "button".into(),
            label: "OK".into(),
            tag: "button".into(),
            disabled: false,
            parent_role: None,
            parent_name: None,
            ..Default::default()
        };
        r.world_model.elements = Some(crate::agent::world_model::Fresh {
            value: vec![ObservedElement::Cdp(cdp_match.clone())],
            written_at: 0,
            source: crate::agent::world_model::FreshnessSource::DirectObservation,
            ttl_steps: Some(2),
        });
        r.update_continuity_after_tool_success("take_ax_snapshot", "");
        let els = r.world_model.elements.as_ref().unwrap();
        assert!(matches!(els.value.first(), Some(ObservedElement::Cdp(_))));
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

        // 4. `new_for_test` opts allow_focus_window back in so the
        //    unit tests in this module exercise the kind/toolset
        //    branches without per-test opt-in; an unseeded fixture
        //    runner must defer on unknown kind.
        let test_default_runner = StateRunner::new_for_test("test-goal".to_string());
        assert!(
            test_default_runner
                .should_skip_focus_window(&args_named, &mcp_empty)
                .is_none(),
        );
    }

    #[test]
    fn default_config_disables_focus_window_via_policy() {
        // Pins the production-default contract: `AgentConfig::default()`
        // must suppress every focus_window unconditionally. `new_for_test`
        // overrides this for the rest of the suite (see above).
        let runner = StateRunner::new("test-goal".to_string(), AgentConfig::default());
        let mcp = ToolsetStub::with(&[]);
        let args = serde_json::json!({"app_name": "AnyApp"});
        assert_eq!(
            runner.should_skip_focus_window(&args, &mcp),
            Some(FocusSkipReason::PolicyDisabled),
            "AgentConfig::default() must suppress focus_window unconditionally",
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

    #[test]
    fn should_skip_focus_window_fires_cdp_attachable_for_electron_pre_connect() {
        // Pre-CDP-connect contract: kind is Electron / Chrome and the
        // server advertises `cdp_connect`. The post-tool hook will
        // auto-connect on its own — the real focus_window is
        // unnecessary and would only steal foreground in the meantime.
        for kind in ["ElectronApp", "ChromeBrowser"] {
            let runner = runner_with_kind("VSCode", kind);
            let mcp = ToolsetStub::with(&["cdp_connect"]);
            let args = serde_json::json!({"app_name": "VSCode"});
            assert_eq!(
                runner.should_skip_focus_window(&args, &mcp),
                Some(FocusSkipReason::CdpAttachable),
                "kind={kind} with cdp_connect advertised must trigger CdpAttachable",
            );
        }
    }

    #[test]
    fn should_skip_focus_window_defers_for_electron_when_cdp_connect_missing() {
        // CDP-attachable arm requires the server to actually advertise
        // `cdp_connect`. Without it the post-tool hook cannot fire, so
        // the first focus_window may itself be needed to bring the
        // window front and the classifier must defer.
        let runner = runner_with_kind("VSCode", "ElectronApp");
        // FULL_CDP_TOOLSET does NOT include cdp_connect by design —
        // it is the dispatch toolset, not the lifecycle one.
        let mcp = ToolsetStub::with(FULL_CDP_TOOLSET);
        let args = serde_json::json!({"app_name": "VSCode"});
        assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
    }

    #[test]
    fn cdp_live_takes_precedence_over_cdp_attachable_for_same_app() {
        // When the session is live AND the server advertises
        // `cdp_connect`, the more specific `CdpLive` arm must fire —
        // the agent has the dispatch toolset, not just the connect
        // primitive. Order matters in the match: CdpLive first.
        let runner = runner_with_kind_and_cdp("Signal", "ElectronApp");
        // Both CDP dispatch AND cdp_connect advertised.
        let mcp = ToolsetStub::with(&["cdp_find_elements", "cdp_click", "cdp_connect"]);
        let args = serde_json::json!({"app_name": "Signal"});
        assert_eq!(
            runner.should_skip_focus_window(&args, &mcp),
            Some(FocusSkipReason::CdpLive),
        );
    }
}

/// Coordinate-primitive guard: defense-in-depth check that a wrong-family
/// dispatch (`click` / `type_text` / `press_key` / `move_mouse` / `scroll`
/// / `drag`) is rejected at the harness layer when a structured surface
/// (`cdp_page` for CDP-backed apps, `take_ax_snapshot` + AX dispatch for
/// Native) is wired for the focused app. Sits behind the per-turn
/// `<tools_in_scope>` filter — these tests pin the predicate alone; the
/// dispatch-site behaviour (synthetic StepOutcome::Error, StepFailed
/// event, recovery_strategy interaction) is covered by the integration
/// suite.
#[cfg(test)]
mod coordinate_primitive_guard_tests {
    use super::*;
    use crate::agent::world_model::{AppKind, CdpPageState, FocusedApp, Fresh, FreshnessSource};
    use clickweave_mcp::ToolCallResult;

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
            unimplemented!("coordinate guard predicate does not dispatch tools")
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

    const AX_TOOLSET: &[&str] = &["take_ax_snapshot", "ax_click", "ax_set_value", "ax_select"];

    fn focused(name: &str, kind: AppKind) -> Fresh<FocusedApp> {
        Fresh {
            value: FocusedApp {
                name: name.to_string(),
                kind,
                pid: 1,
            },
            written_at: 0,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        }
    }

    fn cdp_page(url: &str) -> Fresh<CdpPageState> {
        Fresh {
            value: CdpPageState {
                url: url.to_string(),
                page_fingerprint: "fp".to_string(),
                element_inventory: Vec::new(),
            },
            written_at: 0,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        }
    }

    #[test]
    fn blocks_click_when_cdp_page_live_and_focus_is_electron() {
        let mut runner = StateRunner::new_for_test("g".to_string());
        runner.world_model.focused_app = Some(focused("Signal", AppKind::ElectronApp));
        runner.world_model.cdp_page = Some(cdp_page("https://signal/"));
        let mcp = ToolsetStub::with(&[]);
        let blocked = runner.coordinate_primitive_blocked("click", &mcp);
        assert!(blocked.is_some(), "click must be blocked under live CDP");
        let msg = blocked.unwrap();
        assert!(msg.contains("cdp_page"));
        assert!(msg.contains("cdp_click"));
        assert!(!msg.contains("cdp_evaluate_script"));
    }

    #[test]
    fn blocks_each_coordinate_primitive_under_cdp() {
        let mut runner = StateRunner::new_for_test("g".to_string());
        runner.world_model.focused_app = Some(focused("Signal", AppKind::ElectronApp));
        runner.world_model.cdp_page = Some(cdp_page("https://signal/"));
        let mcp = ToolsetStub::with(&[]);
        for tool in [
            "click",
            "type_text",
            "press_key",
            "move_mouse",
            "scroll",
            "drag",
        ] {
            assert!(
                runner.coordinate_primitive_blocked(tool, &mcp).is_some(),
                "{tool} must be blocked when CDP is wired",
            );
        }
    }

    #[test]
    fn does_not_block_observation_or_structured_tools_under_cdp() {
        let mut runner = StateRunner::new_for_test("g".to_string());
        runner.world_model.focused_app = Some(focused("Signal", AppKind::ElectronApp));
        runner.world_model.cdp_page = Some(cdp_page("https://signal/"));
        let mcp = ToolsetStub::with(&[]);
        for tool in [
            "find_text",
            "find_image",
            "element_at_point",
            "cdp_click",
            "ax_click",
            "take_screenshot",
        ] {
            assert!(
                runner.coordinate_primitive_blocked(tool, &mcp).is_none(),
                "{tool} must NOT be blocked — only coordinate primitives are",
            );
        }
    }

    #[test]
    fn blocks_click_when_focus_is_native_and_ax_dispatch_wired() {
        let mut runner = StateRunner::new_for_test("g".to_string());
        runner.world_model.focused_app = Some(focused("Calculator", AppKind::Native));
        let mcp = ToolsetStub::with(AX_TOOLSET);
        let blocked = runner.coordinate_primitive_blocked("click", &mcp);
        assert!(blocked.is_some(), "click must be blocked under AX dispatch");
        let msg = blocked.unwrap();
        assert!(msg.contains("Native"));
        assert!(msg.contains("ax_click"));
    }

    #[test]
    fn defers_when_focus_is_native_but_ax_toolset_partial() {
        let mut runner = StateRunner::new_for_test("g".to_string());
        runner.world_model.focused_app = Some(focused("Calculator", AppKind::Native));
        // Missing ax_set_value — partial toolset means agent cannot
        // drive via AX, so coordinate primitives remain a valid path.
        let mcp = ToolsetStub::with(&["take_ax_snapshot", "ax_click"]);
        assert!(runner.coordinate_primitive_blocked("click", &mcp).is_none());
    }

    #[test]
    fn defers_when_no_focused_app() {
        let runner = StateRunner::new_for_test("g".to_string());
        // No focused_app set — caller has not yet observed which surface
        // is wired, so we cannot tell which family the agent should be
        // using and must fall through.
        let mcp = ToolsetStub::with(AX_TOOLSET);
        assert!(runner.coordinate_primitive_blocked("click", &mcp).is_none());
    }

    #[test]
    fn defers_for_electron_focus_without_cdp_page() {
        // Electron is focused but no cdp_page yet (auto-connect hasn't
        // attached). Coordinate primitives are not yet redundant — the
        // agent may need them to bring the window front. Guard defers.
        let mut runner = StateRunner::new_for_test("g".to_string());
        runner.world_model.focused_app = Some(focused("Signal", AppKind::ElectronApp));
        let mcp = ToolsetStub::with(&["cdp_connect"]);
        assert!(runner.coordinate_primitive_blocked("click", &mcp).is_none());
    }

    #[test]
    fn is_coordinate_primitive_includes_actions_excludes_observations() {
        for name in [
            "click",
            "type_text",
            "press_key",
            "move_mouse",
            "scroll",
            "drag",
        ] {
            assert!(is_coordinate_primitive(name), "{name} is a coord primitive");
        }
        for name in [
            "find_text",
            "find_image",
            "element_at_point",
            "take_screenshot",
            "ax_click",
            "cdp_click",
            "launch_app",
        ] {
            assert!(
                !is_coordinate_primitive(name),
                "{name} must NOT be classified as a coordinate primitive",
            );
        }
    }
}

/// CDP auto-connect status field (`world_model.cdp_connect_status`).
/// The runner sets this whenever `auto_connect_cdp` exhausts retries
/// and clears it on success or focus change. Without the field, the
/// LLM cannot tell "auto-connect hasn't fired yet" (no cdp_page, no
/// status) from "auto-connect tried and failed permanently" (no
/// cdp_page, status present).
#[cfg(test)]
mod cdp_connect_status_tests {
    use super::*;

    #[test]
    fn record_cdp_connect_failure_writes_fresh_status() {
        let mut runner = StateRunner::new_for_test("g".to_string());
        assert!(runner.world_model.cdp_connect_status.is_none());
        runner.record_cdp_connect_failure("probe_app failed for X: y".to_string());
        let status = runner
            .world_model
            .cdp_connect_status
            .as_ref()
            .expect("status set");
        assert_eq!(status.value, "probe_app failed for X: y");
        assert_eq!(status.written_at, runner.step_index);
    }

    #[test]
    fn second_failure_overwrites_first() {
        let mut runner = StateRunner::new_for_test("g".to_string());
        runner.record_cdp_connect_failure("first".to_string());
        runner.record_cdp_connect_failure("second".to_string());
        assert_eq!(
            runner
                .world_model
                .cdp_connect_status
                .as_ref()
                .unwrap()
                .value,
            "second",
        );
    }
}

/// D24/D29 run-start retrieval gate + step_index ownership tests.
/// The gate (`episodic_run_start_retrieved`) replaces the drift-prone
/// `step_index == 0` proxy; the helper (`advance_recorded_step_index`)
/// is the single owner of `step_index` updates so the counter matches
/// `state.steps.len()` across all recording paths (synthetic skip,
/// policy deny, approval reject, normal LLM turn).
#[cfg(test)]
mod retrieval_gate_tests {
    use super::*;
    use crate::agent::episodic::{EpisodeScope, EpisodicContext, SqliteEpisodicStore};
    use crate::agent::phase::Phase;
    use tempfile::TempDir;

    fn enabled_runner_with_store() -> (StateRunner, TempDir) {
        let dir = TempDir::new().unwrap();
        let wl_path = dir.path().join("episodic.sqlite");
        let ctx = EpisodicContext {
            enabled: true,
            workflow_local_path: wl_path.clone(),
            global_path: None,
            workflow_hash: "gate-test-workflow".into(),
        };
        let runner =
            StateRunner::new_with_episodic("goal".to_string(), AgentConfig::default(), ctx);
        // Sanity: store opened.
        assert!(
            runner.episodic_store.is_some(),
            "test setup expects an episodic store",
        );
        // The `wl_path` is referenced indirectly through the runner's
        // store; pre-open one to confirm SQLite WAL mode took.
        let _verify = SqliteEpisodicStore::new(&wl_path, EpisodeScope::WorkflowLocal).unwrap();
        (runner, dir)
    }

    #[tokio::test]
    async fn run_start_retrieval_consumes_gate_on_first_call() {
        let (mut r, _dir) = enabled_runner_with_store();
        assert!(!r.episodic_run_start_retrieved);

        // First call: run-start trigger fires (zero hits, but the
        // gate-consumed semantic still applies).
        let hits = r.try_retrieve_episodic(Phase::Exploring).await;
        assert!(
            hits.is_empty(),
            "fresh store has no episodes yet — retrieval should be empty",
        );
        assert!(
            r.episodic_run_start_retrieved,
            "first call must mark the run-start slot consumed regardless of hit count",
        );

        // Second call with no Recovering transition: must skip
        // entirely. Previously `step_index == 0` would have re-fired
        // RunStart on policy-deny early-continue paths.
        // Force `step_index` back to 0 to prove the gate (not the
        // counter) is what blocks re-fire.
        r.step_index = 0;
        let hits2 = r.try_retrieve_episodic(Phase::Exploring).await;
        assert!(
            hits2.is_empty(),
            "second call without Recovering transition must be a no-op",
        );
    }

    #[tokio::test]
    async fn recovering_entry_still_fires_after_run_start_consumed() {
        let (mut r, _dir) = enabled_runner_with_store();

        // Consume the run-start slot.
        let _ = r.try_retrieve_episodic(Phase::Exploring).await;
        assert!(r.episodic_run_start_retrieved);

        // Transition into Recovering. Retrieval should fire on the
        // edge (returns empty here because no episodes exist yet, but
        // the call should still execute the trigger branch — verified
        // by the side effect of capturing a `recovering_snapshot`).
        r.task_state.phase = Phase::Recovering;
        let _ = r.try_retrieve_episodic(Phase::Exploring).await;
        assert!(
            r.recovering_snapshot.is_some(),
            "Recovering entry must capture a snapshot for the eventual write",
        );
    }

    #[tokio::test]
    async fn advance_recorded_step_index_increments_counter() {
        let mut r = StateRunner::new_for_test("g".to_string());
        assert_eq!(r.step_index, 0);
        r.advance_recorded_step_index();
        assert_eq!(r.step_index, 1);
        r.advance_recorded_step_index();
        assert_eq!(r.step_index, 2);
    }

    #[tokio::test]
    async fn record_policy_deny_failure_sets_stable_kind() {
        // Policy-deny branches funnel through this helper, and the snapshot derived from
        // `last_failed_*` populates `FailureSignature` on the
        // eventual write. The `error_kind` must be the stable
        // snake_case `policy_denied`, not a free-form string.
        let mut r = StateRunner::new_for_test("g".to_string());
        assert!(r.last_failed_tool_name.is_none());
        assert!(r.last_failed_error_kind.is_none());

        r.record_policy_deny_failure("cdp_click");
        assert_eq!(r.last_failed_tool_name.as_deref(), Some("cdp_click"));
        assert_eq!(
            r.last_failed_error_kind.as_deref(),
            Some("policy_denied"),
            "policy-deny error_kind must be the stable snake_case string used by both branches",
        );
    }

    #[tokio::test]
    async fn clear_last_failure_tracking_drops_both_fields() {
        let mut r = StateRunner::new_for_test("g".to_string());
        r.record_policy_deny_failure("ax_click");
        r.clear_last_failure_tracking();
        assert!(
            r.last_failed_tool_name.is_none(),
            "tool_name must be cleared after success",
        );
        assert!(
            r.last_failed_error_kind.is_none(),
            "error_kind must be cleared after success",
        );
    }

    #[tokio::test]
    async fn run_turn_no_longer_advances_step_index_directly() {
        // Under the new ownership rule, `run_turn` does not bump the
        // counter — that's the helper's job, called by sites that push
        // an `AgentStep`. `agent_done` is terminal with no step push,
        // so `step_index` must stay 0 after the turn.
        use async_trait::async_trait;
        use std::sync::Mutex;

        struct EmptyExec(Mutex<Vec<Result<String, String>>>);
        #[async_trait]
        impl ToolExecutor for EmptyExec {
            async fn call_tool(&self, _: &str, _: &serde_json::Value) -> Result<String, String> {
                let mut q = self.0.lock().unwrap();
                q.pop().unwrap_or_else(|| Err("no result".into()))
            }
        }

        let mut r = StateRunner::new_for_test("g".to_string());
        let exec = EmptyExec(Mutex::new(vec![]));
        let done = AgentTurn {
            mutations: vec![],
            action: AgentAction::AgentDone {
                summary: "done".into(),
            },
        };
        let _ = r.run_turn(&done, &exec).await;
        assert_eq!(
            r.step_index, 0,
            "run_turn must not advance step_index — only `advance_recorded_step_index` does",
        );
    }
}

#[cfg(test)]
mod skills_apply_mutations_tests {
    //! Spec 3 Phase 3 unit tests for the runner-side scratch fields
    //! populated by `apply_mutations`.

    use super::*;
    use crate::agent::world_model::{AppKind, FocusedApp, Fresh, FreshnessSource};

    fn focused_app(name: &str) -> Fresh<FocusedApp> {
        Fresh {
            value: FocusedApp {
                name: name.to_string(),
                kind: AppKind::Native,
                pid: 1,
            },
            written_at: 0,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        }
    }

    #[test]
    fn push_subgoal_records_id_and_push_idx() {
        let mut r = StateRunner::new_for_test("g".to_string());
        r.apply_mutations(&[TaskStateMutation::PushSubgoal {
            text: "open chat".into(),
        }]);
        assert_eq!(r.last_pushed_subgoal_ids.len(), 1);
        assert_eq!(r.push_idx_stack.len(), 1);
        assert_eq!(r.push_idx_stack[0], 0); // recorded_steps was empty
        assert_eq!(r.push_signature_stack.len(), 1);
        assert_eq!(r.produced_node_ids_stack.len(), 1);
        assert!(r.produced_node_ids_stack[0].is_empty());
    }

    #[test]
    fn complete_subgoal_drains_push_idx_into_extraction_queue() {
        let mut r = StateRunner::new_for_test("g".to_string());
        r.apply_mutations(&[TaskStateMutation::PushSubgoal {
            text: "open chat".into(),
        }]);
        r.apply_mutations(&[TaskStateMutation::CompleteSubgoal {
            summary: "done".into(),
        }]);
        assert!(r.push_idx_stack.is_empty(), "push_idx popped on complete");
        assert!(
            r.push_signature_stack.is_empty(),
            "push signature popped on complete"
        );
        assert_eq!(
            r.completed_subgoal_extraction_queue.len(),
            1,
            "extraction queue carries the completed milestone",
        );
    }

    #[test]
    fn complete_subgoal_carries_push_time_signature() {
        let mut r = StateRunner::new_for_test("g".to_string());
        r.world_model.focused_app = Some(focused_app("Finder"));
        let push_sig = crate::agent::skills::signature::compute_subgoal_signature(
            "open inbox",
            &r.world_model,
        );

        r.apply_mutations(&[TaskStateMutation::PushSubgoal {
            text: "open inbox".into(),
        }]);
        r.world_model.focused_app = Some(focused_app("Mail"));
        let completion_sig = crate::agent::skills::signature::compute_subgoal_signature(
            "open inbox",
            &r.world_model,
        );
        r.apply_mutations(&[TaskStateMutation::CompleteSubgoal {
            summary: "done".into(),
        }]);

        let (_, _, queued_sig, _) = r
            .completed_subgoal_extraction_queue
            .first()
            .expect("queued extraction");
        assert_eq!(queued_sig, &push_sig);
        assert_ne!(queued_sig, &completion_sig);
    }

    #[test]
    fn last_pushed_subgoal_ids_cleared_each_batch() {
        let mut r = StateRunner::new_for_test("g".to_string());
        r.apply_mutations(&[TaskStateMutation::PushSubgoal {
            text: "first".into(),
        }]);
        r.apply_mutations(&[]); // empty batch — should still clear
        assert!(r.last_pushed_subgoal_ids.is_empty());
    }

    #[test]
    fn nested_subgoals_queue_produced_nodes_per_frame() {
        let mut r = StateRunner::new_for_test("g".to_string());
        let outer_node = uuid::Uuid::new_v4();
        let inner_node = uuid::Uuid::new_v4();
        let after_inner_node = uuid::Uuid::new_v4();

        r.apply_mutations(&[TaskStateMutation::PushSubgoal {
            text: "outer".into(),
        }]);
        r.record_produced_node_id(outer_node);

        r.apply_mutations(&[TaskStateMutation::PushSubgoal {
            text: "inner".into(),
        }]);
        r.record_produced_node_id(inner_node);

        r.apply_mutations(&[TaskStateMutation::CompleteSubgoal {
            summary: "inner done".into(),
        }]);
        r.record_produced_node_id(after_inner_node);

        r.apply_mutations(&[TaskStateMutation::CompleteSubgoal {
            summary: "outer done".into(),
        }]);

        assert!(r.produced_node_ids_stack.is_empty());
        assert_eq!(r.completed_subgoal_extraction_queue.len(), 2);
        assert_eq!(
            r.completed_subgoal_extraction_queue[0].3,
            vec![inner_node],
            "inner frame only records nodes produced after the inner push",
        );
        assert_eq!(
            r.completed_subgoal_extraction_queue[1].3,
            vec![outer_node, inner_node, after_inner_node],
            "outer frame records every node produced while it was active",
        );
    }

    #[test]
    fn complete_with_empty_stack_records_warning_not_panic() {
        let mut r = StateRunner::new_for_test("g".to_string());
        let warnings = r.apply_mutations(&[TaskStateMutation::CompleteSubgoal {
            summary: "done".into(),
        }]);
        assert!(!warnings.is_empty());
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

    pub(crate) async fn call_finalize_cdp_connected<M: Mcp + ?Sized>(
        runner: &StateRunner,
        app_name: &str,
        cdp_port: u16,
        mcp: &M,
    ) {
        runner.finalize_cdp_connected(app_name, cdp_port, mcp).await;
    }
}

#[cfg(test)]
mod dispatch_skill_tests {
    //! Phase 4 lookup-and-validate coverage for `StateRunner::dispatch_skill`.
    //! The per-step expansion (Task 4.3+) is deferred; these tests pin
    //! the foundation so the resume seam stays stable.

    use super::*;
    use crate::agent::skills::types::{
        ApplicabilityHints, ApplicabilitySignature, ExpectedWorldModelDelta, OutcomePredicate,
        ParameterSlot, ProvenanceEntry, Skill, SkillState, SkillStats, SubgoalSignature,
    };
    use crate::agent::skills::{ActionSketchStep, SkillIndex, SkillScope};
    use chrono::Utc;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    fn make_skill(id: &str, version: u32, state: SkillState, schema: Vec<ParameterSlot>) -> Skill {
        let now = Utc::now();
        Skill {
            id: id.to_string(),
            version,
            state,
            scope: SkillScope::ProjectLocal,
            name: format!("Skill {id}"),
            description: "test skill".to_string(),
            tags: vec![],
            subgoal_text: "open the file".to_string(),
            subgoal_signature: SubgoalSignature("sg".to_string()),
            applicability: ApplicabilityHints {
                apps: vec![],
                hosts: vec![],
                signature: ApplicabilitySignature("app".to_string()),
            },
            parameter_schema: schema,
            action_sketch: vec![ActionSketchStep::ToolCall {
                tool: "noop".to_string(),
                args: serde_json::json!({}),
                captures_pre: vec![],
                captures: vec![],
                expected_world_model_delta: ExpectedWorldModelDelta::default(),
            }],
            outputs: vec![],
            outcome_predicate: OutcomePredicate::SubgoalCompleted {
                post_state_world_model_signature: None,
            },
            provenance: vec![ProvenanceEntry {
                run_id: uuid::Uuid::new_v4().to_string(),
                step_index: 0,
                completed_at: now,
                workflow_hash: "h".to_string(),
            }],
            stats: SkillStats {
                occurrence_count: 1,
                success_rate: 0.5,
                last_seen_at: Some(now),
                last_invoked_at: None,
            },
            edited_by_user: false,
            created_at: now,
            updated_at: now,
            produced_node_ids: vec![],
            body: "# Test\n".to_string(),
        }
    }

    fn tool_step(tool: &str) -> ActionSketchStep {
        ActionSketchStep::ToolCall {
            tool: tool.to_string(),
            args: serde_json::json!({}),
            captures_pre: vec![],
            captures: vec![],
            expected_world_model_delta: ExpectedWorldModelDelta::default(),
        }
    }

    fn slot(name: &str, type_tag: &str, default: Option<serde_json::Value>) -> ParameterSlot {
        ParameterSlot {
            name: name.to_string(),
            type_tag: type_tag.to_string(),
            description: None,
            default,
            enum_values: None,
        }
    }

    fn fresh_runner_with_skill(
        skill: Option<Skill>,
    ) -> (StateRunner, mpsc::Receiver<RunnerOutput>, TempDir) {
        let tmp = TempDir::new().unwrap();
        let mut runner = StateRunner::new_for_test_with_skills(
            "test goal".to_string(),
            tmp.path().to_path_buf(),
        );
        let embedder = Arc::new(crate::agent::episodic::HashedShingleEmbedder::default());
        let mut index = SkillIndex::empty(embedder);
        if let Some(s) = skill {
            index.upsert(s);
        }
        runner.skill_index = Arc::new(parking_lot::RwLock::new(index));
        let (tx, rx) = mpsc::channel(16);
        runner.event_tx = Some(tx);
        (runner, rx, tmp)
    }

    #[test]
    fn single_step_bridge_rejects_multi_step_skill_before_partial_dispatch() {
        let mut skill = make_skill("multi", 1, SkillState::Confirmed, vec![]);
        skill.action_sketch = vec![tool_step("first"), tool_step("second")];
        let frame = SkillFrame::new(Arc::new(skill), serde_json::json!({}));

        match StateRunner::skill_frame_to_single_step_action(&frame) {
            AgentAction::AgentReplan { reason } => {
                assert!(
                    reason.contains("2 replay steps"),
                    "reason should explain unsupported multi-step replay: {reason}"
                );
            }
            other => panic!("expected fail-closed replan, got {:?}", other),
        }
    }

    #[test]
    fn single_step_bridge_dispatches_exactly_one_tool_step() {
        let skill = make_skill("single", 3, SkillState::Confirmed, vec![]);
        let frame = SkillFrame::new(Arc::new(skill), serde_json::json!({}));

        match StateRunner::skill_frame_to_single_step_action(&frame) {
            AgentAction::ToolCall {
                tool_name,
                tool_call_id,
                ..
            } => {
                assert_eq!(tool_name, "noop");
                assert_eq!(tool_call_id, "skill-single-v3-step-0");
            }
            other => panic!("expected single-step tool call, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn unknown_id_yields_replan_naming_the_id() {
        let (mut runner, _rx, _tmp) = fresh_runner_with_skill(None);
        let err = runner
            .dispatch_skill("never_extracted", 1, serde_json::json!({}))
            .await
            .expect_err("missing skill must fail");
        assert!(err.contains("never_extracted"), "reason: {err}");
    }

    #[tokio::test]
    async fn draft_state_is_rejected() {
        let skill = make_skill("draftish", 1, SkillState::Draft, vec![]);
        let (mut runner, _rx, _tmp) = fresh_runner_with_skill(Some(skill));
        let err = runner
            .dispatch_skill("draftish", 1, serde_json::json!({}))
            .await
            .expect_err("draft must not invoke");
        assert!(err.contains("draft"), "reason: {err}");
    }

    #[tokio::test]
    async fn invalid_parameters_yield_replan() {
        let skill = make_skill(
            "needs_count",
            1,
            SkillState::Confirmed,
            vec![slot("count", "integer", None)],
        );
        let (mut runner, _rx, _tmp) = fresh_runner_with_skill(Some(skill));
        let err = runner
            .dispatch_skill("needs_count", 1, serde_json::json!({}))
            .await
            .expect_err("missing required field must fail");
        assert!(err.contains("count"), "reason: {err}");
    }

    #[tokio::test]
    async fn confirmed_emits_invoked_event_and_marks_invoked() {
        let skill = make_skill(
            "confirm_ok",
            2,
            SkillState::Confirmed,
            vec![slot("name", "string", None)],
        );
        let (mut runner, mut rx, _tmp) = fresh_runner_with_skill(Some(skill));
        let frame = runner
            .dispatch_skill("confirm_ok", 2, serde_json::json!({"name": "x"}))
            .await
            .expect("confirmed skill should resolve");
        assert_eq!(frame.skill.id, "confirm_ok");
        assert_eq!(frame.skill.version, 2);
        assert_eq!(frame.next_step, 0);

        let stamped = runner
            .skill_index
            .read()
            .get("confirm_ok", 2)
            .unwrap()
            .stats
            .last_invoked_at;
        assert!(stamped.is_some());

        let event = rx
            .try_recv()
            .expect("SkillInvoked must be emitted")
            .into_event()
            .expect("SkillInvoked must be a durable event");
        match event {
            AgentEvent::SkillInvoked {
                skill_id,
                version,
                parameter_count,
                ..
            } => {
                assert_eq!(skill_id, "confirm_ok");
                assert_eq!(version, 2);
                assert_eq!(parameter_count, 1);
            }
            other => panic!("expected SkillInvoked, got {:?}", other),
        }
    }
}
