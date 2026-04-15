mod cache;
mod context;
mod loop_runner;
mod prompt;
mod recovery;
mod transition;
mod types;

pub use loop_runner::{AgentRunner, ApprovalGate};
pub use types::*;

use clickweave_llm::ChatBackend;
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
/// Returns both the final agent state and the (possibly updated) cache.
pub async fn run_agent_workflow<B: ChatBackend>(
    llm: &B,
    config: AgentConfig,
    goal: String,
    mcp: &McpClient,
    variant_context: Option<&str>,
    cache: Option<AgentCache>,
    channels: Option<AgentChannels>,
    vision: Option<&B>,
) -> anyhow::Result<(AgentState, AgentCache)> {
    let tools = mcp.tools_as_openai();
    let workflow = clickweave_core::Workflow::default();
    let mut runner = match cache {
        Some(c) => AgentRunner::with_cache(llm, config, c),
        None => AgentRunner::new(llm, config),
    };
    if let Some(ch) = channels {
        runner = runner
            .with_events(ch.event_tx)
            .with_approval(ch.approval_tx);
    }
    if let Some(v) = vision {
        runner = runner.with_vision(v);
    }
    let state = runner
        .run(goal, workflow, mcp, variant_context, tools)
        .await?;
    Ok((state, runner.into_cache()))
}

#[cfg(test)]
mod tests;
