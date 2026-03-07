use crate::{McpClient, Tool, ToolCallResult};
use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::collections::HashMap;
use tracing::{info, warn};

/// Configuration for a single MCP server to be managed by the router.
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// Routes tool calls to the correct MCP server based on tool-name ownership.
///
/// Spawns multiple `McpClient` instances, merges their tool lists, and builds
/// a routing table mapping each tool name to the server that owns it.
/// First server in the config list wins on tool-name conflicts.
pub struct McpRouter {
    servers: Vec<(String, McpClient)>,
    tool_ownership: HashMap<String, usize>,
    merged_tools: Vec<Tool>,
}

impl McpRouter {
    /// Spawn all configured MCP servers and build the tool routing table.
    ///
    /// The first config is treated as the primary server — if it fails to spawn,
    /// the entire router fails. Non-primary servers that fail to spawn are
    /// skipped with a warning.
    pub async fn spawn(configs: &[McpServerConfig]) -> Result<Self> {
        if configs.is_empty() {
            return Err(anyhow!("McpRouter requires at least one server config"));
        }

        // Spawn all servers concurrently via JoinSet.
        let mut join_set = tokio::task::JoinSet::new();
        for (i, config) in configs.iter().enumerate() {
            let name = config.name.clone();
            let command = config.command.clone();
            let args = config.args.clone();
            join_set.spawn(async move {
                let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                let result = McpClient::spawn(&command, &args_ref).await;
                (i, name, result)
            });
        }

        // Collect results, preserving config order for deterministic routing.
        let mut indexed_results: Vec<_> = Vec::with_capacity(configs.len());
        while let Some(res) = join_set.join_next().await {
            let (i, name, result) = res.context("MCP spawn task panicked")?;
            indexed_results.push((i, name, result));
        }
        indexed_results.sort_by_key(|(i, _, _)| *i);

        let mut servers = Vec::new();
        for (i, name, result) in indexed_results {
            match result {
                Ok(client) => {
                    info!(
                        "MCP server '{}' spawned with {} tools",
                        name,
                        client.tools().len()
                    );
                    servers.push((name, client));
                }
                Err(e) => {
                    if i == 0 {
                        return Err(e)
                            .context(format!("Primary MCP server '{}' failed to spawn", name));
                    }
                    warn!(
                        "MCP server '{}' failed to spawn: {}. Continuing without it.",
                        name, e
                    );
                }
            }
        }

        let mut tool_ownership = HashMap::new();
        let mut merged_tools = Vec::new();

        for (server_idx, (name, client)) in servers.iter().enumerate() {
            for tool in client.tools() {
                if let Some(&existing_idx) = tool_ownership.get(&tool.name) {
                    warn!(
                        "Tool '{}' from '{}' conflicts with '{}' — using first server's version",
                        tool.name,
                        name,
                        servers
                            .get(existing_idx)
                            .map(|(n, _): &(String, McpClient)| n.as_str())
                            .unwrap_or("unknown")
                    );
                } else {
                    tool_ownership.insert(tool.name.clone(), server_idx);
                    merged_tools.push(tool.clone());
                }
            }
        }

        info!(
            "McpRouter ready: {} servers, {} tools",
            servers.len(),
            merged_tools.len()
        );

        Ok(Self {
            servers,
            tool_ownership,
            merged_tools,
        })
    }

    /// Call a tool by name, routing to the server that owns it.
    pub async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<ToolCallResult> {
        let server_idx = self
            .tool_ownership
            .get(name)
            .ok_or_else(|| anyhow!("Tool '{}' not found in any MCP server", name))?;

        self.servers[*server_idx].1.call_tool(name, arguments).await
    }

    /// Get all tools from all servers (merged, deduplicated).
    pub fn tools(&self) -> &[Tool] {
        &self.merged_tools
    }

    /// Convert all tools to OpenAI-compatible function-calling format.
    pub fn tools_as_openai(&self) -> Vec<Value> {
        crate::tools_to_openai(&self.merged_tools)
    }

    /// Number of active servers.
    pub fn server_count(&self) -> usize {
        self.servers.len()
    }

    /// Dispatch a tool call to a specific MCP server by name, bypassing
    /// tool-ownership lookup. Used by the executor to target chrome-devtools
    /// tools that conflict with native-devtools (e.g. both have `click`).
    pub async fn call_tool_on(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: Option<Value>,
    ) -> Result<ToolCallResult> {
        let idx = self
            .servers
            .iter()
            .position(|(name, _)| name == server_name)
            .ok_or_else(|| anyhow!("MCP server '{}' not found", server_name))?;
        self.servers[idx].1.call_tool(tool_name, arguments).await
    }

    /// Check whether a server with the given name is connected.
    pub fn has_server(&self, server_name: &str) -> bool {
        self.servers.iter().any(|(name, _)| name == server_name)
    }

