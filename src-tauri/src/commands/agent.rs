use super::error::CommandError;
use super::types::*;
use clickweave_core::variant_index::{VariantEntry, VariantIndex};
use clickweave_engine::agent::episodic::EpisodicContext;
use clickweave_engine::agent::skills::{SkillContext, SkillScope, SkillState, SkillStore, slugify};
use clickweave_engine::agent::{
    AgentChannels, AgentConfig, AgentEvent, AgentState, ApprovalRequest,
    DisagreementResolutionAction, PermissionAction, PermissionPolicy, PermissionRule, RunnerOutput,
    TerminalReason,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use tokio_util::sync::CancellationToken;

/// Resolve the path to the global episodic SQLite store. Pulls the
/// app-data root from the managed `AppDataDir` state — the same single
/// source `RunStorage::new_app_data` and the trace-retention sweep use,
/// so the global store always lands where D36's privacy sweep walks.
fn app_data_episodic_path(
    app: &tauri::AppHandle,
) -> Result<std::path::PathBuf, super::error::CommandError> {
    let base = app.state::<crate::commands::types::AppDataDir>().0.clone();
    Ok(base.join("episodic.sqlite"))
}

/// Resolve the global procedural-skills directory. Shares the app-data
/// root used by unsaved projects and trace retention.
fn app_data_global_skills_dir(
    app: &tauri::AppHandle,
) -> Result<std::path::PathBuf, super::error::CommandError> {
    let base = app.state::<crate::commands::types::AppDataDir>().0.clone();
    let dir = base.join("skills_global");
    std::fs::create_dir_all(&dir)
        .map_err(|e| super::error::CommandError::io(format!("create global skills dir: {e}")))?;
    Ok(dir)
}

// ── Request / payload types ─────────────────────────────────────

/// Wire form of a single permission rule. Mirrors
/// `clickweave_engine::agent::PermissionRule` but with a `specta::Type`
/// derive so the TypeScript bindings pick it up.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct PermissionRuleWire {
    pub tool_pattern: String,
    pub args_pattern: Option<String>,
    pub action: PermissionActionWire,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, specta::Type, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PermissionActionWire {
    Allow,
    Ask,
    Deny,
}

impl From<PermissionActionWire> for PermissionAction {
    fn from(a: PermissionActionWire) -> Self {
        match a {
            PermissionActionWire::Allow => PermissionAction::Allow,
            PermissionActionWire::Ask => PermissionAction::Ask,
            PermissionActionWire::Deny => PermissionAction::Deny,
        }
    }
}

impl From<PermissionRuleWire> for PermissionRule {
    fn from(r: PermissionRuleWire) -> Self {
        PermissionRule {
            tool_pattern: r.tool_pattern,
            args_pattern: r.args_pattern,
            action: r.action.into(),
        }
    }
}

/// Wire form of the permission policy the UI ships with every `run_agent`.
/// `tools` is the per-tool override map from the existing 2-tier UI (ask
/// / allow). It is mapped into `PermissionRule`s with the tool name as a
/// literal pattern so the Rust side only needs one evaluator.
#[derive(Debug, Clone, Default, Serialize, Deserialize, specta::Type)]
pub struct PermissionPolicyWire {
    #[serde(default)]
    pub rules: Vec<PermissionRuleWire>,
    #[serde(default)]
    pub allow_all: bool,
    #[serde(default)]
    pub require_confirm_destructive: bool,
    /// Per-tool overrides: `{ "click": "allow" }`. Merged into the rule
    /// list as literal-pattern rules before the evaluator runs.
    #[serde(default)]
    pub per_tool: std::collections::HashMap<String, PermissionActionWire>,
}

impl From<PermissionPolicyWire> for PermissionPolicy {
    fn from(p: PermissionPolicyWire) -> Self {
        // Per-tool overrides append after explicit rules so both sources
        // contribute to rule matching. Ordering does not affect the final
        // action because the evaluator combines matches with
        // Deny > Ask > Allow — not "last rule wins".
        let mut rules: Vec<PermissionRule> =
            p.rules.into_iter().map(PermissionRule::from).collect();
        for (name, action) in p.per_tool {
            rules.push(PermissionRule {
                tool_pattern: name,
                args_pattern: None,
                action: action.into(),
            });
        }
        PermissionPolicy {
            rules,
            allow_all: p.allow_all,
            require_confirm_destructive: p.require_confirm_destructive,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct AgentRunRequest {
    pub goal: String,
    pub agent: EndpointConfig,
    pub project_path: Option<String>,
    pub workflow_name: String,
    pub workflow_id: String,
    /// Permission policy for this run. When `None`, the default policy
    /// (empty rules, allow_all=false, guardrail off) is used.
    #[serde(default)]
    pub permissions: Option<PermissionPolicyWire>,
    /// Halt the run after this many consecutive destructive tool calls.
    /// `0` disables the cap. `None` uses the engine default (3).
    #[serde(default)]
    pub consecutive_destructive_cap: Option<usize>,
    /// Permit `focus_window` MCP calls. When `Some(false)` the runner
    /// suppresses every `focus_window` call with a synthetic skip
    /// regardless of app kind or CDP state. When `Some(true)` the
    /// runner permits `focus_window` and the AX/CDP-scoped guards in
    /// `runner.rs` decide per-call whether the focus is redundant.
    /// `None` leaves the engine default (`false`) in place — runs
    /// operate in the background unless explicitly opted in.
    #[serde(default)]
    pub allow_focus_window: Option<bool>,
    /// Privacy kill switch: when false, the run is entirely in-memory.
    /// No `.clickweave/runs/` directory is created and no trace files
    /// or agent metadata files are written. When `None`, persistence is on —
    /// matches the UI default (`storeTraces: true`).
    #[serde(default)]
    pub store_traces: Option<bool>,
    /// Frontend-generated run ID. The engine stamps every node built
    /// this run with this ID, and `agent://*` events echo it back.
    /// When omitted (legacy callers / tests), a UUID is generated here.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Anchor node to seed `last_node_id` from. When present, the
    /// run's first emitted edge is from `anchor_node_id` to whatever
    /// first node the agent builds.
    #[serde(default)]
    pub anchor_node_id: Option<String>,
    /// Prior conversation turns (goal + summary + run_id) injected
    /// inline above the current goal. Runtime order = chronological.
    #[serde(default)]
    pub prior_turns: Vec<PriorTurnWire>,
    /// Spec 2 master kill switch for episodic memory on this run.
    /// `None` = inherit the engine default (`true`); `Some(false)` =
    /// run with episodic disabled regardless of `EpisodicContext`.
    #[serde(default)]
    pub episodic_enabled: Option<bool>,
    /// Spec 2 retrieval depth — top-k episodes returned per trigger.
    /// `None` = engine default (2). Clamped to `[1, 10]` at the
    /// Tauri seam.
    #[serde(default)]
    pub retrieved_episodes_k: Option<usize>,
    /// Spec 2 D35 privacy opt-in: when `true`, recoveries from this
    /// workflow may be promoted into the global cross-workflow store.
    /// Default off keeps workflows isolated.
    #[serde(default)]
    pub episodic_global_participation: Option<bool>,
    /// Spec 3 master kill switch for procedural skills on this run.
    /// `None` = inherit the engine default (`true`); `Some(false)` =
    /// run with skill extraction/retrieval/replay disabled.
    #[serde(default)]
    pub skills_enabled: Option<bool>,
    /// Spec 3 retrieval depth — top-k applicable skills returned per
    /// `push_subgoal` boundary. Clamped to `[1, 10]` at the Tauri seam.
    #[serde(default)]
    pub applicable_skills_k: Option<usize>,
    /// Spec 3 privacy opt-in: when `true`, confirmed global skills may
    /// participate in retrieval for this run.
    #[serde(default)]
    pub skills_global_participation: Option<bool>,
}

/// Wire form of a prior-turn entry (matches
/// `clickweave_engine::agent::PriorTurn` with string UUIDs for JSON).
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct PriorTurnWire {
    pub goal: String,
    pub summary: String,
    pub run_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentStepPayload {
    pub run_id: String,
    pub summary: String,
    pub tool_name: String,
    pub step_number: usize,
}

// ── Handle ──────────────────────────────────────────────────────

#[derive(Default)]
pub struct AgentHandle {
    cancel_token: Option<CancellationToken>,
    task_handle: Option<tauri::async_runtime::JoinHandle<()>>,
    /// Pending approval oneshot sender — set when the agent is waiting for approval.
    pending_approval_tx: Option<tokio::sync::oneshot::Sender<bool>>,
    /// Pending disagreement-resolution oneshot sender — set after the
    /// engine halts on `CompletionDisagreement` and the Tauri task is
    /// waiting for the operator to confirm or cancel via
    /// `resolve_completion_disagreement`. Consumed by that command or by
    /// `force_stop` (which resolves it as Cancel so the stop path still
    /// writes a truthful terminal record).
    pending_disagreement_tx: Option<tokio::sync::oneshot::Sender<DisagreementResolutionAction>>,
    /// Generation ID for the current run. Used to tag events and reject stale ones.
    run_id: Option<String>,
}

impl AgentHandle {
    /// Cancel the running agent task.
    /// Returns `true` if a task was actually running (or starting).
    pub fn force_stop(&mut self) -> bool {
        // Check cancel_token too — it's installed before the task handle
        // so stop_agent works even during the spawn window.
        let had_task = self.cancel_token.is_some() || self.task_handle.is_some();
        if let Some(token) = self.cancel_token.take() {
            token.cancel();
        }
        // Send explicit cancellation through the approval channel instead
        // of silently dropping the sender. This ensures the engine sees
        // `Ok(false)` (rejection/replan) rather than `Err` (channel closed),
        // which would surface as `approval_unavailable` instead of `cancelled`.
        if let Some(tx) = self.pending_approval_tx.take() {
            let _ = tx.send(false);
        }
        // Same contract for the pending disagreement-resolution oneshot:
        // send an explicit Cancel so the Tauri task records a truthful
        // `DisagreementCancelled` terminal reason. Dropping the sender
        // would make the task fall through to "unknown", leaving the
        // variant index + events.jsonl without a proper record of the
        // operator's stop decision.
        if let Some(tx) = self.pending_disagreement_tx.take() {
            let _ = tx.send(DisagreementResolutionAction::Cancel);
        }
        had_task
    }
}

// ── Disagreement resolution ─────────────────────────────────────

/// Install a pending-disagreement oneshot in `AgentHandle` and wait for
/// the operator's decision. Races the wait against the run's
/// cancellation token so `stop_agent` fired *before* `resolve_completion_disagreement`
/// installs its sender still unblocks the task.
///
/// On resolution, appends a `CompletionDisagreementResolved` event to the
/// run's `events.jsonl` (durable trace) and returns the synthesized
/// `TerminalReason` so the caller can write the variant-index entry and
/// emit the final Tauri event from a single place.
///
/// Returns `None` when neither path fires (sender dropped cleanly) —
/// callers emit `agent://stopped { reason: cancelled }` in that case.
async fn await_disagreement_resolution(
    app: &tauri::AppHandle,
    cancel_token: &CancellationToken,
    storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    run_id: &str,
    agent_summary: String,
    vlm_reasoning: String,
) -> Option<TerminalReason> {
    let (tx, rx) = tokio::sync::oneshot::channel::<DisagreementResolutionAction>();
    {
        let handle = app.state::<Mutex<AgentHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.pending_disagreement_tx = Some(tx);
    }

    // Wait for the operator's decision, racing the run's cancellation
    // token so `stop_agent` during the adjudication window unblocks.
    //
    // `biased;` is load-bearing: without it, `tokio::select!` can pick
    // the cancel branch even when the resolver oneshot already carries
    // the operator's `Confirm`, which would silently overwrite the
    // user's decision with a `DisagreementCancelled` terminal record.
    // The resolver branch must always win when its channel is ready;
    // the cancel branch is the pure fallback for the adjudication-
    // window stop case (force_stop has no sender to consume because
    // `resolve_completion_disagreement` was never called).
    let action = tokio::select! {
        biased;
        res = rx => res.ok(),
        _ = cancel_token.cancelled() => {
            // Clear any stale sender the force_stop path did not consume
            // (theoretically impossible because force_stop always takes
            // it, but defensive is cheap here).
            let handle = app.state::<Mutex<AgentHandle>>();
            let mut guard = handle.lock().unwrap();
            guard.pending_disagreement_tx = None;
            Some(DisagreementResolutionAction::Cancel)
        }
    };

    let action = action?;

    // Persist the resolution to the durable run trace before any
    // terminal-emit side-effects. The Tauri event forwarder has already
    // exited by this point (the event_tx handle was dropped when the
    // engine returned), so we append directly via RunStorage.
    let resolved_event = AgentEvent::CompletionDisagreementResolved {
        action,
        agent_summary: agent_summary.clone(),
        vlm_reasoning: vlm_reasoning.clone(),
    };
    let _ = storage.lock().unwrap().append_agent_event(&resolved_event);
    // Also surface the decision as a lightweight Tauri event so UIs
    // outside the assistant panel (logs drawer, telemetry) observe the
    // resolution. This is in addition to the definitive `agent://complete`
    // / `agent://stopped` emission the caller performs next.
    let _ = app.emit(
        "agent://completion_disagreement_resolved",
        serde_json::json!({
            "run_id": run_id,
            "action": match action {
                DisagreementResolutionAction::Confirm => "confirm",
                DisagreementResolutionAction::Cancel => "cancel",
            },
        }),
    );

    Some(match action {
        DisagreementResolutionAction::Confirm => {
            TerminalReason::DisagreementConfirmed { agent_summary }
        }
        DisagreementResolutionAction::Cancel => TerminalReason::DisagreementCancelled {
            agent_summary,
            vlm_reasoning,
        },
    })
}

// ── Event forwarding seam ───────────────────────────────────────

/// Forward one `AgentEvent` to its paired `agent://*` Tauri event.
///
/// The persistence side of the forwarder (appending every event to
/// `events.jsonl`) stays at the call site so `RunStorage` lock ownership
/// is not smeared across this helper. `GoalComplete` is deliberately a
/// no-op: the terminal `agent://complete` is emitted from the main
/// run-agent task after the engine returns, and the
/// `CompletionDisagreementResolved` variant is emitted by the Tauri
/// layer itself (see `await_disagreement_resolution`), so neither
/// crosses this forwarder at runtime.
///
/// Extracted as a standalone function so the rubric-10 smoke test in
/// `run_agent_smoke_tests` can drive a scripted `AgentEvent` stream
/// against a mock `AppHandle` and assert the full (variant → topic)
/// mapping, locking the forwarder contract before Phase 3b deletes
/// `loop_runner.rs`.
pub(crate) fn forward_agent_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    run_id: &str,
    event: &AgentEvent,
) {
    match event {
        AgentEvent::StepCompleted {
            step_index,
            tool_name,
            summary,
        } => {
            let _ = app.emit(
                "agent://step",
                AgentStepPayload {
                    run_id: run_id.to_string(),
                    summary: summary.clone(),
                    tool_name: tool_name.clone(),
                    step_number: *step_index,
                },
            );
        }
        AgentEvent::NodeAdded { node } => {
            let _ = app.emit(
                "agent://node_added",
                serde_json::json!({ "run_id": run_id, "node": node }),
            );
        }
        AgentEvent::EdgeAdded { edge } => {
            let _ = app.emit(
                "agent://edge_added",
                serde_json::json!({ "run_id": run_id, "edge": edge }),
            );
        }
        AgentEvent::GoalComplete { .. } => {
            // Terminal completion is emitted as agent://complete by the
            // main task after the agent loop finishes. This in-band
            // event is only used for durable tracing.
        }
        AgentEvent::Error { message } => {
            let _ = app.emit(
                "agent://error",
                serde_json::json!({ "run_id": run_id, "message": message }),
            );
        }
        AgentEvent::Warning { message } => {
            let _ = app.emit(
                "agent://warning",
                serde_json::json!({ "run_id": run_id, "message": message }),
            );
        }
        AgentEvent::CdpConnected { app_name, port } => {
            let _ = app.emit(
                "agent://cdp_connected",
                serde_json::json!({
                    "run_id": run_id,
                    "app_name": app_name,
                    "port": port,
                }),
            );
        }
        AgentEvent::StepFailed {
            step_index,
            tool_name,
            error,
        } => {
            let _ = app.emit(
                "agent://step_failed",
                serde_json::json!({
                    "run_id": run_id,
                    "step_number": step_index,
                    "tool_name": tool_name,
                    "error": error,
                }),
            );
        }
        AgentEvent::SubAction { tool_name, summary } => {
            let _ = app.emit(
                "agent://sub_action",
                serde_json::json!({
                    "run_id": run_id,
                    "tool_name": tool_name,
                    "summary": summary,
                }),
            );
        }
        AgentEvent::CompletionDisagreement {
            screenshot_b64,
            vlm_reasoning,
            agent_summary,
        } => {
            let _ = app.emit(
                "agent://completion_disagreement",
                serde_json::json!({
                    "run_id": run_id,
                    "screenshot_b64": screenshot_b64,
                    "vlm_reasoning": vlm_reasoning,
                    "agent_summary": agent_summary,
                }),
            );
        }
        AgentEvent::ConsecutiveDestructiveCapHit {
            recent_tool_names,
            cap,
        } => {
            let _ = app.emit(
                "agent://consecutive_destructive_cap_hit",
                serde_json::json!({
                    "run_id": run_id,
                    "recent_tool_names": recent_tool_names,
                    "cap": cap,
                }),
            );
        }
        // `CompletionDisagreementResolved` is emitted by the Tauri layer
        // (not the engine) so the agent loop never sends it through this
        // channel. Persisting it is handled in
        // `await_disagreement_resolution`.
        AgentEvent::CompletionDisagreementResolved { .. } => {}
        AgentEvent::TaskStateChanged {
            run_id: event_run_id,
            task_state,
        } => {
            let _ = app.emit(
                "agent://task_state_changed",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "task_state": task_state,
                }),
            );
        }
        AgentEvent::WorldModelChanged {
            run_id: event_run_id,
            diff,
        } => {
            let _ = app.emit(
                "agent://world_model_changed",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "diff": diff,
                }),
            );
        }
        AgentEvent::BoundaryRecordWritten {
            run_id: event_run_id,
            boundary_kind,
            step_index,
            milestone_text,
        } => {
            let _ = app.emit(
                "agent://boundary_record_written",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "boundary_kind": boundary_kind,
                    "step_index": step_index,
                    "milestone_text": milestone_text,
                }),
            );
        }
        // Spec 2 D33: episodic-memory events. The runner emits
        // `EpisodesRetrieved` when retrieval surfaces candidates; the
        // background `EpisodicWriter` task emits `EpisodeWritten`
        // (insert/merge in the workflow-local store) and `EpisodePromoted`
        // (run-terminal promotion pass into the global store). All three
        // payloads carry the run's UUID so the frontend's stale-run
        // filter (`useAgentEvents::isStale`) drops late events from a
        // previous run.
        AgentEvent::EpisodesRetrieved {
            run_id: event_run_id,
            trigger,
            count,
            episode_ids,
            scope_breakdown,
        } => {
            let _ = app.emit(
                "agent://episodes_retrieved",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "trigger": trigger,
                    "count": count,
                    "episode_ids": episode_ids,
                    "scope_breakdown": scope_breakdown,
                }),
            );
        }
        AgentEvent::EpisodeWritten {
            run_id: event_run_id,
            outcome,
            episode_id,
            scope,
            occurrence_count,
        } => {
            let _ = app.emit(
                "agent://episode_written",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "outcome": outcome,
                    "episode_id": episode_id,
                    "scope": scope,
                    "occurrence_count": occurrence_count,
                }),
            );
        }
        AgentEvent::EpisodePromoted {
            run_id: event_run_id,
            promoted_episode_ids,
            skipped_count,
        } => {
            let _ = app.emit(
                "agent://episode_promoted",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "promoted_episode_ids": promoted_episode_ids,
                    "skipped_count": skipped_count,
                }),
            );
        }
        AgentEvent::SkillInvoked {
            run_id: event_run_id,
            skill_id,
            version,
            parameter_count,
        } => {
            let _ = app.emit(
                "agent://skill_invoked",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "skill_id": skill_id,
                    "version": version,
                    "parameter_count": parameter_count,
                }),
            );
        }
        AgentEvent::SkillExtracted {
            run_id: event_run_id,
            skill_id,
            version,
            state,
            scope,
        } => {
            let _ = app.emit(
                "agent://skill_extracted",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "skill_id": skill_id,
                    "version": version,
                    "state": state,
                    "scope": scope,
                }),
            );
        }
        AgentEvent::SkillConfirmed {
            run_id: event_run_id,
            skill_id,
            version,
        } => {
            let _ = app.emit(
                "agent://skill_confirmed",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "skill_id": skill_id,
                    "version": version,
                }),
            );
        }
    }
}

