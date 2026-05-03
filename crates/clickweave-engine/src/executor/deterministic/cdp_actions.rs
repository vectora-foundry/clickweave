use super::*;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Call `cdp_fill` with a snapshot-resolved uid.
    pub(super) async fn execute_cdp_fill(
        &mut self,
        p: &clickweave_core::CdpFillParams,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        let uid = self
            .resolve_cdp_target_uid_with_overrides(&p.target, mcp, Some(retry_ctx))
            .await?;
        let args = serde_json::json!({"uid": uid, "value": p.value});
        self.record_event(
            node_run,
            "tool_call",
            serde_json::json!({"name": "cdp_fill", "args": &args}),
        );
        let result =
            mcp.call_tool("cdp_fill", Some(args))
                .await
                .map_err(|e| ExecutorError::ToolCall {
                    tool: "cdp_fill".to_string(),
                    message: e.to_string(),
                })?;
        Self::check_tool_error(&result, "cdp_fill")?;
        let result_text = crate::cdp_lifecycle::extract_text(&result);
        self.record_event(
            node_run,
            "tool_result",
            serde_json::json!({
                "name": "cdp_fill",
                "text": Self::truncate_for_trace(&result_text, 8192),
                "text_len": result_text.len(),
            }),
        );
        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }

    /// Call `cdp_type_text` with the provided text.
    pub(super) async fn execute_cdp_type(
        &mut self,
        p: &clickweave_core::CdpTypeParams,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        let args = serde_json::json!({"text": p.text});
        self.record_event(
            node_run,
            "tool_call",
            serde_json::json!({"name": "cdp_type_text", "args": &args}),
        );
        let result = mcp
            .call_tool("cdp_type_text", Some(args))
            .await
            .map_err(|e| ExecutorError::ToolCall {
                tool: "cdp_type_text".to_string(),
                message: e.to_string(),
            })?;
        Self::check_tool_error(&result, "cdp_type_text")?;
        let result_text = crate::cdp_lifecycle::extract_text(&result);
        self.record_event(
            node_run,
            "tool_result",
            serde_json::json!({
                "name": "cdp_type_text",
                "text": Self::truncate_for_trace(&result_text, 8192),
                "text_len": result_text.len(),
            }),
        );
        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }

    /// Call `cdp_press_key` with the provided key and optional modifiers.
    pub(super) async fn execute_cdp_press_key(
        &mut self,
        p: &clickweave_core::CdpPressKeyParams,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        let mut args = serde_json::json!({"key": p.key});
        if !p.modifiers.is_empty() {
            args["modifiers"] = serde_json::json!(p.modifiers);
        }
        self.record_event(
            node_run,
            "tool_call",
            serde_json::json!({"name": "cdp_press_key", "args": &args}),
        );
        let result = mcp
            .call_tool("cdp_press_key", Some(args))
            .await
            .map_err(|e| ExecutorError::ToolCall {
                tool: "cdp_press_key".to_string(),
                message: e.to_string(),
            })?;
        Self::check_tool_error(&result, "cdp_press_key")?;
        let result_text = crate::cdp_lifecycle::extract_text(&result);
        self.record_event(
            node_run,
            "tool_result",
            serde_json::json!({
                "name": "cdp_press_key",
                "text": Self::truncate_for_trace(&result_text, 8192),
                "text_len": result_text.len(),
            }),
        );
        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }
}
