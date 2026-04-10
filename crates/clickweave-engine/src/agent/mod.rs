mod cache;
mod context;
mod loop_runner;
mod prompt;
mod recovery;
mod transition;
mod types;

pub use loop_runner::AgentRunner;
pub use types::*;

use clickweave_llm::ChatBackend;
use clickweave_mcp::McpClient;

/// Public entry point for running the agent loop from outside the engine crate.
///
/// This wraps `AgentRunner::run` and resolves the `pub(crate)` Mcp trait
/// boundary so that callers (e.g. Tauri commands) can pass a `McpClient`
/// directly.
pub async fn run_agent_workflow(
    llm: &impl ChatBackend,
    config: AgentConfig,
    goal: String,
    mcp: &McpClient,
    variant_context: Option<&str>,
) -> anyhow::Result<AgentState> {
    let tools = mcp.tools_as_openai();
    let workflow = clickweave_core::Workflow::default();
    let mut runner = AgentRunner::new(llm, config);
    runner
        .run(goal, workflow, mcp, variant_context, tools)
        .await
}

#[cfg(test)]
mod tests;
