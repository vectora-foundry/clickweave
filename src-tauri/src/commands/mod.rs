mod agent;
mod chrome_profiles;
pub mod error;
mod executor;
mod project;
mod runs;
mod types;
mod walkthrough;
mod walkthrough_enrichment;
mod walkthrough_session;

pub use agent::{AgentHandle, approve_agent_action, run_agent, stop_agent};
pub use chrome_profiles::{
    create_chrome_profile, get_chrome_profile_path, is_chrome_profile_configured,
    launch_chrome_for_setup, list_chrome_profiles,
};
pub use executor::{ExecutorHandle, run_workflow, stop_workflow, supervision_respond};
pub use project::{
    confirmable_tools, generate_auto_id, get_mcp_status, import_asset, node_type_defaults,
    open_project, pick_save_file, pick_workflow_file, ping, save_project, validate,
};
pub use runs::{list_runs, load_run_events, read_artifact_base64};
pub use types::{AppDataDir, McpStatus};
pub use walkthrough::{
    WalkthroughHandle, apply_walkthrough_annotations, cancel_walkthrough, detect_cdp_apps,
    get_walkthrough_draft, pause_walkthrough, resume_walkthrough, seed_walkthrough_cache,
    start_walkthrough, stop_walkthrough, validate_app_path,
};

#[tauri::command]
#[specta::specta]
pub async fn check_endpoint(
    base_url: String,
    api_key: Option<String>,
    model: Option<String>,
) -> Result<(), error::CommandError> {
    clickweave_llm::check_endpoint(&base_url, api_key.as_deref(), model.as_deref())
        .await
        .map_err(error::CommandError::validation)
}
