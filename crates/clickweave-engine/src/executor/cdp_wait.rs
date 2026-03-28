use super::Mcp;
use super::{ExecutorResult, WorkflowExecutor};
use clickweave_llm::ChatBackend;
use serde_json::Value;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Execute a CdpWait node: call native-devtools cdp_wait_for tool.
    pub(crate) async fn execute_cdp_wait(
        &self,
        text: &str,
        timeout_ms: u64,
        mcp: &(impl Mcp + ?Sized),
    ) -> ExecutorResult<Value> {
        let result = mcp
            .call_tool(
                "cdp_wait_for",
                Some(serde_json::json!({
                    "text": [text],
                    "timeout": timeout_ms
                })),
            )
            .await
            .map_err(|e| super::ExecutorError::ToolCall {
                tool: "cdp_wait_for".into(),
                message: e.to_string(),
            })?;

        if result.is_error == Some(true) {
            // Timeout — wait_for returns error on timeout
            return Ok(serde_json::json!({"found": false}));
        }

        Ok(serde_json::json!({"found": true}))
    }
}
