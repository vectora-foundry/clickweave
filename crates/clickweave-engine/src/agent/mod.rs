mod approval;
mod cache;
mod completion_check;
mod context;
mod context_spine;
mod loop_runner;
pub mod permissions;
mod phase;
pub mod prior_turns;
mod prompt;
mod prompt_spine;
mod recovery;
mod render;
mod runner;
mod step_record;
mod task_state;
mod transition;
mod types;
mod world_model;

pub use loop_runner::AgentRunner;
// `ApprovalGate` lives in `approval` so the state-spine runner can own it
// without a cyclic dep on `loop_runner`. Re-exported under the same path
// so existing call sites keep working until Phase 3b deletes the legacy
// runner.
pub use approval::ApprovalGate;
pub use permissions::{PermissionAction, PermissionPolicy, PermissionRule, ToolAnnotations};
pub use prior_turns::PriorTurn;
pub use prompt::truncate_summary;
pub use runner::{AgentAction, AgentTurn, StateRunner, ToolExecutor, TurnOutcome};
pub use types::*;

use std::path::PathBuf;
use std::sync::Arc;

use clickweave_llm::{ChatBackend, DynChatBackend};
use clickweave_mcp::McpClient;

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
/// This wraps `AgentRunner::run` and resolves the `pub(crate)` Mcp trait
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
pub async fn run_agent_workflow<B: ChatBackend>(
    llm: &B,
    config: AgentConfig,
    goal: String,
    mcp: &McpClient,
    variant_context: Option<&str>,
    cache: Option<AgentCache>,
    channels: Option<AgentChannels>,
    vision: Option<Arc<dyn DynChatBackend>>,
    permissions: Option<PermissionPolicy>,
    run_id: uuid::Uuid,
    anchor_node_id: Option<uuid::Uuid>,
    prior_turns: Vec<prior_turns::PriorTurn>,
    verification_artifacts_dir: Option<PathBuf>,
    storage: Option<RunStorageHandle>,
) -> anyhow::Result<(AgentState, AgentCache)> {
    // Task 3a.1 pivots this wrapper off the legacy `AgentRunner` onto
    // `StateRunner`. The legacy runner stays alive in `loop_runner.rs`
    // (the ~95 legacy integration tests still drive it directly); only
    // this public entry point switches over. The `vision` parameter
    // shape follows D-PR1: `Arc<dyn DynChatBackend>` so primary and
    // VLM can be different concrete backend types without pushing a
    // second generic through the Tauri command surface.
    let tools = mcp.tools_as_openai();
    let workflow = clickweave_core::Workflow::default();
    let mut runner = StateRunner::new(goal.clone(), config);
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
    runner
        .run(
            llm,
            mcp,
            goal,
            workflow,
            variant_context,
            tools,
            anchor_node_id,
            &prior_turns,
        )
        .await
}

#[cfg(test)]
mod tests;
