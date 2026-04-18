use crate::protocol::*;
use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{debug, error, info, trace, warn};

const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(30);
const TOOLS_LIST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(60);

const MAX_SKIPPED_LINES: usize = 64;
const MAX_MISMATCHED_IDS: usize = 8;

/// Typed errors the MCP client surfaces so supervision layers can decide between
/// respawning the subprocess (Timeout, SubprocessClosed) and failing the call
/// (Protocol).
#[derive(Debug, Error)]
pub enum McpError {
    #[error("MCP request `{method}` timed out after {timeout:?}")]
    Timeout { method: String, timeout: Duration },

    #[error("MCP subprocess closed stdout (EOF)")]
    SubprocessClosed,

    #[error("MCP server error {code}: {message}")]
    Protocol { code: i64, message: String },
}

impl JsonRpcResponse {
    fn into_result(self) -> Result<Option<Value>> {
        if let Some(err) = self.error {
            return Err(McpError::Protocol {
                code: err.code,
                message: err.message,
            }
            .into());
        }
        Ok(self.result)
    }
}

/// JSON-RPC 2.0 client over a subprocess spawned on construction.
///
/// Correctness relies on `io_lock` serializing each request-write -> response-read
/// pair. `tools` is a write-rare cache rebuildable via `refresh_tools`, so its
/// `RwLock` poisoning is recovered silently.
pub struct McpClient {
    process: Child,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    io_lock: Mutex<()>,
    request_id: AtomicU64,
    tools: RwLock<Vec<Tool>>,
    stderr_task: Option<JoinHandle<()>>,
}

impl McpClient {
    /// Spawn native-devtools-mcp and initialize the connection.
    pub async fn spawn(command: &str, args: &[&str]) -> Result<Self> {
        info!("Spawning MCP server: {} {:?}", command, args);

        let mut process = tokio::process::Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("Failed to spawn MCP server")?;

        let stdin = process.stdin.take().ok_or_else(|| anyhow!("No stdin"))?;
        let stdout = process.stdout.take().ok_or_else(|| anyhow!("No stdout"))?;
        let stderr = process.stderr.take().ok_or_else(|| anyhow!("No stderr"))?;

        let stderr_task = Some(tokio::spawn(forward_stderr(stderr)));

        let mut client = Self {
            process,
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            io_lock: Mutex::new(()),
            request_id: AtomicU64::new(1),
            tools: RwLock::new(Vec::new()),
            stderr_task,
        };

        client.initialize().await?;
        client.refresh_tools().await?;

        Ok(client)
    }

