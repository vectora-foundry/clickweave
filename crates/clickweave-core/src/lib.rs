pub mod app_detection;
pub mod app_kind;
pub mod auto_id;
pub mod cdp;
pub mod chat_trace;
pub mod chrome_profiles;
pub mod decision_cache;
mod node_params;
pub mod output_schema;
pub mod runtime;
pub mod sanitize;
pub mod storage;
pub mod tool_mapping;
pub mod variant_index;
pub mod walkthrough;
mod workflow;

pub use app_kind::AppKind;
pub use node_params::*;
pub use output_schema::*;
pub use walkthrough::*;
pub use workflow::*;

/// Basic workflow validation: ensures the workflow has at least one node.
pub fn validate_workflow(workflow: &Workflow) -> Result<(), String> {
    if workflow.nodes.is_empty() {
        return Err("Workflow has no nodes".to_string());
    }
    Ok(())
}
