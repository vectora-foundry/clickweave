use super::Mcp;
use super::{ExecutorError, ExecutorResult, WorkflowExecutor};
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
            let err_text = result
                .content
                .first()
                .and_then(|c| c.as_text())
                .unwrap_or("unknown error");
            // Only treat timeout / not-found errors as a normal "not found" result.
            // All other errors (connection failures, protocol errors, etc.) are propagated.
            if err_text.contains("imeout") || err_text.contains("not found") {
                return Ok(serde_json::json!({"found": false}));
            }
            return Err(ExecutorError::ToolCall {
                tool: "cdp_wait_for".into(),
                message: err_text.to_string(),
            });
        }

        Ok(serde_json::json!({"found": true}))
    }
}
