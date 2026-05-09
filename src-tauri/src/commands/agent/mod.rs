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
    pub project_name: String,
    pub project_id: String,
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
mod commands;
mod disagreement;
mod events;
mod forwarder;
mod setup;
mod skill_proposal;
#[cfg(test)]
mod smoke_tests;
mod task;
#[cfg(test)]
mod tests;

pub use commands::{approve_agent_action, resolve_completion_disagreement, run_agent, stop_agent};

use disagreement::await_disagreement_resolution;
use events::forward_agent_event;
use forwarder::{spawn_agent_cleanup, spawn_agent_event_forwarder, spawn_approval_forwarder};
use setup::*;
use skill_proposal::{
    emit_after_agent_event_drain, maybe_spawn_skill_proposal_task, spawn_skill_proposal_task,
};
use task::*;
