mod approval;
mod cache;
mod completion_check;
mod context;
pub mod episodic;
pub mod permissions;
// Phase is part of `TaskState`'s public surface (used to construct
// `task_state_at_entry` snapshots in the episodic memory layer's
// integration tests), so the module surfaces as `pub mod`.
pub mod phase;
pub mod prior_turns;
mod prompt;
mod recovery;
mod render;
mod runner;
// Phase 2 (episodic memory) integration tests construct
// `WorldModelSnapshot`, `StepRecord`, and `TaskState` values from
// outside the crate, so these modules surface as `pub mod`. The
// underlying types already declared `#[allow(dead_code)]` while their
// runtime consumers are wired up in later phases; making the modules
// public does not change that contract — it only lets external test
// code build fixture rows for the episodic store round-trip.
pub mod step_record;
pub mod task_state;
mod transition;
mod types;
pub mod world_model;

// `ApprovalGate` lives in `approval` so the state-spine runner can own it
// without a cyclic dep on the legacy runner. Phase 3b deleted the legacy
// runner; this re-export keeps external callers pointed at a stable path.
pub use approval::ApprovalGate;
pub use permissions::{PermissionAction, PermissionPolicy, PermissionRule, ToolAnnotations};
pub use prior_turns::{PriorTurn, build_goal_block};
pub use prompt::truncate_summary;
pub use runner::{AgentAction, AgentTurn, StateRunner, ToolExecutor, TurnOutcome};
pub use types::*;

use std::path::PathBuf;
use std::sync::Arc;

use clickweave_llm::{ChatBackend, DynChatBackend};

use crate::executor::Mcp;

/// Channels that can be attached to the agent runner for live feedback.
pub struct AgentChannels {
    /// Live event emission channel.
    pub event_tx: tokio::sync::mpsc::Sender<AgentEvent>,
    /// Approval request channel (each request comes with a oneshot response sender).
    pub approval_tx:
        tokio::sync::mpsc::Sender<(ApprovalRequest, tokio::sync::oneshot::Sender<bool>)>,
}

/// Public entry point for running the agent loop from outside the engine crate.
///
/// This wraps `StateRunner::run` and resolves the `pub(crate)` Mcp trait
/// boundary so that callers (e.g. Tauri commands) can pass a `McpClient`
/// directly.
///
/// When `cache` is `Some`, the runner is seeded with cross-run decisions.
/// When `channels` is `Some`, the runner emits live events and waits for
/// approval before each tool execution.
/// When `vision` is `Some`, the runner verifies `agent_done` against a
/// fresh screenshot via the VLM and may halt with a disagreement event
/// when the VLM rejects completion.
/// When `permissions` is `Some`, the runner consults the policy for every
/// non-observation tool call — `Allow` skips approval, `Deny` hard-rejects,
/// `Ask` falls through to the existing approval prompt.
/// When `verification_artifacts_dir` is `Some`, the runner writes a PNG
/// screenshot and a JSON metadata file to that directory after every
/// `agent_done` VLM check — regardless of verdict — so every completion
/// check leaves forensic evidence on disk.
/// Returns both the final agent state and the (possibly updated) cache.
/// Shared `RunStorage` handle threaded from the Tauri command into the
/// engine. Phase 3a adds this as an explicit parameter so the new
/// `StateRunner` can write boundary `StepRecord`s through the same
/// storage the Tauri layer already owns (see Task 3a.6.5). The handle is
/// optional: when `None`, the runner runs storage-less (matches existing
/// integration tests).
pub type RunStorageHandle = std::sync::Arc<std::sync::Mutex<clickweave_core::storage::RunStorage>>;

