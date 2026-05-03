use super::*;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Call the AppDebugKit-operation tool by name with raw parameters.
    pub(super) async fn execute_app_debug_kit_op(
        &mut self,
        p: &clickweave_core::AppDebugKitParams,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        self.log(format!("AppDebugKit operation: {}", p.operation_name));
        let args = if p.parameters.is_null() {
            None
        } else {
            Some(p.parameters.clone())
        };
        self.record_event(
            node_run,
            "tool_call",
            serde_json::json!({"name": p.operation_name, "args": args}),
        );
        let result =
            mcp.call_tool(&p.operation_name, args)
                .await
                .map_err(|e| ExecutorError::ToolCall {
                    tool: p.operation_name.clone(),
                    message: e.to_string(),
                })?;
        Self::check_tool_error(&result, &p.operation_name)?;
        let result_text = crate::cdp_lifecycle::extract_text(&result);
        self.record_event(
            node_run,
            "tool_result",
            serde_json::json!({
                "name": p.operation_name,
                "text": Self::truncate_for_trace(&result_text, 8192),
                "text_len": result_text.len(),
            }),
        );
        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }
}
