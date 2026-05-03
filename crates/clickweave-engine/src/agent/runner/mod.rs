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
use clickweave_core::cdp::CdpFindElementMatch;
use clickweave_llm::{ChatBackend, DynChatBackend, Message};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::agent::context::{CompactBudget, compact};
use crate::agent::permissions::{
    PermissionAction, PermissionPolicy, ToolAnnotations, evaluate as evaluate_permission,
};
use crate::agent::phase::{self, PhaseSignals};
use crate::agent::prompt::{
    UserTurnMessageInput, build_system_prompt, build_system_prompt_with_header,
    build_user_turn_message_from_input,
};
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

mod approval;
mod cdp_lifecycle;
mod focus;
mod loop_control;
mod progress;
mod records;
mod tool_classification;
mod turn;
mod turn_runtime;

pub(crate) use focus::FocusSkipReason;
#[cfg(test)]
pub(crate) use progress::{NO_ACTION_MUTATION_ONLY_PREFIX, STALE_CDP_UID_PREFIX};
pub(crate) use progress::{NO_PROGRESS_WARNING_PREFIX, UNVERIFIED_SIDE_EFFECT_PREFIX};
pub(crate) use tool_classification::{
    diff_world_model_signatures, extract_result_text, is_observation_tool,
};
#[cfg(test)]
pub(crate) use tool_classification::{is_ax_dispatch_tool, is_state_transition_tool};

pub(super) use approval::{ApprovalResult, CapStatus};
pub use turn::{AgentAction, AgentTurn, ToolExecutor, TurnOutcome, parse_agent_turn};
pub(crate) use turn::{McpToolExecutor, append_assistant_and_tool_result};

use focus::{
    AX_DISPATCH_TOOLSET, CDP_DISPATCH_TOOLSET, RunningAppInfo, force_background_launch_app,
    is_coordinate_primitive, launch_app_has_launch_only_args, mcp_has_toolset,
};
use progress::{
    ACTION_CYCLE_WINDOW, ActionProgressSignature, LastActionProgress,
    NO_ACTION_MUTATION_ONLY_REASON, REPEAT_ACTION_THRESHOLD, TEXT_SUBMIT_SEARCH_THRESHOLD,
    TextSubmitSearchProgress, UNVERIFIED_SIDE_EFFECT_COMPLETION_BLOCKED_REASON,
    build_action_cycle_nudge, build_no_progress_nudge, build_post_text_submit_nudge,
    build_stale_cdp_uid_nudge, build_unverified_side_effect_nudge, cdp_find_elements_has_matches,
    combine_with_side_effect_nudge, detect_repeated_action_cycle,
    guard_completion_after_unverified_side_effect, is_send_submit_cdp_search,
    is_stale_cdp_uid_error, is_text_composition_tool, is_unverified_side_effect_action,
    reset_no_progress_tracking, stable_no_progress_context_signature,
};
use tool_classification::{
    APP_LIFECYCLE_TOOLS, CDP_NAVIGATION_TOOLS, FOCUS_CHANGING_TOOLS, OBSERVATION_TOOLS,
    brief_summarize_args, build_annotations_index,
};

#[derive(Debug, Default)]
pub(crate) struct CdpPageObservation {
    pub page_url: String,
    pub page_fingerprint: String,
    pub inventory: Vec<CdpElementInventorySummary>,
}

struct RunLoopContext {
    messages: Vec<Message>,
    tools: Vec<Value>,
    advertised_tool_names: Vec<String>,
    annotations_by_tool: HashMap<String, ToolAnnotations>,
    budget: CompactBudget,
}

#[derive(Default)]
struct RunLoopTrackers {
    previous_result: Option<String>,
    last_failure: Option<(String, Value, String)>,
    last_action: Option<LastActionProgress>,
    recent_actions: VecDeque<ActionProgressSignature>,
    pending_text_submit_search: Option<TextSubmitSearchProgress>,
}

enum LoopStepFlow {
    Continue,
    Break,
    Dispatch,
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
mod tests;
