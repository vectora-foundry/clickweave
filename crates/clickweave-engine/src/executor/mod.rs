mod ai_step;
mod app_resolve;
mod control_flow;
mod deterministic;
mod element_resolve;
mod graph_nav;
mod run_loop;
mod supervision;
mod trace;
mod variables;
mod verdict;

#[cfg(test)]
mod tests;

use clickweave_core::decision_cache::DecisionCache;
use clickweave_core::runtime::RuntimeContext;
use clickweave_core::storage::RunStorage;
use clickweave_core::{ExecutionMode, NodeRun, NodeVerdict, Workflow};
use clickweave_llm::{ChatBackend, LlmClient, LlmConfig, Message};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutorState {
    Idle,
    Running,
}

pub enum ExecutorCommand {
    Stop,
    Resume,
    Skip,
    Abort,
}

/// Events sent from the executor back to the UI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutorEvent {
    Log(String),
    StateChanged(ExecutorState),
    NodeStarted(Uuid),
    NodeCompleted(Uuid),
    NodeFailed(Uuid, String),
    RunCreated(Uuid, NodeRun),
    WorkflowCompleted,
    ChecksCompleted(Vec<NodeVerdict>),
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
        /// Base64-encoded screenshot captured during verification, if available.
        screenshot: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedApp {
    pub name: String,
    pub pid: i32,
}

pub struct WorkflowExecutor<C: ChatBackend = LlmClient> {
    workflow: Workflow,
    agent: C,
    vlm: Option<C>,
    /// Planner-class LLM used for supervision verification in Test mode.
    /// Falls back to VLM, then agent if not configured.
    supervision: Option<C>,
    mcp_command: String,
    execution_mode: ExecutionMode,
    project_path: Option<PathBuf>,
    event_tx: Sender<ExecutorEvent>,
    storage: RunStorage,
    app_cache: RwLock<HashMap<String, ResolvedApp>>,
    focused_app: RwLock<Option<String>>,
    element_cache: RwLock<HashMap<(String, Option<String>), String>>,
    context: RuntimeContext,
    decision_cache: RwLock<DecisionCache>,
    /// Persistent conversation history for supervision across the entire run.
    supervision_history: RwLock<Vec<Message>>,
    /// Verdicts from Verification-role nodes, accumulated during execution.
    runtime_verdicts: Vec<NodeVerdict>,
    /// Set by eval_control_flow when a loop exits; consumed by the main loop
    /// to run a deferred visual verification after the loop completes.
    pending_loop_exit: Option<PendingLoopExit>,
}

pub(crate) struct PendingLoopExit {
    pub node_id: Uuid,
    pub loop_name: String,
    pub reason: LoopExitReason,
    pub iterations: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopExitReason {
    ConditionMet,
    MaxIterations,
}

impl LoopExitReason {
    pub fn as_str(self) -> &'static str {
        match self {
            LoopExitReason::ConditionMet => "condition_met",
            LoopExitReason::MaxIterations => "max_iterations",
        }
    }
}

impl WorkflowExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workflow: Workflow,
        agent_config: LlmConfig,
        vlm_config: Option<LlmConfig>,
        supervision_config: Option<LlmConfig>,
        mcp_command: String,
        execution_mode: ExecutionMode,
        project_path: Option<PathBuf>,
        event_tx: Sender<ExecutorEvent>,
        storage: RunStorage,
    ) -> Self {
        let decision_cache = DecisionCache::load(&storage.cache_path())
            .unwrap_or_else(|| DecisionCache::new(workflow.id));
        Self {
            workflow,
            agent: LlmClient::new(agent_config),
            vlm: vlm_config.map(LlmClient::new),
            supervision: supervision_config.map(LlmClient::new),
            mcp_command,
            execution_mode,
            project_path,
            event_tx,
            storage,
            app_cache: RwLock::new(HashMap::new()),
            focused_app: RwLock::new(None),
            element_cache: RwLock::new(HashMap::new()),
            context: RuntimeContext::new(),
            decision_cache: RwLock::new(decision_cache),
            supervision_history: RwLock::new(Vec::new()),
            runtime_verdicts: Vec::new(),
            pending_loop_exit: None,
        }
    }
}

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Return the best available LLM for text reasoning tasks (app resolution,
    /// element resolution). Prefers supervision (planner-class), falls back to
    /// VLM, then agent. The tiny agent model often has insufficient context for
    /// these prompts.
    pub(crate) fn reasoning_backend(&self) -> &C {
        self.supervision
            .as_ref()
            .or(self.vlm.as_ref())
            .unwrap_or(&self.agent)
    }

    /// Return the best available LLM for vision tasks (image analysis,
    /// screenshot verification). Prefers an explicitly configured VLM, falls
    /// back to supervision (planner-class). Returns `None` only when neither
    /// VLM nor planner is configured.
    pub(crate) fn vision_backend(&self) -> Option<&C> {
        self.vlm.as_ref().or(self.supervision.as_ref())
    }
}