fn maybe_spawn_skill_proposal_task(
    event: &AgentEvent,
    skill_ctx: &SkillContext,
    agent_config: clickweave_llm::LlmConfig,
) {
    let AgentEvent::SkillExtracted {
        skill_id,
        version,
        state,
        scope,
        ..
    } = event
    else {
        return;
    };
    if !skill_ctx.enabled || *state != SkillState::Draft || *scope != SkillScope::ProjectLocal {
        return;
    }

    spawn_skill_proposal_task(skill_ctx, agent_config, skill_id.clone(), *version);
}

async fn wait_for_agent_event_drain(event_tx: &tokio::sync::mpsc::Sender<RunnerOutput>) {
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    if event_tx
        .send(RunnerOutput::DrainBarrier { ack: ack_tx })
        .await
        .is_ok()
    {
        let _ = ack_rx.await;
    }
}

async fn emit_after_agent_event_drain<R: tauri::Runtime>(
    event_tx: &tokio::sync::mpsc::Sender<RunnerOutput>,
    app: &tauri::AppHandle<R>,
    topic: &str,
    payload: serde_json::Value,
) {
    wait_for_agent_event_drain(event_tx).await;
    let _ = app.emit(topic, payload);
}

fn spawn_skill_proposal_task(
    skill_ctx: &SkillContext,
    agent_config: clickweave_llm::LlmConfig,
    skill_id: String,
    version: u32,
) {
    let skills_dir = skill_ctx.project_skills_dir.clone();
    tauri::async_runtime::spawn(async move {
        let store = SkillStore::new(skills_dir.clone());
        let skill_path = skills_dir.join(format!("{}-v{}.md", slugify(&skill_id), version));
        let Ok(skill) = store.read_skill(&skill_path) else {
            tracing::warn!(%skill_id, version, "skills: proposal task could not read skill file");
            return;
        };
        if skill.state != SkillState::Draft || skill.stats.occurrence_count < 3 {
            return;
        }
        let proposal_path = crate::llm::skill_proposal::proposal_path(&skills_dir, &skill);
        if proposal_path.exists() {
            return;
        }

        let mut provenance = skill.provenance.clone();
        provenance.sort_by_key(|p| p.completed_at);
        let start = provenance.len().saturating_sub(3);
        let contributing = provenance[start..].to_vec();

        let llm =
            clickweave_llm::LlmClient::new(agent_config.with_thinking(false).with_max_tokens(2048));
        match crate::llm::skill_proposal::propose_skill_refinement(&skill, &contributing, &llm)
            .await
        {
            Ok(proposal) => {
                if let Err(err) =
                    crate::llm::skill_proposal::write_skill_proposal(&skills_dir, &skill, &proposal)
                {
                    tracing::warn!(%skill_id, version, error = %err, "skills: failed to write proposal");
                }
            }
            Err(err) => {
                tracing::warn!(%skill_id, version, error = %err, "skills: proposal generation failed");
            }
        }
    });
}

