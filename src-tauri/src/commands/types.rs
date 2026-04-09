use clickweave_core::storage::RunStorage;
use clickweave_core::{ExecutionMode, NodeType, Workflow};
use clickweave_llm::LlmConfig;
use clickweave_llm::planner::conversation::RunContext;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::PathBuf;
use tauri::Manager;

pub struct AppDataDir(pub PathBuf);

/// MCP sidecar resolution result, checked once at startup.
/// Ok(path) = binary found, Err(reason) = missing or invalid.
pub struct McpStatus(pub Result<String, String>);

pub fn resolve_storage(
    app: &tauri::AppHandle,
    project_path: &Option<String>,
    workflow_name: &str,
    workflow_id: uuid::Uuid,
) -> RunStorage {
    match project_path {
        Some(p) => RunStorage::new(&project_dir(p), workflow_name),
        None => {
            let app_data_dir = app.state::<AppDataDir>();
            RunStorage::new_app_data(&app_data_dir.0, workflow_name, workflow_id)
        }
    }
}

pub fn project_dir(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.extension().is_some() {
        p.parent().unwrap_or(&p).to_path_buf()
    } else {
        p
    }
}

pub fn parse_uuid(s: &str, label: &str) -> Result<uuid::Uuid, super::error::CommandError> {
    s.parse()
        .map_err(|_| super::error::CommandError::validation(format!("Invalid {} ID", label)))
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct ProjectData {
    pub path: String,
    pub workflow: Workflow,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct NodeTypeInfo {
    pub name: &'static str,
    pub output_role: String,
    pub node_context: String,
    pub icon: &'static str,
    pub node_type: NodeType,
}

#[derive(Debug, Serialize, Type)]
pub struct ConfirmableTool {
    pub name: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct EndpointConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
}

impl EndpointConfig {
    pub fn into_llm_config(self, temperature: Option<f32>) -> LlmConfig {
        LlmConfig {
            base_url: self.base_url,
            api_key: self.api_key.filter(|k| !k.is_empty()),
            model: self.model,
            temperature,
            max_tokens: None,
            ..LlmConfig::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.base_url.is_empty() || self.model.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct RunRequest {
    pub workflow: Workflow,
    pub project_path: Option<String>,
    pub agent: EndpointConfig,
    pub fast: Option<EndpointConfig>,
    /// Planner LLM used for supervision in Test mode.
    pub planner: Option<EndpointConfig>,
    pub execution_mode: ExecutionMode,
    #[serde(default)]
    pub auto_approve_resolutions: bool,
    #[serde(default = "default_outcome_delay_ms")]
    pub outcome_delay_ms: u64,
    #[serde(default = "default_supervision_delay_ms")]
    pub supervision_delay_ms: u64,
}

fn default_outcome_delay_ms() -> u64 {
    1000
}

fn default_supervision_delay_ms() -> u64 {
    500
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct PatchRequest {
    pub workflow: Workflow,
    pub user_prompt: String,
    pub planner: EndpointConfig,
    pub allow_ai_transforms: bool,
    pub allow_agent_steps: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct WorkflowPatch {
    pub added_nodes: Vec<clickweave_core::Node>,
    pub removed_node_ids: Vec<String>,
    pub updated_nodes: Vec<clickweave_core::Node>,
    pub added_edges: Vec<clickweave_core::Edge>,
    pub removed_edges: Vec<clickweave_core::Edge>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct RunsQuery {
    pub project_path: Option<String>,
    pub workflow_id: String,
    pub workflow_name: String,
    pub node_name: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct RunEventsQuery {
    pub project_path: Option<String>,
    pub workflow_id: String,
    pub workflow_name: String,
    pub node_name: String,
    pub execution_dir: Option<String>,
    pub run_id: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct ImportedAsset {
    pub relative_path: String,
    pub absolute_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogPayload {
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatePayload {
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodePayload {
    pub node_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeErrorPayload {
    pub node_id: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SupervisionPassedPayload {
    pub node_id: String,
    pub node_name: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SupervisionPausedPayload {
    pub node_id: String,
    pub node_name: String,
    pub finding: String,
    /// Base64-encoded screenshot captured during verification, if available.
    pub screenshot: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct AssistantChatRequest {
    pub workflow: Workflow,
    pub user_message: String,
    pub run_context: Option<RunContext>,
    pub planner: EndpointConfig,
    pub fast: Option<EndpointConfig>,
    pub allow_ai_transforms: bool,
    pub allow_agent_steps: bool,
    pub max_repair_attempts: u32,
    #[serde(default)]
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct AssistantChatResponse {
    pub patch: Option<WorkflowPatch>,
    pub warnings: Vec<String>,
    pub context_usage: Option<f32>,
    pub intent: Option<String>,
}

// --- Walkthrough event payloads ---

#[derive(Debug, Clone, Serialize)]
pub struct WalkthroughStatePayload {
    pub status: clickweave_core::WalkthroughStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct WalkthroughDraftPayload {
    pub actions: Vec<clickweave_core::WalkthroughAction>,
    pub draft: clickweave_core::Workflow,
    pub warnings: Vec<String>,
    pub action_node_map: Vec<clickweave_core::walkthrough::ActionNodeEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WalkthroughEventPayload {
    pub event: clickweave_core::WalkthroughEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct AppResolutionSeedEntry {
    pub node_id: String,
    pub app_name: String,
}

// --- Planner session event payloads ---

#[derive(Debug, Clone, Serialize)]
pub struct PlannerToolCallPayload {
    pub session_id: String,
    pub tool_name: String,
    pub args: serde_json::Value,
    pub result: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannerConfirmationPayload {
    pub session_id: String,
    pub message: String,
    pub tool_name: String,
}

// --- Resolution event payloads ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionProposedPayload {
    pub node_id: String,
    pub node_name: String,
    pub reason: String,
    pub patch: WorkflowPatch,
    pub screenshot: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessagePayload {
    pub session_id: String,
    pub entry: clickweave_llm::planner::conversation::ChatEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStartedPayload {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchAppliedPayload {
    pub patch: WorkflowPatch,
}
