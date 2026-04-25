use super::error::CommandError;
use super::types::*;
use clickweave_core::variant_index::{VariantEntry, VariantIndex};
use clickweave_engine::agent::{
    AgentCache, AgentChannels, AgentConfig, AgentEvent, ApprovalRequest,
    DisagreementResolutionAction, PermissionAction, PermissionPolicy, PermissionRule,
    TerminalReason,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use tokio_util::sync::CancellationToken;

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
    /// suppresses every `focus_window` call with a synthetic skip,
    /// regardless of app kind or CDP state — useful for background-run
    /// policies. `None` leaves the engine default (`true`) in place.
    #[serde(default)]
    pub allow_focus_window: Option<bool>,
    /// Privacy kill switch: when false, the run is entirely in-memory.
    /// No `.clickweave/runs/` directory is created and no trace files
    /// or cache files are written. When `None`, persistence is on —
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
        } => {
            let _ = app.emit(
                "agent://boundary_record_written",
                serde_json::json!({
                    "run_id": run_id,
                    "event_run_id": event_run_id,
                    "boundary_kind": boundary_kind,
                    "step_index": step_index,
                }),
            );
        }
        // TODO(Phase 4): emit to frontend as `agent://episode_written` /
        // `agent://episode_promoted`. Phase 3 only adds the engine-side
        // emission; the Tauri fan-out + frontend store wiring lands in
        // Phase 4.
        AgentEvent::EpisodeWritten { .. } => {
            tracing::trace!(?event, "episodic event awaiting Phase 4 fan-out");
        }
        AgentEvent::EpisodePromoted { .. } => {
            tracing::trace!(?event, "episodic event awaiting Phase 4 fan-out");
        }
    }
}

// ── Commands ────────────────────────────────────────────────────