fn ensure_agent_idle(app: &tauri::AppHandle) -> Result<(), CommandError> {
    let handle = app.state::<Mutex<AgentHandle>>();
    let guard = handle.lock().unwrap();
    if guard.cancel_token.is_some() || guard.task_handle.is_some() {
        return Err(CommandError::already_running());
    }
    Ok(())
}

fn parse_workflow_id(request: &AgentRunRequest) -> Result<uuid::Uuid, CommandError> {
    request
        .workflow_id
        .parse()
        .map_err(|_| CommandError::validation("Invalid workflow ID"))
}

fn resolve_run_id(request: &AgentRunRequest) -> Result<(String, uuid::Uuid), CommandError> {
    let run_id = request
        .run_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let run_uuid = run_id
        .parse()
        .map_err(|_| CommandError::validation("Invalid run_id"))?;
    Ok((run_id, run_uuid))
}

fn parse_anchor_node_id(request: &AgentRunRequest) -> Result<Option<uuid::Uuid>, CommandError> {
    match request.anchor_node_id.as_deref() {
        Some(s) if !s.is_empty() => s
            .parse()
            .map(Some)
            .map_err(|_| CommandError::validation("Invalid anchor_node_id")),
        _ => Ok(None),
    }
}

fn parse_prior_turns(
    request: &AgentRunRequest,
) -> Result<Vec<clickweave_engine::agent::PriorTurn>, CommandError> {
    request
        .prior_turns
        .iter()
        .map(|t| {
            let run_id: uuid::Uuid = t
                .run_id
                .parse()
                .map_err(|_| CommandError::validation("Invalid prior_turn.run_id"))?;
            Ok(clickweave_engine::agent::PriorTurn {
                goal: t.goal.clone(),
                summary: t.summary.clone(),
                run_id,
            })
        })
        .collect()
}

fn build_episodic_context(
    app: &tauri::AppHandle,
    storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    request: &AgentRunRequest,
    persist_traces: bool,
    enabled: bool,
    global_participation: bool,
) -> Result<EpisodicContext, CommandError> {
    if !persist_traces || !enabled {
        return Ok(EpisodicContext::disabled());
    }

    let wl_path = storage.lock().unwrap().base_path().join("episodic.sqlite");
    let global_path = if global_participation {
        Some(app_data_episodic_path(app)?)
    } else {
        None
    };
    Ok(EpisodicContext {
        enabled: true,
        workflow_local_path: wl_path,
        global_path,
        workflow_hash: request.workflow_id.clone(),
    })
}

fn build_skill_context(
    app: &tauri::AppHandle,
    storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    request: &AgentRunRequest,
    persist_traces: bool,
    enabled: bool,
    global_participation: bool,
) -> Result<SkillContext, CommandError> {
    let project_skills_dir = {
        let guard = storage.lock().unwrap();
        if persist_traces && enabled {
            guard
                .project_skills_dir()
                .map_err(|e| CommandError::io(format!("resolve project skills dir: {e}")))?
        } else {
            guard.base_path().join("skills")
        }
    };
    let global_skills_dir = if persist_traces && enabled && global_participation {
        Some(app_data_global_skills_dir(app)?)
    } else {
        None
    };
    Ok(SkillContext {
        enabled: persist_traces && enabled,
        project_skills_dir,
        global_skills_dir,
        project_id: request.workflow_id.clone(),
    })
}

fn agent_config_from_request(
    consecutive_destructive_cap: Option<usize>,
    allow_focus_window: Option<bool>,
    episodic_settings_enabled: bool,
    retrieved_episodes_k_override: Option<usize>,
    skills_settings_enabled: bool,
    applicable_skills_k_override: Option<usize>,
    skills_global_participation: bool,
) -> AgentConfig {
    let mut config = AgentConfig::default();
    if let Some(cap) = consecutive_destructive_cap {
        config.consecutive_destructive_cap = cap;
    }
    if let Some(allow) = allow_focus_window {
        config.allow_focus_window = allow;
    }
    config.episodic_enabled = episodic_settings_enabled;
    if let Some(k) = retrieved_episodes_k_override {
        config.retrieved_episodes_k = k.clamp(1, 10);
    }
    config.skills_enabled = skills_settings_enabled;
    if let Some(k) = applicable_skills_k_override {
        config.applicable_skills_k = k.clamp(1, 10);
    }
    config.skills_global_participation = skills_global_participation;
    config
}

fn install_agent_run_handle(app: &tauri::AppHandle, cancel_token: CancellationToken, run_id: &str) {
    let handle = app.state::<Mutex<AgentHandle>>();
    let mut guard = handle.lock().unwrap();
    guard.cancel_token = Some(cancel_token);
    guard.run_id = Some(run_id.to_string());
}

fn store_agent_task_handle(
    app: &tauri::AppHandle,
    task_handle: tauri::async_runtime::JoinHandle<()>,
) {
    let handle = app.state::<Mutex<AgentHandle>>();
    let mut guard = handle.lock().unwrap();
    guard.task_handle = Some(task_handle);
}

fn spawn_agent_event_forwarder(
    event_forwarder_token: CancellationToken,
    mut event_rx: tokio::sync::mpsc::Receiver<RunnerOutput>,
    event_storage: Arc<Mutex<clickweave_core::storage::RunStorage>>,
    event_emit_handle: tauri::AppHandle,
    event_run_id: String,
    proposal_skill_ctx: SkillContext,
    proposal_agent_config: clickweave_llm::LlmConfig,
    events_done_tx: tokio::sync::oneshot::Sender<()>,
) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = event_forwarder_token.cancelled() => {
                    drain_remaining_runner_outputs(
                        &mut event_rx,
                        &event_storage,
                        &proposal_skill_ctx,
                        proposal_agent_config.clone(),
                    );
                    break;
                }
                maybe_output = event_rx.recv() => {
                    match maybe_output {
                        Some(output) => handle_runner_output(
                            output,
                            &event_storage,
                            &event_emit_handle,
                            &event_run_id,
                            &proposal_skill_ctx,
                            proposal_agent_config.clone(),
                        ),
                        None => break,
                    }
                }
            }
        }
        let _ = events_done_tx.send(());
    });
}

fn drain_remaining_runner_outputs(
    event_rx: &mut tokio::sync::mpsc::Receiver<RunnerOutput>,
    event_storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    proposal_skill_ctx: &SkillContext,
    proposal_agent_config: clickweave_llm::LlmConfig,
) {
    while let Ok(output) = event_rx.try_recv() {
        match output {
            RunnerOutput::Event(event) => {
                let _ = event_storage.lock().unwrap().append_agent_event(&event);
            }
            RunnerOutput::DrainBarrier { ack } => {
                let _ = ack.send(());
            }
            RunnerOutput::SkillProposalNeeded {
                skill_id, version, ..
            } => {
                spawn_skill_proposal_task(
                    proposal_skill_ctx,
                    proposal_agent_config.clone(),
                    skill_id,
                    version,
                );
            }
        }
    }
}

fn handle_runner_output(
    output: RunnerOutput,
    event_storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    event_emit_handle: &tauri::AppHandle,
    event_run_id: &str,
    proposal_skill_ctx: &SkillContext,
    proposal_agent_config: clickweave_llm::LlmConfig,
) {
    match output {
        RunnerOutput::Event(event) => {
            let _ = event_storage.lock().unwrap().append_agent_event(&event);
            forward_agent_event(event_emit_handle, event_run_id, &event);
            maybe_spawn_skill_proposal_task(&event, proposal_skill_ctx, proposal_agent_config);
        }
        RunnerOutput::DrainBarrier { ack } => {
            let _ = ack.send(());
        }
        RunnerOutput::SkillProposalNeeded {
            skill_id, version, ..
        } => {
            spawn_skill_proposal_task(proposal_skill_ctx, proposal_agent_config, skill_id, version);
        }
    }
}