    fn next_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::SeqCst)
    }

    async fn write_message(&self, json: &str) -> Result<()> {
        let mut buf = Vec::with_capacity(json.len() + 1);
        buf.extend_from_slice(json.as_bytes());
        buf.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&buf).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn read_response(&self) -> Result<JsonRpcResponse> {
        let mut stdout = self.stdout.lock().await;
        let mut skipped = 0usize;

        loop {
            let mut line = String::new();
            let bytes = stdout
                .read_line(&mut line)
                .await
                .context("Failed to read MCP response line")?;
            if bytes == 0 {
                return Err(McpError::SubprocessClosed.into());
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                warn!("MCP produced blank stdout line; skipping");
                skipped += 1;
                if skipped > MAX_SKIPPED_LINES {
                    return Err(anyhow!(
                        "Too many malformed lines on MCP stdout (>{MAX_SKIPPED_LINES})"
                    ));
                }
                continue;
            }

            trace!("MCP response: {}", trimmed);

            let value: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(e) => {
                    let preview: String = trimmed.chars().take(200).collect();
                    warn!(
                        "Non-JSON line on MCP stdout (skipping): {} -- {}",
                        e, preview
                    );
                    skipped += 1;
                    if skipped > MAX_SKIPPED_LINES {
                        return Err(anyhow!(
                            "Too many malformed lines on MCP stdout (>{MAX_SKIPPED_LINES})"
                        ));
                    }
                    continue;
                }
            };

            if value.get("method").is_some() && value.get("id").is_none() {
                debug!(
                    "Skipping server notification: {}",
                    value
                        .get("method")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown")
                );
                continue;
            }

            return serde_json::from_value(value).context("Failed to parse MCP response");
        }
    }

    async fn send_request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<JsonRpcResponse> {
        let _guard = self.io_lock.lock().await;

        let id = self.next_id();
        let request = JsonRpcRequest::new(id, method, params);
        let json = serde_json::to_string(&request)?;

        trace!("MCP request: {}", json);
        self.write_message(&json).await?;

        // Enforce a cumulative deadline across id-mismatch retries so a server
        // emitting stray ids can't keep the request alive past `timeout`.
        let deadline = Instant::now() + timeout;
        let mut mismatched = 0usize;
        loop {
            let response = match tokio::time::timeout_at(deadline, self.read_response()).await {
                Ok(r) => r?,
                Err(_) => {
                    return Err(McpError::Timeout {
                        method: method.to_string(),
                        timeout,
                    }
                    .into());
                }
            };

            if response.id != Some(id) {
                mismatched += 1;
                error!(
                    "MCP response id mismatch: expected {}, got {:?}",
                    id, response.id
                );
                if mismatched > MAX_MISMATCHED_IDS {
                    return Err(anyhow!(
                        "Too many mismatched response ids on MCP stdout (>{MAX_MISMATCHED_IDS})"
                    ));
                }
                continue;
            }

            if let Some(err) = &response.error {
                error!("MCP error: {} (code {})", err.message, err.code);
            }

            return Ok(response);
        }
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
            .send_request(
                "initialize",
                Some(serde_json::to_value(params)?),
                INITIALIZE_TIMEOUT,
            )
            .await?;
        let result = response
            .into_result()?
            .ok_or_else(|| anyhow!("initialize response missing both `result` and `error`"))?;
        let init_result: InitializeResult =
            serde_json::from_value(result).context("Failed to deserialize initialize result")?;
        info!(
            "MCP initialized: protocol={}, server={:?}",
            init_result.protocol_version,
            init_result.server_info.as_ref().map(|s| &s.name)
        );

        self.send_notification("notifications/initialized").await
    }

    /// Re-fetch the tool list from the MCP server.
    /// Call after operations that change the server's available tools
    /// (e.g., `cdp_connect` exposes new CDP inspection tools).
    pub async fn refresh_tools(&self) -> Result<()> {
        let response = self
            .send_request("tools/list", None, TOOLS_LIST_TIMEOUT)
            .await?;

        if let Some(result) = response.into_result()? {
            let tools_result: ToolsListResult = serde_json::from_value(result)?;
            info!(
                "Refreshed MCP tools: {} available",
                tools_result.tools.len()
            );
            for tool in &tools_result.tools {
                debug!("  - {}: {:?}", tool.name, tool.description);
            }
            *self.tools_write() = tools_result.tools;
        }

        Ok(())
    }

    /// Get available tool count.
    pub fn tool_count(&self) -> usize {
        self.tools_read().len()
    }

    /// Check whether a tool with the given name is available.
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools_read().iter().any(|t| t.name == name)
    }

    /// Call a tool by name with arguments, using the default timeout.
    pub async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<ToolCallResult> {
        self.call_tool_with_timeout(name, arguments, DEFAULT_TOOL_CALL_TIMEOUT)
            .await
    }

    /// On timeout, returns [`McpError::Timeout`] so supervision layers can
    /// decide to respawn the subprocess.
    pub async fn call_tool_with_timeout(
        &self,
        name: &str,
        arguments: Option<Value>,
        timeout: Duration,
    ) -> Result<ToolCallResult> {
        let params = ToolCallParams {
            name: name.to_string(),
            arguments,
        };

        let response = self
            .send_request("tools/call", Some(serde_json::to_value(params)?), timeout)
            .await?;

        let result = response
            .into_result()?
            .ok_or_else(|| anyhow!("tools/call response missing both `result` and `error`"))?;
        let tool_result: ToolCallResult = serde_json::from_value(result)?;
        Ok(tool_result)
    }

    /// Convert MCP tools to OpenAI-compatible tool format.
    pub fn tools_as_openai(&self) -> Vec<Value> {
        crate::tools_to_openai(&self.tools_read())
    }

    fn tools_read(&self) -> RwLockReadGuard<'_, Vec<Tool>> {
        self.tools.read().unwrap_or_else(|e| e.into_inner())
    }

    fn tools_write(&self) -> RwLockWriteGuard<'_, Vec<Tool>> {
        self.tools.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Sends `SIGKILL` to the subprocess. Reaping is handled by Tokio's
    /// `kill_on_drop` machinery when the `McpClient` is dropped.
    pub fn kill(&mut self) -> Result<()> {
        self.process
            .start_kill()
            .context("Failed to kill MCP server")?;
        Ok(())
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if let Some(handle) = self.stderr_task.take() {
            handle.abort();
        }
        if let Err(e) = self.process.start_kill() {
            debug!("MCP kill on drop failed: {e}");
        }
    }
}

async fn forward_stderr(stderr: ChildStderr) {
    let mut reader = BufReader::new(stderr).lines();
    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                debug!(target: "mcp.stderr", "{line}");
            }
            Ok(None) => break,
            Err(e) => {
                warn!(target: "mcp.stderr", "error reading MCP stderr: {e}");
                break;
            }
        }
    }
}
