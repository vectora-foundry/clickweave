//! Shared mapping between `NodeType` and MCP tool invocations.
//!
//! Used by both the agent (tool args → NodeType) and the executor (NodeType → tool args).

use crate::{
    AppKind, AxClickParams, AxSelectParams, AxSetValueParams, AxTarget, CdpClickParams,
    CdpClosePageParams, CdpFillParams, CdpHandleDialogParams, CdpHoverParams, CdpNavigateParams,
    CdpNewPageParams, CdpPressKeyParams, CdpSelectPageParams, CdpTarget, CdpTypeParams,
    CdpWaitParams, ClickParams, ClickTarget, DragParams, FindAppParams, FindImageParams,
    FindTextParams, FocusTarget, FocusWindowParams, HoverParams, LaunchAppParams,
    McpToolCallParams, MouseButton, NodeType, PressKeyParams, QuitAppParams, ScreenshotMode,
    ScrollParams, TakeScreenshotParams, TypeTextParams,
};
use serde_json::Value;
use std::fmt;

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

fn required_str<'a>(
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

fn optional_str(args: &Value, field: &str) -> String {
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

/// Convert a tool name and JSON arguments to a `NodeType`.
///
/// Maps known tool names to typed `NodeType` variants. Unknown tools that exist
/// in `known_tools` are mapped to `McpToolCall`. Unknown tools not in
/// `known_tools` return `Err(UnknownTool)`.
pub fn tool_invocation_to_node_type(
    name: &str,
    args: &Value,
    known_tools: &[Value],
) -> Result<NodeType, ToolMappingError> {
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