fn spawn_approval_forwarder(
    mut approval_rx: tokio::sync::mpsc::Receiver<(
        ApprovalRequest,
        tokio::sync::oneshot::Sender<bool>,
    )>,
    forwarder_token: CancellationToken,
    approval_emit_handle: tauri::AppHandle,
    approval_run_id: String,
    approval_done_tx: tokio::sync::oneshot::Sender<()>,
) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                req = approval_rx.recv() => {
                    match req {
                        Some((request, resp_tx)) => {
                            if forwarder_token.is_cancelled() {
                                let _ = resp_tx.send(false);
                                break;
                            }
                            install_pending_approval(&approval_emit_handle, resp_tx);
                            emit_approval_required(&approval_emit_handle, &approval_run_id, request);
                        }
                        None => break,
                    }
                }
                _ = forwarder_token.cancelled() => {
                    reject_queued_approval_requests(&mut approval_rx);
                    break;
                }
            }
        }
        let _ = approval_done_tx.send(());
    });
}

fn install_pending_approval(app: &tauri::AppHandle, resp_tx: tokio::sync::oneshot::Sender<bool>) {
    let handle = app.state::<Mutex<AgentHandle>>();
    let mut guard = handle.lock().unwrap();
    guard.pending_approval_tx = Some(resp_tx);
}

fn emit_approval_required(app: &tauri::AppHandle, run_id: &str, request: ApprovalRequest) {
    let _ = app.emit(
        "agent://approval_required",
        serde_json::json!({
            "run_id": run_id,
            "step_index": request.step_index,
            "tool_name": request.tool_name,
            "arguments": request.arguments,
            "description": request.description,
        }),
    );
}

fn reject_queued_approval_requests(
    approval_rx: &mut tokio::sync::mpsc::Receiver<(
        ApprovalRequest,
        tokio::sync::oneshot::Sender<bool>,
    )>,
) {
    while let Ok((_req, resp_tx)) = approval_rx.try_recv() {
        let _ = resp_tx.send(false);
    }
}

fn spawn_agent_cleanup(
    cleanup_handle: tauri::AppHandle,
    done_rx: tokio::sync::oneshot::Receiver<()>,
    events_done_rx: tokio::sync::oneshot::Receiver<()>,
    approval_done_rx: tokio::sync::oneshot::Receiver<()>,
) {
    tauri::async_runtime::spawn(async move {
        let _ = done_rx.await;
        let _ = events_done_rx.await;
        let _ = approval_done_rx.await;

        let handle = cleanup_handle.state::<Mutex<AgentHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.cancel_token = None;
        guard.task_handle = None;
        guard.pending_approval_tx = None;
        guard.pending_disagreement_tx = None;
        guard.run_id = None;
    });
}

struct AgentRunTaskInput {
    mcp_binary_path: String,
    agent_token: CancellationToken,
    terminal_event_tx: tokio::sync::mpsc::Sender<RunnerOutput>,
    emit_handle: tauri::AppHandle,
    task_run_id: String,
    done_tx: tokio::sync::oneshot::Sender<()>,
    agent_config: clickweave_llm::LlmConfig,
    consecutive_destructive_cap: Option<usize>,
    allow_focus_window: Option<bool>,
    episodic_settings_enabled: bool,
    retrieved_episodes_k_override: Option<usize>,
    skills_settings_enabled: bool,
    applicable_skills_k_override: Option<usize>,
    skills_global_participation: bool,
    storage: Arc<Mutex<clickweave_core::storage::RunStorage>>,
    event_tx: tokio::sync::mpsc::Sender<RunnerOutput>,
    approval_tx: tokio::sync::mpsc::Sender<(ApprovalRequest, tokio::sync::oneshot::Sender<bool>)>,
    goal: String,
    prior_turns: Vec<clickweave_engine::agent::PriorTurn>,
    permission_policy: Option<PermissionPolicy>,
    run_uuid: uuid::Uuid,
    anchor_uuid: Option<uuid::Uuid>,
    episodic_ctx: EpisodicContext,
    skill_ctx: SkillContext,
    persist_traces: bool,
    promotion_episodic_ctx: EpisodicContext,
    promotion_workflow_hash: String,
    run_start_utc: chrono::DateTime<chrono::Utc>,
}

fn spawn_agent_run_task(input: AgentRunTaskInput) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        run_agent_task(input).await;
    })
}

async fn run_agent_task(input: AgentRunTaskInput) {
    let AgentRunTaskInput {
        mcp_binary_path,
        agent_token,
        terminal_event_tx,
        emit_handle,
        task_run_id,
        done_tx,
        agent_config,
        consecutive_destructive_cap,
        allow_focus_window,
        episodic_settings_enabled,
        retrieved_episodes_k_override,
        skills_settings_enabled,
        applicable_skills_k_override,
        skills_global_participation,
        storage,
        event_tx,
        approval_tx,
        goal,
        prior_turns,
        permission_policy,
        run_uuid,
        anchor_uuid,
        episodic_ctx,
        skill_ctx,
        persist_traces,
        promotion_episodic_ctx,
        promotion_workflow_hash,
        run_start_utc,
    } = input;

    let Some(mcp) = spawn_mcp_for_agent(
        &mcp_binary_path,
        &agent_token,
        &terminal_event_tx,
        &emit_handle,
        &task_run_id,
    )
    .await
    else {
        let _ = done_tx.send(());
        return;
    };

    let llm = clickweave_llm::LlmClient::new(agent_config.clone().with_thinking(false));
    let vision: Arc<dyn clickweave_llm::DynChatBackend> = Arc::new(clickweave_llm::LlmClient::new(
        agent_config.with_thinking(false).with_max_tokens(512),
    ));
    let config = agent_config_from_request(
        consecutive_destructive_cap,
        allow_focus_window,
        episodic_settings_enabled,
        retrieved_episodes_k_override,
        skills_settings_enabled,
        applicable_skills_k_override,
        skills_global_participation,
    );

    let (variant_context, verification_artifacts_dir) = match initialize_agent_storage(&storage) {
        Ok(v) => v,
        Err(message) => {
            emit_agent_task_error(&terminal_event_tx, &emit_handle, &task_run_id, message).await;
            let _ = done_tx.send(());
            return;
        }
    };

    let goal_block = clickweave_engine::agent::build_goal_block(
        &goal,
        &prior_turns,
        if variant_context.is_empty() {
            None
        } else {
            Some(variant_context.as_str())
        },
        1000,
    );
    let channels = AgentChannels {
        event_tx: event_tx.clone(),
        approval_tx,
    };

    let result = tokio::select! {
        res = clickweave_engine::agent::run_agent_workflow(
            &llm,
            config,
            goal_block,
            &mcp,
            Some(channels),
            Some(vision.clone()),
            permission_policy,
            run_uuid,
            anchor_uuid,
            verification_artifacts_dir,
            Some(storage.clone()),
            Some(episodic_ctx.clone()),
            Some(skill_ctx.clone()),
        ) => res,
        _ = agent_token.cancelled() => {
            emit_after_agent_event_drain(
                &terminal_event_tx,
                &emit_handle,
                "agent://stopped",
                serde_json::json!({ "run_id": task_run_id, "reason": "cancelled" }),
            )
            .await;
            let _ = done_tx.send(());
            return;
        }
    };

    match result {
        Ok((state, writer_tx)) => {
            handle_agent_success(
                state,
                writer_tx,
                &emit_handle,
                &agent_token,
                &storage,
                &task_run_id,
                &terminal_event_tx,
                persist_traces,
                &promotion_episodic_ctx,
                &promotion_workflow_hash,
                run_start_utc,
            )
            .await;
        }
        Err(e) => {
            emit_agent_task_error(
                &terminal_event_tx,
                &emit_handle,
                &task_run_id,
                format!("{e}"),
            )
            .await;
        }
    }

    let _ = done_tx.send(());
}

async fn spawn_mcp_for_agent(
    mcp_binary_path: &str,
    agent_token: &CancellationToken,
    terminal_event_tx: &tokio::sync::mpsc::Sender<RunnerOutput>,
    emit_handle: &tauri::AppHandle,
    task_run_id: &str,
) -> Option<clickweave_mcp::McpClient> {
    tokio::select! {
        res = clickweave_mcp::McpClient::spawn(mcp_binary_path, &[]) => {
            match res {
                Ok(m) => Some(m),
                Err(e) => {
                    emit_agent_task_error(
                        terminal_event_tx,
                        emit_handle,
                        task_run_id,
                        format!("MCP spawn failed: {e}"),
                    )
                    .await;
                    None
                }
            }
        }
        _ = agent_token.cancelled() => {
            emit_after_agent_event_drain(
                terminal_event_tx,
                emit_handle,
                "agent://stopped",
                serde_json::json!({ "run_id": task_run_id, "reason": "cancelled" }),
            )
            .await;
            None
        }
    }
}

fn initialize_agent_storage(
    storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
) -> Result<(String, Option<std::path::PathBuf>), String> {
    let mut guard = storage.lock().unwrap();
    if let Err(e) = guard.begin_execution() {
        return Err(format!("Run storage init failed: {e}"));
    }
    let variant_index = VariantIndex::load_existing(&guard.variant_index_path(), guard.base_path());
    Ok((
        variant_index.as_context_text(),
        guard.execution_artifacts_dir(),
    ))
}

