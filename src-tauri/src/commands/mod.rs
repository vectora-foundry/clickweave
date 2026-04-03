mod assistant;
mod chrome_profiles;
pub mod error;
mod executor;
mod planner;
mod planner_session;
pub mod pre_gather;
mod project;
mod resolution_listener;
mod runs;
mod types;
mod walkthrough;
mod walkthrough_enrichment;
mod walkthrough_session;

pub use assistant::{
    assistant_chat, cancel_assistant_chat, get_assistant_session_id, rewind_conversation,
};
pub use chrome_profiles::{
    create_chrome_profile, get_chrome_profile_path, is_chrome_profile_configured,
    launch_chrome_for_setup, list_chrome_profiles,
};
pub use executor::{ExecutorHandle, run_workflow, stop_workflow, supervision_respond};
pub use planner::patch_workflow;
pub use planner_session::{
    AssistantSessionHandle, PlannerHandle, clear_assistant_session, planner_confirmation_respond,
};
pub use project::{
    confirmable_tools, generate_auto_id, get_mcp_status, import_asset, load_conversation,
    node_type_defaults, open_project, pick_save_file, pick_workflow_file, ping, save_conversation,
    save_project, validate,
};
pub use resolution_listener::{ResolutionState, resolution_respond};
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
