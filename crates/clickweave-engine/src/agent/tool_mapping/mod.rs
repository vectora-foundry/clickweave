//! Engine-private mapping between `TraceNodeKind` and MCP tool invocations.
//!
//! Moved from `clickweave-core::tool_mapping` as part of the skill-only-shell
//! rewrite. The mapping is engine-internal once the Tauri surface no longer
//! exposes canvas types.
//!
//! The `TraceNodeKind` enum (renamed from `NodeType` in core) is re-exported
//! from this module. Within the submodules a `use super::TraceNodeKind as
//! NodeType;` alias keeps the match arms readable without a wholesale
//! pattern rename.

use clickweave_core::{
    AppKind, AxClickParams, AxSelectParams, AxSetValueParams, AxTarget, CdpClickParams,
    CdpClosePageParams, CdpFillParams, CdpHandleDialogParams, CdpHoverParams, CdpNavigateParams,
    CdpNewPageParams, CdpPressKeyParams, CdpSelectPageParams, CdpTarget, CdpTypeParams,
    CdpWaitParams, ClickParams, ClickTarget, DragParams, FindAppParams, FindImageParams,
    FindTextParams, FocusTarget, FocusWindowParams, HoverParams, LaunchAppParams,
    McpToolCallParams, MouseButton, PressKeyParams, QuitAppParams, ScreenshotMode, ScrollParams,
    TakeScreenshotParams, TypeTextParams,
};
use serde_json::Value;
use std::fmt;

pub use crate::agent::trace_graph::TraceNodeKind;

/// A tool name and its arguments, ready to be sent to an MCP server.
#[derive(Debug, Clone)]
pub struct ToolInvocation {
    pub name: String,
    pub arguments: Value,
}

/// Errors that can occur during tool mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolMappingError {
    /// The tool name is not in the known tools list.
    UnknownTool(String),
    /// A required argument is missing.
    MissingArgument { tool: String, argument: String },
    /// The node type cannot be mapped to a tool invocation (e.g. AiStep, AppDebugKitOp).
    NotAToolNode,
}

impl fmt::Display for ToolMappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolMappingError::UnknownTool(name) => write!(f, "Unknown tool: {}", name),
            ToolMappingError::MissingArgument { tool, argument } => {
                write!(f, "{} requires non-empty '{}' argument", tool, argument)
            }
            ToolMappingError::NotAToolNode => write!(f, "Not a tool node"),
        }
    }
}

impl std::error::Error for ToolMappingError {}

pub(super) fn required_str<'a>(
    args: &'a Value,
    tool: &str,
    argument: &str,
) -> Result<&'a str, ToolMappingError> {
    args.get(argument)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolMappingError::MissingArgument {
            tool: tool.into(),
            argument: argument.into(),
        })
}

pub(super) fn optional_str(args: &Value, field: &str) -> String {
    args.get(field)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

mod ax;
mod cdp;
mod desktop;
mod node_to_tool;

pub use node_to_tool::node_type_to_tool_invocation;

/// Convert a tool name and JSON arguments to a `TraceNodeKind`.
///
/// Maps known tool names to typed `TraceNodeKind` variants. Unknown tools that
/// exist in `known_tools` are mapped to `McpToolCall`. Unknown tools not in
/// `known_tools` return `Err(UnknownTool)`.
pub fn tool_invocation_to_node_type(
    name: &str,
    args: &Value,
    known_tools: &[Value],
) -> Result<TraceNodeKind, ToolMappingError> {
    // Local alias so the submodule match arms continue to read as `NodeType::*`.
    use TraceNodeKind as NodeType;

    if let Some(node) = desktop::desktop_tool_invocation_to_node_type(name, args)? {
        return Ok(node);
    }
    if let Some(node) = cdp::cdp_tool_invocation_to_node_type(name, args) {
        return Ok(node);
    }
    if let Some(node) = ax::ax_tool_invocation_to_node_type(name, args) {
        return Ok(node);
    }
    if known_tools
        .iter()
        .any(|t| t["function"]["name"].as_str() == Some(name))
    {
        return Ok(NodeType::McpToolCall(McpToolCallParams {
            tool_name: name.to_string(),
            arguments: args.clone(),
        }));
    }
    Err(ToolMappingError::UnknownTool(name.to_string()))
}

#[cfg(test)]
mod tests;