async fn emit_agent_task_error(
    terminal_event_tx: &tokio::sync::mpsc::Sender<RunnerOutput>,
    emit_handle: &tauri::AppHandle,
    task_run_id: &str,
    message: String,
) {
    emit_after_agent_event_drain(
        terminal_event_tx,
        emit_handle,
        "agent://error",
        serde_json::json!({ "run_id": task_run_id, "message": message }),
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn handle_agent_success(
    state: AgentState,
    writer_tx: Option<
        tokio::sync::mpsc::Sender<clickweave_engine::agent::episodic::types::WriteRequest>,
    >,
    emit_handle: &tauri::AppHandle,
    agent_token: &CancellationToken,
    storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    task_run_id: &str,
    terminal_event_tx: &tokio::sync::mpsc::Sender<RunnerOutput>,
    persist_traces: bool,
    promotion_episodic_ctx: &EpisodicContext,
    promotion_workflow_hash: &str,
    run_start_utc: chrono::DateTime<chrono::Utc>,
) {
    let resolved_terminal = resolve_terminal_reason(
        state.terminal_reason,
        emit_handle,
        agent_token,
        storage,
        task_run_id,
    )
    .await;
    append_variant_entry(storage, persist_traces, &resolved_terminal);
    emit_resolved_terminal(
        terminal_event_tx,
        emit_handle,
        task_run_id,
        &resolved_terminal,
    )
    .await;
    queue_terminal_promotion(
        writer_tx,
        emit_handle,
        task_run_id,
        promotion_episodic_ctx,
        promotion_workflow_hash,
        run_start_utc,
        &resolved_terminal,
    )
    .await;
}

async fn resolve_terminal_reason(
    terminal_reason: Option<TerminalReason>,
    emit_handle: &tauri::AppHandle,
    agent_token: &CancellationToken,
    storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    task_run_id: &str,
) -> Option<TerminalReason> {
    match terminal_reason {
        Some(TerminalReason::CompletionDisagreement {
            agent_summary,
            vlm_reasoning,
        }) => {
            await_disagreement_resolution(
                emit_handle,
                agent_token,
                storage,
                task_run_id,
                agent_summary,
                vlm_reasoning,
            )
            .await
        }
        other => other,
    }
}

fn append_variant_entry(
    storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    persist_traces: bool,
    resolved_terminal: &Option<TerminalReason>,
) {
    if !persist_traces {
        return;
    }
    let (divergence_summary, success) = match resolved_terminal {
        Some(reason) => (reason.divergence_summary(), reason.is_completed()),
        None => ("Stopped: unknown reason".to_string(), false),
    };
    let variant_entry = VariantEntry {
        execution_dir: storage
            .lock()
            .unwrap()
            .execution_dir_name()
            .unwrap_or("unknown")
            .to_string(),
        diverged_at_step: None,
        divergence_summary,
        success,
    };
    let _ = VariantIndex::append(
        &storage.lock().unwrap().variant_index_path(),
        &variant_entry,
    );
}

async fn emit_resolved_terminal(
    terminal_event_tx: &tokio::sync::mpsc::Sender<RunnerOutput>,
    emit_handle: &tauri::AppHandle,
    task_run_id: &str,
    resolved_terminal: &Option<TerminalReason>,
) {
    match resolved_terminal {
        Some(TerminalReason::Completed { summary })
        | Some(TerminalReason::DisagreementConfirmed {
            agent_summary: summary,
        }) => {
            emit_after_agent_event_drain(
                terminal_event_tx,
                emit_handle,
                "agent://complete",
                serde_json::json!({ "run_id": task_run_id, "summary": summary }),
            )
            .await;
        }
        Some(TerminalReason::DisagreementCancelled { .. }) => {
            emit_after_agent_event_drain(
                terminal_event_tx,
                emit_handle,
                "agent://stopped",
                serde_json::json!({
                    "run_id": task_run_id,
                    "reason": "user_cancelled_disagreement",
                }),
            )
            .await;
        }
        Some(reason) => {
            let mut payload = serde_json::to_value(reason).unwrap_or_default();
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("run_id".to_string(), serde_json::json!(task_run_id));
            }
            emit_after_agent_event_drain(
                terminal_event_tx,
                emit_handle,
                "agent://stopped",
                payload,
            )
            .await;
        }
        None => {
            emit_after_agent_event_drain(
                terminal_event_tx,
                emit_handle,
                "agent://stopped",
                serde_json::json!({ "run_id": task_run_id, "reason": "cancelled" }),
            )
            .await;
        }
    }
}

async fn queue_terminal_promotion(
    writer_tx: Option<
        tokio::sync::mpsc::Sender<clickweave_engine::agent::episodic::types::WriteRequest>,
    >,
    emit_handle: &tauri::AppHandle,
    task_run_id: &str,
    promotion_episodic_ctx: &EpisodicContext,
    promotion_workflow_hash: &str,
    run_start_utc: chrono::DateTime<chrono::Utc>,
    resolved_terminal: &Option<TerminalReason>,
) {
    if !promotion_episodic_ctx.enabled || promotion_episodic_ctx.global_path.is_none() {
        return;
    }
    let Some(tx) = writer_tx else {
        return;
    };
    use clickweave_engine::agent::episodic::{
        PromotionTerminalKind, types::WriteRequest as EpisodicWriteRequest,
    };
    let terminal_kind = match resolved_terminal {
        Some(TerminalReason::Completed { .. }) => PromotionTerminalKind::Clean,
        _ => PromotionTerminalKind::SkipPromotion,
    };
    if let Err(e) = tx.try_send(EpisodicWriteRequest::PromotePass {
        workflow_hash: promotion_workflow_hash.to_string(),
        terminal_kind,
        run_started_at: run_start_utc,
    }) {
        tracing::warn!(error = %e, "episodic: PromotePass dropped at terminal");
        let _ = emit_handle.emit(
            "agent://warning",
            serde_json::json!({
                "run_id": task_run_id,
                "message": format!("episodic: promotion dropped: backpressure ({e})"),
            }),
        );
    }
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel::<()>();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        if tx
            .send(EpisodicWriteRequest::Flush { ack: ack_tx })
            .await
            .is_err()
        {
            return;
        }
        let _ = ack_rx.await;
    })
    .await;
}

// ── Commands ────────────────────────────────────────────────────

