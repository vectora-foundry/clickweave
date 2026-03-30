mod builder;
mod mapping;
mod parse;
mod patch;
mod plan;
mod prompt;
pub mod tool_use;
mod types;

pub mod assistant;
pub mod conversation;
pub mod conversation_loop;
pub mod resolution;
pub mod summarize;

// Private imports for test submodules (accessible via `crate::planner::*`).
// These were private `use` bindings in the original mod.rs; they remain private
// after the split and are gated to cfg(test) since only tests rely on them.
#[cfg(test)]
use clickweave_core::{Node, NodeType, Position, Workflow};

#[cfg(test)]
mod tests;

// ── Re-exports: types ─────────────────────────────────────────────

pub use types::*;

// ── Re-exports: builder ───────────────────────────────────────────

pub(crate) use builder::{
    build_graph_plan_as_patch, build_patch_from_output, build_plan_as_patch,
    build_workflow_from_graph, infer_control_flow_edges, normalize_auto_ids,
    pair_endloop_with_loop, parse_lenient,
};

// ── Re-exports: submodules ────────────────────────────────────────

pub use assistant::{AssistantResult, assistant_chat, assistant_chat_with_backend};
pub use conversation_loop::{ConversationOutput, ToolCallRecord, conversation_loop};
pub use patch::{patch_workflow, patch_workflow_with_backend};
pub use plan::{plan_workflow, plan_workflow_with_backend};
pub use tool_use::{PlannerToolExecutor, ToolPermission};
