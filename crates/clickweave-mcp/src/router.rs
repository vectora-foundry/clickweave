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

        let mut servers = Vec::new();

        for (i, config) in configs.iter().enumerate() {
            let args_ref: Vec<&str> = config.args.iter().map(|s| s.as_str()).collect();
            let result = McpClient::spawn(&config.command, &args_ref).await;

            match result {
                Ok(client) => {
                    info!(
                        "MCP server '{}' spawned with {} tools",
                        config.name,
                        client.tools().len()
                    );
                    servers.push((config.name.clone(), client));
                }
                Err(e) => {
                    if i == 0 {
                        return Err(e).context(format!(
                            "Primary MCP server '{}' failed to spawn",
                            config.name
                        ));
                    }
                    warn!(
                        "MCP server '{}' failed to spawn: {}. Continuing without it.",
                        config.name, e
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
        self.merged_tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema
                    }
                })
            })
            .collect()
    }

    /// Number of active servers.
    pub fn server_count(&self) -> usize {
        self.servers.len()
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
/// Chrome DevTools MCP is always added as a secondary server via npx.
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

    vec![
        native,
        McpServerConfig {
            name: "chrome-devtools".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "chrome-devtools-mcp".into()],
        },
    ]
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
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].name, "native-devtools");
        assert_eq!(configs[0].command, "npx");
        assert!(configs[0].args.contains(&"-y".to_string()));
        assert!(configs[0].args.contains(&"native-devtools-mcp".to_string()));
        assert_eq!(configs[1].name, "chrome-devtools");
    }

    #[test]
    fn default_configs_custom_command() {
        let configs = default_server_configs("/usr/local/bin/native-devtools");
        assert_eq!(configs[0].command, "/usr/local/bin/native-devtools");
        assert!(configs[0].args.is_empty());
        // Chrome DevTools still uses npx
        assert_eq!(configs[1].command, "npx");
    }
}