#[tauri::command]
#[specta::specta]
pub async fn run_agent(
    app: tauri::AppHandle,
    request: AgentRunRequest,
) -> Result<(), CommandError> {
    ensure_agent_idle(&app)?;

    let mcp_binary_path =
        crate::mcp_resolve::resolve_mcp_binary().map_err(|e| CommandError::mcp(format!("{e}")))?;

    let workflow_id = parse_workflow_id(&request)?;

    let mut storage = resolve_storage(
        &app,
        &request.project_path,
        &request.workflow_name,
        workflow_id,
    );
    // Privacy kill switch: an explicit `false` from the UI disables
    // all on-disk writes for this run. The default is persist-on to
    // preserve existing behaviour when the UI does not send the flag.
    let persist_traces = request.store_traces.unwrap_or(true);
    storage.set_persistent(persist_traces);
    let storage = Arc::new(Mutex::new(storage));

    // Generate a per-run generation ID so event consumers can reject
    // stale events from a previous run that drain after stop/restart.
    // The frontend may supply its own run_id so the user message bubble
    // can be tagged before `agent://started` arrives — honor it when
    // present and syntactically valid.
    let (run_id, run_uuid) = resolve_run_id(&request)?;
    let anchor_uuid = parse_anchor_node_id(&request)?;
    let prior_turns = parse_prior_turns(&request)?;

    let consecutive_destructive_cap = request.consecutive_destructive_cap;
    let allow_focus_window = request.allow_focus_window;
    let episodic_settings_enabled = request.episodic_enabled.unwrap_or(true);
    let retrieved_episodes_k_override = request.retrieved_episodes_k;
    let episodic_global_participation = request.episodic_global_participation.unwrap_or(false);
    let skills_settings_enabled = request.skills_enabled.unwrap_or(true);
    let applicable_skills_k_override = request.applicable_skills_k;
    let skills_global_participation = request.skills_global_participation.unwrap_or(false);

    let episodic_ctx = build_episodic_context(
        &app,
        &storage,
        &request,
        persist_traces,
        episodic_settings_enabled,
        episodic_global_participation,
    )?;
    let skill_ctx = build_skill_context(
        &app,
        &storage,
        &request,
        persist_traces,
        skills_settings_enabled,
        skills_global_participation,
    )?;
    let agent_config = request.agent.into_llm_config(None);
    let permission_policy: Option<PermissionPolicy> = request.permissions.map(Into::into);

    // Capture the run-start timestamp so PromotePass scopes promotion
    // to episodes touched during this run.
    let run_start_utc = chrono::Utc::now();

    let cancel_token = CancellationToken::new();
    let agent_token = cancel_token.clone();
    let forwarder_token = cancel_token.clone();
    let event_forwarder_token = cancel_token.clone();

    // Live event channel: agent runner -> Tauri event emitter
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<RunnerOutput>(64);

    // Approval channel: agent runner sends requests, we forward to UI and store
    // the oneshot response sender in the handle for `approve_agent_action` to use.
    let (approval_tx, approval_rx) =
        tokio::sync::mpsc::channel::<(ApprovalRequest, tokio::sync::oneshot::Sender<bool>)>(1);

    let emit_handle = app.clone();
    let event_emit_handle = app.clone();
    let approval_emit_handle = app.clone();
    let cleanup_handle = app.clone();
    let goal = request.goal.clone();
    let task_storage = storage.clone();
    let event_storage = storage.clone();
    let task_run_id = run_id.clone();
    let event_run_id = run_id.clone();
    let terminal_event_tx = event_tx.clone();
    let approval_run_id = run_id.clone();
    let task_episodic_ctx = episodic_ctx.clone();
    let task_skill_ctx = skill_ctx.clone();
    let proposal_skill_ctx = skill_ctx.clone();
    let proposal_agent_config = agent_config.clone();
    let promotion_episodic_ctx = episodic_ctx.clone();
    let promotion_workflow_hash = episodic_ctx.workflow_hash.clone();

    // Channels used to signal cleanup when the agent task, event forwarder,
    // and approval forwarder have all finished, preventing stale event leakage.
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let (events_done_tx, events_done_rx) = tokio::sync::oneshot::channel::<()>();
    let (approval_done_tx, approval_done_rx) = tokio::sync::oneshot::channel::<()>();

    // Install cancel_token and run_id before spawning so stop_agent() works
    // even during the spawn window (before task_handle is available).
    install_agent_run_handle(&app, cancel_token, &run_id);

    // Emit agent://started so the frontend knows the run_id before any other events.
    let _ = app.emit("agent://started", serde_json::json!({ "run_id": &run_id }));

    let task_handle = spawn_agent_run_task(AgentRunTaskInput {
        mcp_binary_path,
        agent_token,
        terminal_event_tx,
        emit_handle,
        task_run_id,
        done_tx,
        agent_config: agent_config.clone(),
        consecutive_destructive_cap,
        allow_focus_window,
        episodic_settings_enabled,
        retrieved_episodes_k_override,
        skills_settings_enabled,
        applicable_skills_k_override,
        skills_global_participation,
        storage: task_storage,
        event_tx: event_tx.clone(),
        approval_tx,
        goal,
        prior_turns,
        permission_policy,
        run_uuid,
        anchor_uuid,
        episodic_ctx: task_episodic_ctx,
        skill_ctx: task_skill_ctx,
        persist_traces,
        promotion_episodic_ctx,
        promotion_workflow_hash,
        run_start_utc,
    });

    spawn_agent_event_forwarder(
        event_forwarder_token,
        event_rx,
        event_storage,
        event_emit_handle,
        event_run_id,
        proposal_skill_ctx,
        proposal_agent_config,
        events_done_tx,
    );
    spawn_approval_forwarder(
        approval_rx,
        forwarder_token,
        approval_emit_handle,
        approval_run_id,
        approval_done_tx,
    );
    store_agent_task_handle(&app, task_handle);
    spawn_agent_cleanup(cleanup_handle, done_rx, events_done_rx, approval_done_rx);

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn stop_agent(app: tauri::AppHandle) -> Result<(), CommandError> {
    let handle = app.state::<Mutex<AgentHandle>>();
    let mut guard = handle.lock().unwrap();
    if !guard.force_stop() {
        return Err(CommandError::validation("No agent is running"));
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn approve_agent_action(
    app: tauri::AppHandle,
    approved: bool,
) -> Result<(), CommandError> {
    let handle = app.state::<Mutex<AgentHandle>>();
    let mut guard = handle.lock().unwrap();
    let tx = guard
        .pending_approval_tx
        .take()
        .ok_or(CommandError::validation("No pending approval request"))?;
    drop(guard);

    tx.send(approved).map_err(|_| {
        CommandError::validation("Approval channel closed — agent task may have ended")
    })
}

/// Wire form for `resolve_completion_disagreement`. Mirrors
/// `DisagreementResolutionAction` but derives `specta::Type` so the
/// TypeScript binding picks it up.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum CompletionDisagreementActionWire {
    Confirm,
    Cancel,
}

impl From<CompletionDisagreementActionWire> for DisagreementResolutionAction {
    fn from(a: CompletionDisagreementActionWire) -> Self {
        match a {
            CompletionDisagreementActionWire::Confirm => DisagreementResolutionAction::Confirm,
            CompletionDisagreementActionWire::Cancel => DisagreementResolutionAction::Cancel,
        }
    }
}

/// Resolve a pending VLM completion disagreement. The operator picks
/// either `confirm` (override the VLM, mark the run complete) or
/// `cancel` (agree with the VLM, halt the run). The backend records the
/// decision to `events.jsonl` + `variant_index.jsonl` and emits the
/// appropriate terminal Tauri event.
///
/// Concurrency note: the AgentHandle lock is held across the oneshot
/// send on purpose. `force_stop` (the Stop button) also locks the
/// AgentHandle, cancels the run's CancellationToken, and takes the
/// disagreement sender from the same slot. If this command released
/// the lock after `.take()` but before `.send()`, a concurrent
/// `force_stop` could trip the cancel token in the gap and the
/// `tokio::select!` in `await_disagreement_resolution` would pick the
/// cancel branch before the confirm ever arrived — silently losing
/// the operator's decision. `oneshot::Sender::send` is synchronous
/// and infallible except for a dropped receiver, so holding the
/// `std::sync::Mutex` across it is cheap and race-closing.
#[tauri::command]
#[specta::specta]
pub async fn resolve_completion_disagreement(
    app: tauri::AppHandle,
    action: CompletionDisagreementActionWire,
) -> Result<(), CommandError> {
    let handle = app.state::<Mutex<AgentHandle>>();
    let mut guard = handle.lock().unwrap();
    let tx = guard
        .pending_disagreement_tx
        .take()
        .ok_or(CommandError::validation(
            "No pending completion disagreement",
        ))?;
    tx.send(action.into()).map_err(|_| {
        CommandError::validation("Disagreement channel closed — agent task may have ended")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: `AgentHandle::force_stop` must NOT drop the pending
    /// approval sender silently. Dropping surfaces to the engine as
    /// `Err(channel closed)` → `TerminalReason::ApprovalUnavailable`,
    /// which the Tauri layer then emits as `agent://stopped { reason:
    /// approval_unavailable }`. The fix sends `Ok(false)` explicitly so
    /// the engine treats the stop as a rejection (`Replan`) and the
    /// outer select races on `cancel_token.cancel()` to emit
    /// `agent://stopped { reason: cancelled }`.
    #[test]
    fn force_stop_sends_rejection_through_pending_approval() {
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        let mut handle = AgentHandle {
            cancel_token: Some(CancellationToken::new()),
            pending_approval_tx: Some(tx),
            ..Default::default()
        };

        let had_task = handle.force_stop();

        assert!(
            had_task,
            "force_stop should report true when cancel_token is installed"
        );
        // The receiver must see `Ok(false)` — not `Err` from a dropped sender.
        assert_eq!(
            rx.blocking_recv(),
            Ok(false),
            "force_stop must send explicit rejection, not drop the oneshot"
        );
    }

    /// `force_stop` must also cancel the CancellationToken so the outer
    /// agent task observes the stop during the spawn window (before
    /// `task_handle` is installed). The scenario: a user hits Stop while
    /// MCP spawn is still in progress.
    #[test]
    fn force_stop_cancels_token_for_spawn_window_stop() {
        let token = CancellationToken::new();
        let mut handle = AgentHandle {
            cancel_token: Some(token.clone()),
            ..Default::default()
        };
        // Simulate the spawn window: no task_handle, no pending approval.
        // `force_stop` must still succeed — the token alone is sufficient
        // evidence that a run is in flight.

        let had_task = handle.force_stop();

        assert!(
            had_task,
            "force_stop must return true when a cancel_token is present \
             even without a task_handle (the spawn window)"
        );
        assert!(
            token.is_cancelled(),
            "The CancellationToken must be cancelled so the spawning \
             task sees the stop before it finishes MCP bring-up"
        );
    }

    /// `force_stop` must return false when no run is active, so the
    /// Tauri command can return a validation error instead of silently
    /// succeeding.
    #[test]
    fn force_stop_returns_false_when_no_run_active() {
        let mut handle = AgentHandle::default();
        let had_task = handle.force_stop();
        assert!(
            !had_task,
            "force_stop must return false when no run is active"
        );
    }

    /// When a VLM completion disagreement is pending, `force_stop` must
    /// resolve the oneshot as `Cancel` — not drop it. Dropping would
    /// surface as a receiver error in the Tauri task, leaving the run
    /// without a truthful terminal record (variant index + events.jsonl
    /// entry both missing).
    #[test]
    fn force_stop_resolves_pending_disagreement_as_cancel() {
        let (tx, rx) = tokio::sync::oneshot::channel::<DisagreementResolutionAction>();
        let mut handle = AgentHandle {
            cancel_token: Some(CancellationToken::new()),
            pending_disagreement_tx: Some(tx),
            ..Default::default()
        };

        let had_task = handle.force_stop();

        assert!(
            had_task,
            "force_stop must report true when a pending disagreement is installed"
        );
        assert_eq!(
            rx.blocking_recv(),
            Ok(DisagreementResolutionAction::Cancel),
            "force_stop must send explicit Cancel through the disagreement channel, \
             not drop the oneshot (drops cause ambiguous `unknown` terminal records)"
        );
    }

    /// Regression: even though `resolve_completion_disagreement` now
    /// holds the AgentHandle lock across `tx.send(...)`, both branches
    /// of the `await_disagreement_resolution` select can still be ready
    /// at the same time — the loop's own cancellation path (e.g., a
    /// workflow-level cancel or shutdown) can cancel the token
    /// independently of `force_stop`, so a Confirm already sitting in
    /// the oneshot can race a tripped token. Without `biased;`,
    /// `tokio::select!` may pick the cancel branch and silently
    /// overwrite the confirm with a DisagreementCancelled terminal
    /// record. This test asserts the biased-select policy preserves
    /// the operator's decision.
    #[tokio::test]
    async fn biased_select_preserves_confirm_when_token_also_cancelled() {
        let (tx, rx) = tokio::sync::oneshot::channel::<DisagreementResolutionAction>();
        let token = CancellationToken::new();

        // Arrange: both branches are ready simultaneously — the confirm
        // has been sent and the cancel-token has been tripped.
        tx.send(DisagreementResolutionAction::Confirm).unwrap();
        token.cancel();

        let action = tokio::select! {
            biased;
            res = rx => res.ok(),
            _ = token.cancelled() => Some(DisagreementResolutionAction::Cancel),
        };

        assert_eq!(
            action,
            Some(DisagreementResolutionAction::Confirm),
            "biased select must prefer the resolver oneshot over a \
             cancelled token so the operator's Confirm is never overwritten"
        );
    }

    /// Regression: `resolve_completion_disagreement` must hold the
    /// `AgentHandle` lock across `tx.send(...)`. If the lock were
    /// released after `.take()` but before `.send()`, a concurrent
    /// `force_stop` could cancel the run's CancellationToken in the
    /// gap — and then the select race in `await_disagreement_resolution`
    /// would take the cancel branch before the confirm ever arrived,
    /// silently overwriting the operator's decision. This test
    /// simulates the interleaving: after the resolver's critical
    /// section completes (ordered by the AgentHandle mutex), a
    /// subsequent `force_stop` must find no pending sender and the
    /// receiver must already hold the Confirm. Asserting this
    /// invariant documents that the lock-hold-across-send policy is
    /// load-bearing, not incidental.
    #[test]
    fn resolver_critical_section_closes_confirm_vs_force_stop_window() {
        let (tx, rx) = tokio::sync::oneshot::channel::<DisagreementResolutionAction>();
        let token = CancellationToken::new();
        let handle_mutex = Mutex::new(AgentHandle {
            cancel_token: Some(token.clone()),
            pending_disagreement_tx: Some(tx),
            ..Default::default()
        });

        // Simulate the resolver's critical section — `.take()` the sender
        // and send on it while still holding the lock. This mirrors the
        // real command.
        {
            let mut guard = handle_mutex.lock().unwrap();
            let tx = guard
                .pending_disagreement_tx
                .take()
                .expect("pending_disagreement_tx should be installed");
            tx.send(DisagreementResolutionAction::Confirm).unwrap();
        }

        // A later `force_stop` then observes no sender to consume (so
        // it cannot overwrite the confirm) and only cancels the token.
        let had_task = {
            let mut guard = handle_mutex.lock().unwrap();
            guard.force_stop()
        };

        assert!(had_task, "force_stop should report true on active run");
        assert!(token.is_cancelled(), "force_stop must cancel the token");
        assert_eq!(
            rx.blocking_recv(),
            Ok(DisagreementResolutionAction::Confirm),
            "receiver must still see the operator's Confirm — force_stop \
             had no pending sender to overwrite it with Cancel"
        );
    }
}

#[cfg(test)]
mod run_agent_smoke_tests {
    //! Rubric-10 gate for Phase 3b cutover (D-PR2 / Task 3b.0).
    //!
    //! This test covers the user-visible Tauri seam of the agent run:
    //! the engine produces `AgentEvent`s, the Tauri forwarder persists
    //! every event to `events.jsonl` and fans it out to a matching
    //! `agent://*` topic. Because the actual `run_agent` command
    //! constructs a real `LlmClient` and spawns an MCP subprocess, the
    //! scripted smoke test drives the backend-of-Tauri surface directly:
    //!
    //! - calls `clickweave_engine::agent::run_agent_workflow` with the
    //!   shared `ScriptedLlm` + `StaticMcp` stubs (mirrors what
    //!   `run_agent` would do after MCP bring-up),
    //! - drains the engine event channel through a channel-pump loop
    //!   that invokes the exact same `forward_agent_event` helper and
    //!   `RunStorage::append_agent_event` call the production spawn
    //!   uses,
    //! - captures `agent://*` emits via `tauri::test::mock_app()` +
    //!   per-topic `listen_any` handlers,
    //! - asserts emit count matches `AgentEvent` line count in
    //!   `events.jsonl` (filtered to exclude `StepRecord` boundary
    //!   writes, which live in the same file per Task 3a.6.5), and
    //! - asserts the legacy `AgentState` wire-shape
    //!   (`state.steps.len()` matches the scripted tool-call count and
    //!   `state.terminal_reason` is `Completed`).
    //!
    //! Any future event-forwarding regression — a missing match arm on
    //! a new `AgentEvent` variant, a dropped persistence call, a
    //! divergent emit topic — fails this test.

    use super::*;
    use clickweave_engine::agent::test_stubs::{ScriptedLlm, StaticMcp, llm_reply_tool};
    use clickweave_engine::agent::{AgentConfig, run_agent_workflow};
    use std::sync::{Arc, Mutex};
    use tauri::Listener;

    /// Every `agent://*` topic `forward_agent_event` can emit. Listed
    /// explicitly so the test panics loud if a new `AgentEvent` variant
    /// is added without a matching topic — keep in sync with
    /// `forward_agent_event`.
    const AGENT_TOPICS: &[&str] = &[
        "agent://step",
        "agent://node_added",
        "agent://edge_added",
        "agent://error",
        "agent://warning",
        "agent://cdp_connected",
        "agent://step_failed",
        "agent://sub_action",
        "agent://completion_disagreement",
        "agent://consecutive_destructive_cap_hit",
        "agent://task_state_changed",
        "agent://world_model_changed",
        "agent://boundary_record_written",
        "agent://episodes_retrieved",
        "agent://episode_written",
        "agent://episode_promoted",
    ];

    fn agent_event_line_count(events_path: &std::path::Path) -> usize {
        std::fs::read_to_string(events_path)
            .ok()
            .map(|raw| {
                raw.lines()
                    .filter(|line| !line.is_empty())
                    .filter(|line| {
                        serde_json::from_str::<serde_json::Value>(line)
                            .ok()
                            .and_then(|value| value.get("type").cloned())
                            .is_some()
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    async fn wait_for_captured_count(
        captured: &Arc<Mutex<Vec<(String, String)>>>,
        expected: usize,
    ) {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if captured.lock().unwrap().len() >= expected {
                    break;
                }
                tokio::task::yield_now().await;
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("captured Tauri events in time");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runner_output_forwarder_skips_drain_barrier_and_persists_events() {
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let run_id = uuid::Uuid::new_v4().to_string();

        let tmp = tempfile::tempdir().expect("tempdir");
        let workflow_name = "runner-output-forwarder";
        let mut storage_inner =
            clickweave_core::storage::RunStorage::new(tmp.path(), workflow_name);
        let exec_dir = storage_inner.begin_execution().expect("begin_execution");
        let events_path = tmp
            .path()
            .join(".clickweave")
            .join("runs")
            .join(workflow_name)
            .join(&exec_dir)
            .join("events.jsonl");
        let storage = Arc::new(Mutex::new(storage_inner));

        let (tx, mut rx) = tokio::sync::mpsc::channel::<RunnerOutput>(8);
        let forwarded: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let forwarded_for_task = Arc::clone(&forwarded);
        let storage_for_task = Arc::clone(&storage);
        let forwarder_task = tokio::spawn(async move {
            while let Some(output) = rx.recv().await {
                match output {
                    RunnerOutput::Event(event) => {
                        let _ = storage_for_task.lock().unwrap().append_agent_event(&event);
                        forward_agent_event(&handle, &run_id, &event);
                        forwarded_for_task.lock().unwrap().push(event);
                    }
                    RunnerOutput::DrainBarrier { ack } => {
                        let _ = ack.send(());
                    }
                    RunnerOutput::SkillProposalNeeded { .. } => {}
                }
            }
        });

        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        tx.send(RunnerOutput::DrainBarrier { ack: ack_tx })
            .await
            .expect("send drain barrier");
        tokio::time::timeout(std::time::Duration::from_secs(1), ack_rx)
            .await
            .expect("drain barrier ack in time")
            .expect("drain barrier ack sender alive");
        assert_eq!(
            agent_event_line_count(&events_path),
            0,
            "DrainBarrier must not append an AgentEvent line",
        );

        tx.send(RunnerOutput::Event(AgentEvent::Warning {
            message: "synthetic warning".to_string(),
        }))
        .await
        .expect("send warning event");
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        tx.send(RunnerOutput::DrainBarrier { ack: ack_tx })
            .await
            .expect("send second drain barrier");
        tokio::time::timeout(std::time::Duration::from_secs(1), ack_rx)
            .await
            .expect("second drain barrier ack in time")
            .expect("second drain barrier ack sender alive");

        drop(tx);
        forwarder_task.await.expect("forwarder joined");

        assert_eq!(
            agent_event_line_count(&events_path),
            1,
            "RunnerOutput::Event must append exactly one AgentEvent line",
        );
        assert!(
            matches!(
                forwarded.lock().unwrap().as_slice(),
                [AgentEvent::Warning { .. }]
            ),
            "only the durable event should be forwarded",
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terminal_emit_waits_for_prior_runner_output_drain() {
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let run_id = uuid::Uuid::new_v4().to_string();

        let captured: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        for topic in ["agent://warning", "agent://complete"] {
            let captured = Arc::clone(&captured);
            handle.listen_any(topic, move |evt| {
                captured
                    .lock()
                    .unwrap()
                    .push((topic.to_string(), evt.payload().to_string()));
            });
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel::<RunnerOutput>(8);
        let forwarder_handle = handle.clone();
        let forwarder_run_id = run_id.clone();
        let forwarder_task = tokio::spawn(async move {
            while let Some(output) = rx.recv().await {
                match output {
                    RunnerOutput::Event(event) => {
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        forward_agent_event(&forwarder_handle, &forwarder_run_id, &event);
                    }
                    RunnerOutput::DrainBarrier { ack } => {
                        let _ = ack.send(());
                    }
                    RunnerOutput::SkillProposalNeeded { .. } => {}
                }
            }
        });

        tx.send(RunnerOutput::Event(AgentEvent::Warning {
            message: "queued before terminal".to_string(),
        }))
        .await
        .expect("send prior event");
        emit_after_agent_event_drain(
            &tx,
            &handle,
            "agent://complete",
            serde_json::json!({ "run_id": run_id, "summary": "done" }),
        )
        .await;
        drop(tx);
        forwarder_task.await.expect("forwarder joined");

        wait_for_captured_count(&captured, 2).await;
        let topics: Vec<String> = captured
            .lock()
            .unwrap()
            .iter()
            .map(|(topic, _)| topic.clone())
            .collect();
        assert_eq!(
            topics,
            vec![
                "agent://warning".to_string(),
                "agent://complete".to_string()
            ],
            "terminal emit must not outrun already queued per-step events",
        );
    }

    /// Rubric-10 gate (D-PR2): every `AgentEvent` the engine emits
    /// must (1) reach `events.jsonl` and (2) route to exactly one
    /// `agent://<topic>` via `forward_agent_event`. The scripted
    /// scenario runs two tool calls and terminates on `agent_done`,
    /// which produces a known-non-zero event stream (at minimum
    /// `StepCompleted`; typically also `NodeAdded` / `EdgeAdded` /
    /// `GoalComplete`). The test does not pin an exact event count —
    /// it asserts emit and persistence counts are equal and both
    /// non-empty, which catches any future missing-match-arm
    /// regression.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_agent_emits_full_event_stream_and_persists_records() {
        // Guardrail: any future deadlock (runner hang, forwarder pump
        // never draining, Tauri listener never firing) must produce a
        // loud timeout rather than wedging CI. 60s is generous for a
        // fully stubbed scenario — the engine-side happy-path
        // equivalent finishes in ~50 ms.
        tokio::time::timeout(std::time::Duration::from_secs(60), run_smoke_test_body())
            .await
            .expect("smoke test must finish within 60s (deadlock / hang regression)");
    }

    async fn run_smoke_test_body() {
        // ── Arrange: mock Tauri AppHandle + per-topic capture ──────
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();

        // `listen_any` subscribes on a specific topic; collecting to
        // a shared Vec gives us a post-run view of every forwarded
        // event. The GoalComplete + CompletionDisagreementResolved
        // variants intentionally do not show up here — those are
        // emitted by the run-agent task itself, not by this
        // forwarder.
        let captured: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        for topic in AGENT_TOPICS {
            let topic = topic.to_string();
            let captured = Arc::clone(&captured);
            handle.listen_any(topic.clone(), move |evt| {
                captured
                    .lock()
                    .unwrap()
                    .push((topic.clone(), evt.payload().to_string()));
            });
        }

        // ── Arrange: scripted LLM + MCP stubs ──────────────────────
        // Two tool calls then agent_done. `cdp_find_elements` returns
        // an empty matches set, mirroring the stable fixture in the
        // engine-side end-to-end happy-path test.
        let llm = ScriptedLlm::new(vec![
            llm_reply_tool(
                "cdp_find_elements",
                serde_json::json!({"query": "", "max_results": 300}),
            ),
            llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
            llm_reply_tool(
                "agent_done",
                serde_json::json!({"summary": "rubric-10 smoke test"}),
            ),
        ]);
        let mcp = StaticMcp::with_tools(&["cdp_find_elements", "cdp_click"])
            .with_reply(
                "cdp_find_elements",
                r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
            )
            .with_reply("cdp_click", "clicked");

        // ── Arrange: real RunStorage rooted at a tempdir ───────────
        let tmp = tempfile::tempdir().expect("tempdir");
        let workflow_name = "rubric-10-smoke";
        let mut storage_inner =
            clickweave_core::storage::RunStorage::new(tmp.path(), workflow_name);
        let exec_dir = storage_inner.begin_execution().expect("begin_execution");
        let events_path = tmp
            .path()
            .join(".clickweave")
            .join("runs")
            .join(workflow_name)
            .join(&exec_dir)
            .join("events.jsonl");
        let storage = Arc::new(Mutex::new(storage_inner));

        // ── Arrange: engine event channel + Tauri-forwarder pump ───
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<RunnerOutput>(64);
        let (approval_tx, _approval_rx) =
            tokio::sync::mpsc::channel::<(ApprovalRequest, tokio::sync::oneshot::Sender<bool>)>(1);
        let channels = AgentChannels {
            event_tx,
            approval_tx,
        };

        let run_id = uuid::Uuid::new_v4().to_string();
        let run_uuid: uuid::Uuid = run_id.parse().unwrap();

        // Forwarder pump: mirrors the production agent.rs body —
        // persist to `events.jsonl`, then call
        // `forward_agent_event`. Count forwarded events here so the
        // assertion does not depend on listener-dispatch latency.
        let forwarder_handle = handle.clone();
        let forwarder_run_id = run_id.clone();
        let forwarder_storage = Arc::clone(&storage);
        let forwarded: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let forwarded_for_task = Arc::clone(&forwarded);
        let forwarder_task = tokio::spawn(async move {
            while let Some(output) = event_rx.recv().await {
                let Some(event) = output.into_event() else {
                    continue;
                };
                let _ = forwarder_storage.lock().unwrap().append_agent_event(&event);
                forward_agent_event(&forwarder_handle, &forwarder_run_id, &event);
                forwarded_for_task.lock().unwrap().push(event);
            }
        });

        // ── Act: drive the engine ──────────────────────────────────
        let (state, _writer_tx) = run_agent_workflow(
            &llm,
            AgentConfig::default(),
            "rubric-10 gate: forwarder + persistence contract".to_string(),
            &mcp,
            Some(channels),
            None,
            // Permission policy: `allow_all` so scripted destructive-ish
            // tool calls (cdp_click) don't block waiting on an approval
            // oneshot that nothing in this test answers. The production
            // agent.rs threads the operator's policy from the UI; this
            // smoke test only cares about event forwarding, so the
            // simplest shape that bypasses the approval gate is enough.
            Some(PermissionPolicy {
                allow_all: true,
                ..PermissionPolicy::default()
            }),
            run_uuid,
            None,
            None,
            Some(Arc::clone(&storage)),
            None,
            None,
        )
        .await
        .expect("run_agent_workflow ok");

        // Wait for the forwarder pump to drain (`event_tx` was dropped
        // when the workflow returned, so the recv loop exits cleanly).
        forwarder_task.await.expect("forwarder joined");

        // Give the Tauri listener task a scheduling window so the
        // per-topic capture vector observes every emit.
        tokio::task::yield_now().await;

        // ── Assert: legacy AgentState wire-shape ───────────────────
        assert_eq!(
            state.steps.len(),
            2,
            "scripted tool-call count (2) must match state.steps.len(); got {:?}",
            state.steps,
        );
        assert!(
            matches!(
                state.terminal_reason,
                Some(TerminalReason::Completed { ref summary })
                    if summary == "rubric-10 smoke test"
            ),
            "terminal_reason must be Completed with the agent_done summary, got {:?}",
            state.terminal_reason,
        );
        assert!(
            state.completed,
            "state.completed must be true after agent_done terminal",
        );

        // ── Assert: forwarder touched every engine event ───────────
        let forwarded_events = forwarded.lock().unwrap();
        let forwarded_count = forwarded_events.len();
        assert!(
            forwarded_count > 0,
            "the forwarder must receive at least one AgentEvent from the engine",
        );

        // ── Assert: events.jsonl holds every forwarded event ───────
        // `events.jsonl` also contains StepRecord boundary writes
        // (Task 3a.6.5) and `AgentEvent::BoundaryRecordWritten`
        // AgentEvents (Task 3.4). Both shapes carry `boundary_kind`,
        // but only `AgentEvent` lines carry `serde(tag = "type")` —
        // filter on `type` presence so the count comparison is
        // apples-to-apples against the forwarded-event stream.
        let trace_raw = std::fs::read_to_string(&events_path)
            .unwrap_or_else(|e| panic!("read events.jsonl at {:?}: {}", events_path, e));
        let trace_json: Vec<serde_json::Value> = trace_raw
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).expect("events.jsonl line is valid JSON"))
            .collect();
        let agent_event_lines: Vec<&serde_json::Value> = trace_json
            .iter()
            .filter(|v| v.get("type").is_some())
            .collect();
        assert_eq!(
            agent_event_lines.len(),
            forwarded_count,
            "events.jsonl AgentEvent line count ({}) must equal forwarded-event \
             count ({}); trace_raw={}",
            agent_event_lines.len(),
            forwarded_count,
            trace_raw,
        );

        // ── Assert: every forwarded event reached `agent://*` ──────
        // `GoalComplete` and `CompletionDisagreementResolved` are the
        // two variants `forward_agent_event` deliberately swallows
        // (terminal emission / Tauri-only origin), so subtract those
        // from the expected capture count. Every other forwarded
        // variant must produce exactly one `agent://<topic>` payload.
        let forwarder_silenced = forwarded_events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AgentEvent::GoalComplete { .. }
                        | AgentEvent::CompletionDisagreementResolved { .. }
                )
            })
            .count();
        let expected_emits = forwarded_count - forwarder_silenced;
        let captured_events = captured.lock().unwrap();
        assert_eq!(
            captured_events.len(),
            expected_emits,
            "every forwarded AgentEvent (minus GoalComplete / \
             CompletionDisagreementResolved) must produce exactly one \
             `agent://<topic>` emission — forwarded={}, silenced={}, \
             captured={:?}",
            forwarded_count,
            forwarder_silenced,
            captured_events,
        );

        // ── Assert: the run emitted a concrete `agent://step` ──────
        // A successful scripted scenario must pass through at least
        // one `StepCompleted` — that's the canonical user-visible
        // event the UI renders per step.
        assert!(
            captured_events
                .iter()
                .any(|(topic, _)| topic == "agent://step"),
            "at least one `agent://step` emission expected; captured={:?}",
            captured_events,
        );

        // Sanity: every captured event payload carries the run_id we
        // seeded. This pins the `event_run_id.clone()` pass-through
        // behaviour in `forward_agent_event` — a regression there
        // would silently strip the id from frontend-visible payloads.
        for (topic, payload) in captured_events.iter() {
            let parsed: serde_json::Value = serde_json::from_str(payload)
                .unwrap_or_else(|e| panic!("payload on {} is valid JSON: {}", topic, e));
            assert_eq!(
                parsed.get("run_id").and_then(|v| v.as_str()),
                Some(run_id.as_str()),
                "every `agent://*` payload must carry run_id={}; topic={}, payload={}",
                run_id,
                topic,
                payload,
            );
        }
    }

    /// F2 acceptance test: pin the exact top-level JSON keys for the
    /// three Spec 2 D33 episodic events. The locked contract lives at
    /// `docs/design/2026-04-24_agent-episodic-memory.md:699-701`. A
    /// future drift on either the engine event variant fields or the
    /// `forward_agent_event` payload shape must fail this test loud.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn forward_agent_event_emits_locked_episodic_payload_shapes() {
        use clickweave_engine::agent::ScopeBreakdown;
        use clickweave_engine::agent::episodic::{EpisodeScope, RetrievalTrigger};
        use std::collections::BTreeSet;
        use tauri::Listener;

        let app = tauri::test::mock_app();
        let handle = app.handle().clone();

        let captured: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        for topic in [
            "agent://episodes_retrieved",
            "agent://episode_written",
            "agent://episode_promoted",
        ] {
            let captured = Arc::clone(&captured);
            handle.listen_any(topic, move |evt| {
                captured
                    .lock()
                    .unwrap()
                    .push((topic.to_string(), evt.payload().to_string()));
            });
        }

        let run_id = uuid::Uuid::new_v4();
        let run_id_str = run_id.to_string();

        let retrieved = AgentEvent::EpisodesRetrieved {
            run_id,
            trigger: RetrievalTrigger::RunStart,
            count: 2,
            episode_ids: vec!["ep_a".into(), "ep_b".into()],
            scope_breakdown: ScopeBreakdown {
                workflow: 1,
                global: 1,
            },
        };
        let written = AgentEvent::EpisodeWritten {
            run_id,
            outcome: "inserted".into(),
            episode_id: "ep_c".into(),
            scope: EpisodeScope::WorkflowLocal,
            occurrence_count: 1,
        };
        let promoted = AgentEvent::EpisodePromoted {
            run_id,
            promoted_episode_ids: vec!["ep_d".into()],
            skipped_count: 3,
        };

        forward_agent_event(&handle, &run_id_str, &retrieved);
        forward_agent_event(&handle, &run_id_str, &written);
        forward_agent_event(&handle, &run_id_str, &promoted);

        // Yield so listener tasks pick up the emits.
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }

        let captured = captured.lock().unwrap();
        let by_topic = |t: &str| -> serde_json::Value {
            let raw = captured
                .iter()
                .find(|(topic, _)| topic == t)
                .unwrap_or_else(|| panic!("no emission on {} — captured={:?}", t, captured))
                .1
                .clone();
            serde_json::from_str(&raw).expect("payload is valid JSON")
        };
        let key_set = |v: &serde_json::Value| -> BTreeSet<String> {
            v.as_object()
                .expect("payload is an object")
                .keys()
                .cloned()
                .collect()
        };
        let expect_keys = |actual: BTreeSet<String>, want: &[&str], topic: &str| {
            let want_set: BTreeSet<String> = want.iter().map(|s| (*s).to_string()).collect();
            assert_eq!(
                actual, want_set,
                "{} payload must carry exactly the locked Spec 2 D33 keys",
                topic,
            );
        };

        let r = by_topic("agent://episodes_retrieved");
        // `event_run_id` is the harness-added forwarder echo of the
        // engine-side `run_id`; the spec contract is on the engine
        // payload's keys (which are the *other* fields). Both must be
        // present in the emit per the existing forwarder pattern.
        expect_keys(
            key_set(&r),
            &[
                "run_id",
                "event_run_id",
                "trigger",
                "count",
                "episode_ids",
                "scope_breakdown",
            ],
            "episodes_retrieved",
        );
        let breakdown = r.get("scope_breakdown").expect("scope_breakdown present");
        let breakdown_keys: BTreeSet<String> = breakdown
            .as_object()
            .expect("scope_breakdown is an object")
            .keys()
            .cloned()
            .collect();
        expect_keys(
            breakdown_keys,
            &["workflow", "global"],
            "episodes_retrieved.scope_breakdown",
        );

        let w = by_topic("agent://episode_written");
        expect_keys(
            key_set(&w),
            &[
                "run_id",
                "event_run_id",
                "outcome",
                "episode_id",
                "scope",
                "occurrence_count",
            ],
            "episode_written",
        );

        let p = by_topic("agent://episode_promoted");
        expect_keys(
            key_set(&p),
            &[
                "run_id",
                "event_run_id",
                "promoted_episode_ids",
                "skipped_count",
            ],
            "episode_promoted",
        );
        // `promoted_episode_ids` must carry the actual IDs, not be
        // collapsed to a count.
        let ids = p
            .get("promoted_episode_ids")
            .and_then(|v| v.as_array())
            .expect("promoted_episode_ids is an array");
        assert_eq!(
            ids.len(),
            1,
            "exactly one promoted ID expected from synthetic event",
        );
        assert_eq!(ids[0].as_str(), Some("ep_d"));
    }
}
