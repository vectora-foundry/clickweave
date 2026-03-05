use crate::protocol::*;
use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tracing::{debug, error, info, trace};

pub struct McpClient {
    process: Child,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    /// Serializes the full request-write → response-read exchange so concurrent
    /// callers cannot interleave and misattribute responses.
    io_lock: Mutex<()>,
    request_id: AtomicU64,
    tools: Vec<Tool>,
}

impl McpClient {
    /// Spawn native-devtools-mcp and initialize the connection
    pub async fn spawn(command: &str, args: &[&str]) -> Result<Self> {
        info!("Spawning MCP server: {} {:?}", command, args);

        let mut process = tokio::process::Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .context("Failed to spawn MCP server")?;

        let stdin = process.stdin.take().ok_or_else(|| anyhow!("No stdin"))?;
        let stdout = process.stdout.take().ok_or_else(|| anyhow!("No stdout"))?;

        let mut client = Self {
            process,
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            io_lock: Mutex::new(()),
            request_id: AtomicU64::new(1),
            tools: Vec::new(),
        };

        client.initialize().await?;
        client.fetch_tools().await?;

        Ok(client)
    }

    /// Spawn using npx (cross-platform default)
    pub async fn spawn_npx() -> Result<Self> {
        Self::spawn("npx", &["-y", "native-devtools-mcp"]).await
    }

    fn next_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::SeqCst)
    }

    async fn write_message(&self, json: &str) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(json.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn read_response(&self) -> Result<JsonRpcResponse> {
        let mut stdout = self.stdout.lock().await;
        let mut line = String::new();
        stdout.read_line(&mut line).await?;

        trace!("MCP response: {}", line.trim());

        serde_json::from_str(&line).context("Failed to parse MCP response")
    }

    async fn send_request(&self, method: &str, params: Option<Value>) -> Result<JsonRpcResponse> {
        let _guard = self.io_lock.lock().await;

        let id = self.next_id();
        let request = JsonRpcRequest::new(id, method, params);
        let json = serde_json::to_string(&request)?;

        trace!("MCP request: {}", json);
        self.write_message(&json).await?;

        let response = self.read_response().await?;
        if let Some(err) = &response.error {
            error!("MCP error: {} (code {})", err.message, err.code);
        }

        Ok(response)
    }

    async fn send_notification(&self, method: &str) -> Result<()> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method
        });
        let json = serde_json::to_string(&notification)?;

        debug!("MCP notification: {}", json);
        self.write_message(&json).await
    }

    async fn initialize(&mut self) -> Result<()> {
        let params = InitializeParams {
            protocol_version: "2024-11-05".to_string(),
            capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "clickweave".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        let response = self
            .send_request("initialize", Some(serde_json::to_value(params)?))
            .await?;

        if let Some(result) = response.result {
            let init_result: InitializeResult = serde_json::from_value(result)?;
            info!(
                "MCP initialized: protocol={}, server={:?}",
                init_result.protocol_version,
                init_result.server_info.as_ref().map(|s| &s.name)
            );
        }

        self.send_notification("notifications/initialized").await
    }

    async fn fetch_tools(&mut self) -> Result<()> {
        let response = self.send_request("tools/list", None).await?;

        if let Some(result) = response.result {
            let tools_result: ToolsListResult = serde_json::from_value(result)?;
            info!("Loaded {} MCP tools", tools_result.tools.len());
            for tool in &tools_result.tools {
                debug!("  - {}: {:?}", tool.name, tool.description);
            }
            self.tools = tools_result.tools;
        }

        Ok(())
    }

    /// Get available tools
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    /// Call a tool by name with arguments
    pub async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<ToolCallResult> {
        let params = ToolCallParams {
            name: name.to_string(),
            arguments,
        };

        let response = self
            .send_request("tools/call", Some(serde_json::to_value(params)?))
            .await?;

        if let Some(err) = response.error {
            return Err(anyhow!("Tool call failed: {}", err.message));
        }

        let result = response
            .result
            .ok_or_else(|| anyhow!("No result from tool call"))?;

        let tool_result: ToolCallResult = serde_json::from_value(result)?;
        Ok(tool_result)
    }

    /// Convert MCP tools to OpenAI-compatible tool format
    pub fn tools_as_openai(&self) -> Vec<Value> {
        crate::tools_to_openai(&self.tools)
    }

    /// Check if process is still running
    pub fn is_running(&mut self) -> bool {
        matches!(self.process.try_wait(), Ok(None))
    }

    /// Kill the MCP server process
    pub fn kill(&mut self) -> Result<()> {
        self.process
            .start_kill()
            .context("Failed to kill MCP server")?;
        Ok(())
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}