#[tauri::command]
#[specta::specta]
pub async fn run_agent(
    app: tauri::AppHandle,
    request: AgentRunRequest,
) -> Result<(), CommandError> {
    {
        let handle = app.state::<Mutex<AgentHandle>>();
        let guard = handle.lock().unwrap();
        if guard.cancel_token.is_some() || guard.task_handle.is_some() {
            return Err(CommandError::already_running());
        }
    }

    let agent_config = request.agent.into_llm_config(None);
    let mcp_binary_path =
        crate::mcp_resolve::resolve_mcp_binary().map_err(|e| CommandError::mcp(format!("{e}")))?;

    let workflow_id: uuid::Uuid = request
        .workflow_id
        .parse()
        .map_err(|_| CommandError::validation("Invalid workflow ID"))?;

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
    let run_id = request
        .run_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let run_uuid: uuid::Uuid = run_id
        .parse()
        .map_err(|_| CommandError::validation("Invalid run_id"))?;

    let anchor_uuid: Option<uuid::Uuid> = match request.anchor_node_id.as_deref() {
        Some(s) if !s.is_empty() => Some(
            s.parse()
                .map_err(|_| CommandError::validation("Invalid anchor_node_id"))?,
        ),
        _ => None,
    };

    let prior_turns: Vec<clickweave_engine::agent::PriorTurn> = request
        .prior_turns
        .iter()
        .map(|t| {
            let run_id: uuid::Uuid = t
                .run_id
                .parse()
                .map_err(|_| CommandError::validation("Invalid prior_turn.run_id"))?;
            Ok::<_, CommandError>(clickweave_engine::agent::PriorTurn {
                goal: t.goal.clone(),
                summary: t.summary.clone(),
                run_id,
            })
        })
        .collect::<Result<_, CommandError>>()?;

    let permission_policy: Option<PermissionPolicy> = request.permissions.map(Into::into);
    let consecutive_destructive_cap = request.consecutive_destructive_cap;
    let allow_focus_window = request.allow_focus_window;

    let cancel_token = CancellationToken::new();
    let agent_token = cancel_token.clone();
    let forwarder_token = cancel_token.clone();
    let event_forwarder_token = cancel_token.clone();

    // Live event channel: agent runner -> Tauri event emitter
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);

    // Approval channel: agent runner sends requests, we forward to UI and store
    // the oneshot response sender in the handle for `approve_agent_action` to use.
    let (approval_tx, mut approval_rx) =
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
    let approval_run_id = run_id.clone();

    // Channels used to signal cleanup when the agent task, event forwarder,
    // and approval forwarder have all finished, preventing stale event leakage.
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let (events_done_tx, events_done_rx) = tokio::sync::oneshot::channel::<()>();
    let (approval_done_tx, approval_done_rx) = tokio::sync::oneshot::channel::<()>();

    // Install cancel_token and run_id before spawning so stop_agent() works
    // even during the spawn window (before task_handle is available).
    {
        let handle = app.state::<Mutex<AgentHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.cancel_token = Some(cancel_token);
        guard.run_id = Some(run_id.clone());
    }

    // Emit agent://started so the frontend knows the run_id before any other events.
    let _ = app.emit("agent://started", serde_json::json!({ "run_id": &run_id }));

    let task_handle = tauri::async_runtime::spawn(async move {
        // Spawn MCP server — cancellation-aware so stop_agent() works
        // even during slow MCP startup / handshake.
        let mcp = tokio::select! {
            res = clickweave_mcp::McpClient::spawn(&mcp_binary_path, &[]) => {
                match res {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = emit_handle.emit(
                            "agent://error",
                            serde_json::json!({ "run_id": task_run_id, "message": format!("MCP spawn failed: {e}") }),
                        );
                        let _ = done_tx.send(());
                        return;
                    }
                }
            }
            _ = agent_token.cancelled() => {
                let _ = emit_handle.emit(
                    "agent://stopped",
                    serde_json::json!({ "run_id": task_run_id, "reason": "cancelled" }),
                );
                let _ = done_tx.send(());
                return;
            }
        };

        // Thinking is explicitly enabled: small agent models need a
        // reasoning pass to avoid pattern-matching salient literals from the
        // goal text into tool arguments.
        let llm = clickweave_llm::LlmClient::new(agent_config.clone().with_thinking(false));
        // Vision backend: reuse the agent endpoint (the user already has this
        // configured) with thinking disabled and a low token budget — the
        // post-done check only needs to emit YES/NO + a sentence. If the
        // endpoint cannot process images, the VLM call errors and the loop
        // falls through to normal completion instead of tanking the run.
        // D-PR1: StateRunner stores vision as `Arc<dyn DynChatBackend>` so
        // the primary backend and VLM can be different concrete types.
        // Wrap at the Tauri seam; the blanket `impl<B: ChatBackend + Send
        // + Sync> DynChatBackend for B` lets any concrete ChatBackend flow
        // through `Arc::new(...)` without further plumbing.
        let vision: std::sync::Arc<dyn clickweave_llm::DynChatBackend> = std::sync::Arc::new(
            clickweave_llm::LlmClient::new(agent_config.with_thinking(false).with_max_tokens(512)),
        );
        let mut config = AgentConfig::default();
        if let Some(cap) = consecutive_destructive_cap {
            config.consecutive_destructive_cap = cap;
        }
        if let Some(allow) = allow_focus_window {
            config.allow_focus_window = allow;
        }

        // Begin storage execution and load cross-run state under a single lock.
        // Storage init failure prevents the run from starting — durable tracing
        // must be available before executing any agent actions.
        let storage = task_storage;
        let (variant_context, cache, verification_artifacts_dir) = {
            let mut guard = storage.lock().unwrap();
            match guard.begin_execution() {
                Ok(_) => {}
                Err(e) => {
                    let _ = emit_handle.emit(
                        "agent://error",
                        serde_json::json!({
                            "run_id": task_run_id,
                            "message": format!("Run storage init failed: {e}"),
                        }),
                    );
                    let _ = done_tx.send(());
                    return;
                }
            }
            // Load via `load_existing` so entries whose execution dir
            // is gone (retention sweep, crash, manual cleanup) never
            // leak back into agent context — even if the on-disk
            // JSONL still carries them. This enforces the privacy
            // contract at read time so it is robust to races, partial
            // failures, and hand-cleanup.
            let variant_index =
                VariantIndex::load_existing(&guard.variant_index_path(), guard.base_path());
            let cache = AgentCache::load_from_path(&guard.agent_cache_path());
            let verification_artifacts_dir = guard.execution_artifacts_dir();
            (
                variant_index.as_context_text(),
                cache,
                verification_artifacts_dir,
            )
        };

        let channels = AgentChannels {
            event_tx,
            approval_tx,
        };

        // D18 (Task 3.5): compose prior-turn log + variant context +
        // the user's goal into a single goal_block string. The engine
        // feeds this into `messages[1]`, leaving `messages[0]` (the
        // system prompt) stable across runs for prompt-cache hits.
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

        // Run the agent loop
        let result = tokio::select! {
            res = clickweave_engine::agent::run_agent_workflow(
                &llm,
                config,
                goal_block,
                &mcp,
                Some(cache),
                Some(channels),
                Some(vision.clone()),
                // Permission policy is threaded from the UI via the
                // `run_agent` request; None means "use the default
                // (empty-rules, allow_all=false, guardrail off)" which
                // reproduces the Phase-1 behaviour.
                permission_policy.clone(),
                run_uuid,
                anchor_uuid,
                verification_artifacts_dir,
                Some(storage.clone()),
            ) => res,
            _ = agent_token.cancelled() => {
                let _ = emit_handle.emit(
                    "agent://stopped",
                    serde_json::json!({ "run_id": task_run_id, "reason": "cancelled" }),
                );
                let _ = done_tx.send(());
                return;
            }
        };

        match result {
            Ok((state, updated_cache)) => {
                // Persist the updated cache — skipped when the privacy
                // kill switch is off so the workflow-level cache file
                // stays as it was before the run.
                if persist_traces {
                    let _ = updated_cache.save_to_path(&storage.lock().unwrap().agent_cache_path());
                }

                // If the engine halted on a pending VLM disagreement, block
                // here until the operator resolves it (confirm / cancel) via
                // `resolve_completion_disagreement`, or until `stop_agent`
                // fires `force_stop` which resolves the oneshot as Cancel.
                // This keeps the Tauri task alive during adjudication so
                // the variant-index + events.jsonl writes happen exactly
                // once per run, against the final operator decision.
                let resolved_terminal = match state.terminal_reason {
                    Some(TerminalReason::CompletionDisagreement {
                        agent_summary,
                        vlm_reasoning,
                    }) => {
                        await_disagreement_resolution(
                            &emit_handle,
                            &agent_token,
                            &storage,
                            &task_run_id,
                            agent_summary,
                            vlm_reasoning,
                        )
                        .await
                    }
                    other => other,
                };

                // Derive variant metadata from the resolved terminal reason.
                let (divergence_summary, success) = match &resolved_terminal {
                    Some(reason) => (reason.divergence_summary(), reason.is_completed()),
                    None => ("Stopped: unknown reason".to_string(), false),
                };

                // Write the variant-index entry for every resolved run —
                // including the post-resolution disagreement paths. The
                // only case we must not write is when the operator's
                // decision is still pending, but by this point the
                // `await_disagreement_resolution` call has already
                // collapsed that state into a concrete terminal reason
                // (or a cancellation below).
                //
                // Skip the write when the privacy kill switch is off so
                // no per-run metadata is appended to the workflow-level
                // variant index file.
                if persist_traces {
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

                // Emit the truthful terminal event. If no resolved
                // terminal is available (force_stop fired during the
                // adjudication window *before* the oneshot was installed
                // — a race that `await_disagreement_resolution` already
                // handles) we fall back to `agent://stopped { reason:
                // cancelled }`.
                match &resolved_terminal {
                    Some(TerminalReason::Completed { summary })
                    | Some(TerminalReason::DisagreementConfirmed {
                        agent_summary: summary,
                    }) => {
                        let _ = emit_handle.emit(
                            "agent://complete",
                            serde_json::json!({ "run_id": task_run_id, "summary": summary }),
                        );
                    }
                    Some(TerminalReason::DisagreementCancelled { .. }) => {
                        let _ = emit_handle.emit(
                            "agent://stopped",
                            serde_json::json!({
                                "run_id": task_run_id,
                                "reason": "user_cancelled_disagreement",
                            }),
                        );
                    }
                    Some(reason) => {
                        let mut payload = serde_json::to_value(reason).unwrap_or_default();
                        if let Some(obj) = payload.as_object_mut() {
                            obj.insert("run_id".to_string(), serde_json::json!(task_run_id));
                        }
                        let _ = emit_handle.emit("agent://stopped", payload);
                    }
                    None => {
                        let _ = emit_handle.emit(
                            "agent://stopped",
                            serde_json::json!({ "run_id": task_run_id, "reason": "cancelled" }),
                        );
                    }
                }
            }
            Err(e) => {
                let _ = emit_handle.emit(
                    "agent://error",
                    serde_json::json!({ "run_id": task_run_id, "message": format!("{e}") }),
                );
            }
        }

        let _ = done_tx.send(());
    });

    // Spawn a task to forward live agent events to the Tauri frontend.
    // Cancellation-aware: stops accepting new events once the run is cancelled,
    // then drains any remaining buffered events and signals completion.
    // Persistence is synchronous within the forwarder to guarantee ordering
    // and completeness in events.jsonl. The agent loop emits events at LLM
    // pace (~seconds per step), so the I/O cost is negligible.
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = event_forwarder_token.cancelled() => {
                    // Drain remaining buffered events before exiting
                    while let Ok(event) = event_rx.try_recv() {
                        let _ = event_storage.lock().unwrap().append_agent_event(&event);
                    }
                    break;
                }
                maybe_event = event_rx.recv() => {
                    match maybe_event {
                        Some(event) => {
                            // Durable trace: persist every event to events.jsonl
                            let _ = event_storage.lock().unwrap().append_agent_event(&event);
                            // Fan out to the matching `agent://*` Tauri topic.
                            // See `forward_agent_event` for the full variant
                            // mapping — extracted so the run-agent smoke test
                            // can drive it against a mock `AppHandle`.
                            forward_agent_event(&event_emit_handle, &event_run_id, &event);
                        }
                        None => break,
                    }
                }
            }
        }
        let _ = events_done_tx.send(());
    });

    // Spawn a task to forward approval requests to the Tauri frontend
    // and store the oneshot response sender in the handle.
    // Cancellation-aware so it stops when force_stop() fires, preventing
    // stale approvals from leaking into a subsequent run.
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                req = approval_rx.recv() => {
                    match req {
                        Some((request, resp_tx)) => {
                            // After winning the select race, re-check cancellation
                            // to avoid leaking a stale approval into the next run.
                            // Send explicit rejection so the engine sees Ok(false)
                            // instead of Err (channel closed → ApprovalUnavailable).
                            if forwarder_token.is_cancelled() {
                                let _ = resp_tx.send(false);
                                break;
                            }
                            {
                                let handle = approval_emit_handle.state::<Mutex<AgentHandle>>();
                                let mut guard = handle.lock().unwrap();
                                guard.pending_approval_tx = Some(resp_tx);
                            }
                            let _ = approval_emit_handle.emit(
                                "agent://approval_required",
                                serde_json::json!({
                                    "run_id": approval_run_id,
                                    "step_index": request.step_index,
                                    "tool_name": request.tool_name,
                                    "arguments": request.arguments,
                                    "description": request.description,
                                }),
                            );
                        }
                        None => break,
                    }
                }
                _ = forwarder_token.cancelled() => {
                    // Drain any queued approval requests, sending rejection
                    // so the engine sees Ok(false) instead of a channel drop.
                    while let Ok((_req, resp_tx)) = approval_rx.try_recv() {
                        let _ = resp_tx.send(false);
                    }
                    break;
                }
            }
        }
        let _ = approval_done_tx.send(());
    });

    // Store task_handle now that it's available (cancel_token + run_id
    // were already installed before spawn).
    {
        let handle = app.state::<Mutex<AgentHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.task_handle = Some(task_handle);
    }

    // Spawn cleanup task: wait for the agent task, event forwarder, and
    // approval forwarder to all complete before clearing the handle. This
    // prevents stale buffered events or approvals from leaking into a
    // subsequent run.
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
    ];

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
        // an empty matches set so no cache replay interferes with the
        // step count (mirrors the stable fixture in the engine-side
        // end-to-end happy-path test).
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
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
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
            while let Some(event) = event_rx.recv().await {
                let _ = forwarder_storage.lock().unwrap().append_agent_event(&event);
                forward_agent_event(&forwarder_handle, &forwarder_run_id, &event);
                forwarded_for_task.lock().unwrap().push(event);
            }
        });

        // ── Act: drive the engine ──────────────────────────────────
        let (state, _cache) = run_agent_workflow(
            &llm,
            AgentConfig::default(),
            "rubric-10 gate: forwarder + persistence contract".to_string(),
            &mcp,
            None,
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
}
