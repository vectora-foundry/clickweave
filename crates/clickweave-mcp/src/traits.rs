use crate::ToolCallResult;
use anyhow::Result;
use serde_json::Value;
use std::future::Future;

/// Abstraction over MCP tool invocation for testability.
///
/// The engine's resolution and supervision methods call tools through this
/// trait, allowing production code to use `McpRouter` while tests inject a
/// `StubToolProvider` with scripted responses.
pub trait ToolProvider: Send + Sync {
    /// Call a tool by name, routing to the appropriate server.
    fn call_tool(
        &self,
        name: &str,
        arguments: Option<Value>,
    ) -> impl Future<Output = Result<ToolCallResult>> + Send;

    /// Call a tool on a specific server by name, bypassing tool-ownership lookup.
    fn call_tool_on(
        &self,
        _server_name: &str,
        name: &str,
        arguments: Option<Value>,
    ) -> impl Future<Output = Result<ToolCallResult>> + Send {
        self.call_tool(name, arguments)
    }

    /// Convert all available tools to OpenAI-compatible function-calling format.
    fn tools_as_openai(&self) -> Vec<Value>;
}
