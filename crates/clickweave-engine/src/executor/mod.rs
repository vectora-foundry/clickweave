//! Native skill executor surface.
//!
//! With the canvas-era `WorkflowExecutor` deleted (D28), this module is
//! the small set of building blocks the rest of the engine still needs:
//!
//! - The `Mcp` trait + `clickweave_mcp::McpClient` adapter — every
//!   executor and agent dispatch goes through it.
//! - `ExecutorEvent` / `ExecutorState` / `ExecutorCommand` — the wire
//!   shape between the runner task and the Tauri layer.
//! - `error::ExecutorError` and supporting types (`CdpCandidate`,
//!   `CandidateView`, `Rect`).
//! - `skill_runner` — the index-walking runner that consumes
//!   `&Skill::action_sketch` directly.
//! - `screenshot` — VLM-input capture helper used by the agent runner.
//! - `cdp_helpers` and `best_effort` — pure-process helpers carried
//!   forward from the deleted deterministic module so the agent's
//!   CDP lifecycle still compiles.

pub(crate) mod best_effort;
pub(crate) mod cdp_helpers;
pub mod error;
pub(crate) mod screenshot;
pub mod skill_runner;

pub use error::*;
pub use skill_runner::{SkillRunContext, run_skill_steps};

use clickweave_core::SkillRun;
use clickweave_llm::ChatBackend;
use serde::{Deserialize, Serialize};
use std::future::Future;
use uuid::Uuid;

/// Trait abstracting MCP tool operations, used to enable test stubs.
///
/// Kept `pub` so external callers (`crate::agent::run_agent_workflow`,
/// the `skill_runner`, and engine-boundary tests) can accept any
/// `M: Mcp` — the public entry points take the trait as a bound so
/// stubs can replace a real `McpClient` subprocess.
pub trait Mcp: Send + Sync {
    fn call_tool(
        &self,
        name: &str,
        arguments: Option<serde_json::Value>,
    ) -> impl Future<Output = anyhow::Result<clickweave_mcp::ToolCallResult>> + Send;

    /// Check whether a tool with the given name is available.
    fn has_tool(&self, name: &str) -> bool;

    /// Convert available tools to the OpenAI-compatible function-call
    /// format.
    fn tools_as_openai(&self) -> Vec<serde_json::Value>;

    /// Re-fetch the server's tool list into the client's internal
    /// cache. Refreshes what `has_tool` reports (e.g. so
    /// `cdp_find_elements` becomes visible after `cdp_connect`) but
    /// does **not** change the agent's LLM-visible tool list — the
    /// latter is seeded once per run in `agent/mod.rs` and kept stable
    /// for prompt-cache stability.
    fn refresh_server_tool_list(&self) -> impl Future<Output = anyhow::Result<()>> + Send;
}

impl Mcp for clickweave_mcp::McpClient {
    fn call_tool(
        &self,
        name: &str,
        arguments: Option<serde_json::Value>,
    ) -> impl Future<Output = anyhow::Result<clickweave_mcp::ToolCallResult>> + Send {
        clickweave_mcp::McpClient::call_tool(self, name, arguments)
    }

    fn has_tool(&self, name: &str) -> bool {
        clickweave_mcp::McpClient::has_tool(self, name)
    }

    fn tools_as_openai(&self) -> Vec<serde_json::Value> {
        clickweave_mcp::McpClient::tools_as_openai(self)
    }

    fn refresh_server_tool_list(&self) -> impl Future<Output = anyhow::Result<()>> + Send {
        clickweave_mcp::McpClient::refresh_tools(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutorState {
    Idle,
    Running,
}

pub enum ExecutorCommand {
    Resume,
    Skip,
    Abort,
}

/// Events sent from the runner task back to the UI.
///
/// The pre-1.D `WorkflowExecutor` shape is preserved on the wire so
/// the front end's `executor://*` listeners and the per-event payload
/// emitters in `commands/executor.rs` continue to compile while the
/// new `skill_runner` ramps up event coverage. Variants tied to the
/// deleted deterministic / supervision / ambiguity flows still exist
/// here as inert payload types — Phase 1.E rewires them to
/// skill-keyed `SafetyScope`-shaped emissions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutorEvent {
    Log(String),
    StateChanged(ExecutorState),
    NodeStarted(Uuid),
    NodeCompleted(Uuid),
    NodeFailed(Uuid, String),
    RunCreated(Uuid, SkillRun),
    WorkflowCompleted,
    ChecksCompleted(Vec<()>),
    Error(String),
    SupervisionPassed {
        node_id: Uuid,
        node_name: String,
        summary: String,
    },
    SupervisionPaused {
        node_id: Uuid,
        node_name: String,
        finding: String,
        /// Base64-encoded screenshot captured during verification, if
        /// available.
        screenshot: Option<String>,
    },
    /// Agent picked one candidate from an ambiguous CDP resolver
    /// match. Fires after the agent commits to a choice; the runner
    /// continues with the chosen uid. The UI renders this as a
    /// persistent card with a modal that overlays each candidate's
    /// rect on top of the captured screenshot.
    AmbiguityResolved {
        node_id: Uuid,
        target: String,
        candidates: Vec<CandidateView>,
        chosen_uid: String,
        reasoning: String,
        viewport_width: f64,
        viewport_height: f64,
        screenshot_path: String,
        screenshot_base64: String,
    },
    NodeCancelled(Uuid),
}

/// Number of consecutive trace-write failures tolerated before the
/// runner emits a degraded-persistence error. Retained at the module
/// level so the per-run telemetry surface stays stable across the
/// 1.D rewrite even though the live counter now lives inside
/// `SkillRunContext` rather than `WorkflowExecutor`.
pub const TRACE_WRITE_FAILURE_THRESHOLD: u32 = 3;

// Allows the trait re-export to type-check even if the compiler
// elides the otherwise-unused import in some build configurations.
#[allow(dead_code)]
fn _executor_chat_backend_marker<C: ChatBackend>() {}