#[allow(clippy::too_many_arguments)]
pub async fn run_agent_workflow<B, M>(
    llm: &B,
    config: AgentConfig,
    goal: String,
    mcp: &M,
    cache: Option<AgentCache>,
    channels: Option<AgentChannels>,
    vision: Option<Arc<dyn DynChatBackend>>,
    permissions: Option<PermissionPolicy>,
    run_id: uuid::Uuid,
    anchor_node_id: Option<uuid::Uuid>,
    verification_artifacts_dir: Option<PathBuf>,
    storage: Option<RunStorageHandle>,
    // Spec 2 episodic-memory wiring. `None` → `EpisodicContext::disabled()`,
    // preserving the legacy "no episodic" behaviour for tests and
    // internal callers that don't construct paths.
    episodic_ctx: Option<crate::agent::episodic::EpisodicContext>,
) -> anyhow::Result<(
    AgentState,
    AgentCache,
    // A clone of the runner-owned episodic writer's channel sender. The
    // Tauri caller enqueues `WriteRequest::PromotePass` on this sender
    // after `run` returns so that the single worker task — and its single
    // pair of SQLite connections — handles both `DeriveAndInsert` and
    // `PromotePass`. Dropping the sender signals the worker to exit after
    // draining. `None` when episodic is disabled for this run.
    Option<tokio::sync::mpsc::Sender<crate::agent::episodic::types::WriteRequest>>,
)>
where
    B: ChatBackend,
    M: Mcp + ?Sized,
{
    // Phase 3b Task 3.3 deleted the legacy runner; this entry point now
    // drives `StateRunner` directly. The `vision` parameter shape follows
    // D-PR1: `Arc<dyn DynChatBackend>` so primary and VLM can be different
    // concrete backend types without pushing a second generic through the
    // Tauri command surface.
    //
    // D18 (Task 3.5): variant context + prior-turn log are no longer
    // separate parameters. Callers compose them into `goal` via
    // `build_goal_block`, so the engine sees a single goal string
    // destined for `messages[1]`. The system prompt (`messages[0]`)
    // stays stable across runs for prefix-cache hits.
    let tools = mcp.tools_as_openai();
    let workflow = clickweave_core::Workflow::default();
    // P4: thread the per-run `EpisodicContext` through `new_with_episodic`
    // so the runner can open the workflow-local + global SQLite stores
    // before the loop starts. Callers that pass `None` get the disabled
    // context — episodic stays a no-op for that run.
    let episodic_ctx =
        episodic_ctx.unwrap_or_else(crate::agent::episodic::EpisodicContext::disabled);
    let mut runner = StateRunner::new_with_episodic(goal.clone(), config, episodic_ctx);
    if let Some(c) = cache {
        runner = runner.with_cache(c);
    }
    runner = runner.with_run_id(run_id);
    if let Some(ch) = channels {
        runner = runner
            .with_events(ch.event_tx)
            .with_approval(ch.approval_tx);
    }
    if let Some(v) = vision {
        runner = runner.with_vision(v);
    }
    if let Some(policy) = permissions {
        runner = runner.with_permissions(policy);
    }
    if let Some(dir) = verification_artifacts_dir {
        runner = runner.with_verification_artifacts_dir(dir);
    }
    if let Some(s) = storage {
        runner = runner.with_storage(s);
    }
    // Spawn the episodic writer last so it captures the live `event_tx`
    // and `run_id` already seeded by `with_events` / `with_run_id`. The
    // writer is a no-op when `episodic_active()` is false.
    runner = runner.with_episodic_writer();

    // Clone the writer's channel sender *before* `run` consumes the
    // runner. The clone shares the same worker task and SQLite connections
    // — no second connection is opened. The caller (Tauri command) holds
    // this sender to queue a run-terminal `PromotePass` on the same
    // writer that processed all `DeriveAndInsert` requests during the run,
    // eliminating the cross-connection visibility concern flagged in the
    // architectural gap review (R1.H1).
    let writer_tx = runner.writer_sender();

    let (state, cache) = runner
        .run(llm, mcp, goal, workflow, tools, anchor_node_id)
        .await?;
    Ok((state, cache, writer_tx))
}

/// Shared test doubles (`ScriptedLlm`, `StaticMcp`, `NullMcp`, `YesVlm`,
/// `NoVlm`, `llm_reply_tool`, …). Gated on `cfg(test)` for this crate's
/// own tests and on the `test-stubs` feature for downstream
/// `[dev-dependencies]` consumers (see `clickweave-tauri`'s run-agent
/// smoke test).
#[cfg(any(test, feature = "test-stubs"))]
pub mod test_stubs;

#[cfg(test)]
mod tests;