    /// Spawn a single MCP server at runtime and add it to the routing table.
    ///
    /// If a server with the same name already exists, this is a no-op.
    /// Tools from the new server are added to the merged list; existing
    /// servers win on tool-name conflicts (same first-wins policy as spawn).
    pub async fn spawn_server(&mut self, config: &McpServerConfig) -> Result<()> {
        if self.has_server(&config.name) {
            return Ok(());
        }

        let args_ref: Vec<&str> = config.args.iter().map(|s| s.as_str()).collect();
        let client = McpClient::spawn(&config.command, &args_ref)
            .await
            .with_context(|| format!("Failed to spawn MCP server '{}'", config.name))?;

        info!(
            "MCP server '{}' spawned dynamically with {} tools",
            config.name,
            client.tools().len()
        );

        let server_idx = self.servers.len();
        for tool in client.tools() {
            if !self.tool_ownership.contains_key(&tool.name) {
                self.tool_ownership.insert(tool.name.clone(), server_idx);
                self.merged_tools.push(tool.clone());
            } else {
                warn!(
                    "Tool '{}' from '{}' conflicts with existing — skipping",
                    tool.name, config.name
                );
            }
        }

        self.servers.push((config.name.clone(), client));
        Ok(())
    }

    /// Kill all MCP server processes.
    pub fn kill_all(&mut self) {
        for (name, client) in &mut self.servers {
            if let Err(e) = client.kill() {
                warn!("Failed to kill MCP server '{}': {}", name, e);
            }
        }
    }
}

/// Build the default set of MCP server configs.
///
/// `mcp_command` controls the native-devtools server:
/// - `"npx"` spawns via `npx -y native-devtools-mcp`
/// - Any other value is used as a direct command path
///
/// CDP servers are spawned lazily per-app by the executor.
pub fn default_server_configs(mcp_command: &str) -> Vec<McpServerConfig> {
    let native = if mcp_command == "npx" {
        McpServerConfig {
            name: "native-devtools".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "native-devtools-mcp".into()],
        }
    } else {
        McpServerConfig {
            name: "native-devtools".into(),
            command: mcp_command.into(),
            args: vec![],
        }
    };

    vec![native]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an McpRouter from pre-built tool lists (no subprocess spawning).
    /// Each entry is (server_name, Vec<Tool>).
    fn make_router(servers: Vec<(&str, Vec<Tool>)>) -> McpRouter {
        let mut tool_ownership = HashMap::new();
        let mut merged_tools = Vec::new();

        for (server_idx, (_, tools)) in servers.iter().enumerate() {
            for tool in tools {
                if !tool_ownership.contains_key(&tool.name) {
                    tool_ownership.insert(tool.name.clone(), server_idx);
                    merged_tools.push(tool.clone());
                }
            }
        }

        McpRouter {
            // We can't create real McpClient instances, so leave servers empty.
            // These tests only exercise the routing table, not call_tool.
            servers: Vec::new(),
            tool_ownership,
            merged_tools,
        }
    }

    fn tool(name: &str) -> Tool {
        Tool {
            name: name.to_string(),
            description: Some(format!("{} tool", name)),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    #[test]
    fn merged_tools_from_two_servers() {
        let router = make_router(vec![
            ("native", vec![tool("click"), tool("find_text")]),
            (
                "chrome",
                vec![tool("navigate_page"), tool("evaluate_script")],
            ),
        ]);
        assert_eq!(router.tools().len(), 4);
        let names: Vec<&str> = router.tools().iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"click"));
        assert!(names.contains(&"navigate_page"));
    }

    #[test]
    fn first_server_wins_on_conflict() {
        let router = make_router(vec![
            ("native", vec![tool("click"), tool("take_screenshot")]),
            ("chrome", vec![tool("click"), tool("navigate_page")]),
        ]);
        // "click" should appear once (from native)
        let click_count = router.tools().iter().filter(|t| t.name == "click").count();
        assert_eq!(click_count, 1);
        // Total: click, take_screenshot, navigate_page = 3
        assert_eq!(router.tools().len(), 3);
    }

    #[test]
    fn tools_as_openai_format() {
        let router = make_router(vec![("native", vec![tool("click")])]);
        let openai = router.tools_as_openai();
        assert_eq!(openai.len(), 1);
        assert_eq!(openai[0]["type"], "function");
        assert_eq!(openai[0]["function"]["name"], "click");
    }

    #[test]
    fn empty_server_produces_empty_tools() {
        let router = make_router(vec![("empty", vec![])]);
        assert_eq!(router.tools().len(), 0);
        assert_eq!(router.tools_as_openai().len(), 0);
    }

    #[test]
    fn default_configs_npx() {
        let configs = default_server_configs("npx");
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "native-devtools");
        assert_eq!(configs[0].command, "npx");
        assert!(configs[0].args.contains(&"-y".to_string()));
        assert!(configs[0].args.contains(&"native-devtools-mcp".to_string()));
    }

    #[test]
    fn has_server_returns_false_for_test_router() {
        // make_router uses an empty servers vec (no real McpClient instances),
        // so has_server always returns false. The method is trivial and tested
        // via integration testing for the true case.
        let router = make_router(vec![("native", vec![tool("click")])]);
        assert!(!router.has_server("native"));
        assert!(!router.has_server("nonexistent"));
    }

    #[test]
    fn call_tool_on_unknown_server_returns_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let router = make_router(vec![("native", vec![tool("click")])]);
            let result = router.call_tool_on("nonexistent", "click", None).await;
            assert!(result.is_err());
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("MCP server 'nonexistent' not found")
            );
        });
    }

    #[test]
    fn default_configs_custom_command() {
        let configs = default_server_configs("/usr/local/bin/native-devtools");
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].command, "/usr/local/bin/native-devtools");
        assert!(configs[0].args.is_empty());
    }
}
