use super::*;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Hover branch: CDP path first (when CDP-capable + connected), native
    /// move_mouse fallback, then dwell for the configured duration.
    pub(super) async fn execute_hover(
        &mut self,
        node_id: Uuid,
        p: &clickweave_core::HoverParams,
        mcp: &(impl Mcp + ?Sized),
        node_run: &mut Option<&mut NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        self.log(format!(
            "Hover: {}",
            NodeType::Hover(p.clone()).action_description()
        ));

        let app_kind = self.focused_app_kind();

        // CDP path: try hover via chrome-devtools-mcp for Electron/Chrome apps
        if app_kind.uses_cdp()
            && self.cdp_connected_to_focused_app()
            && let Some(target) = &p.target
        {
            match self
                .resolve_and_hover_cdp(node_id, target.text(), mcp, node_run.as_deref(), retry_ctx)
                .await
            {
                Ok(result_text) => {
                    self.record_event(
                        node_run.as_deref(),
                        "tool_result",
                        serde_json::json!({
                            "tool": "hover",
                            "method": "cdp",
                            "result": Self::truncate_for_trace(&result_text, 8192),
                        }),
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(p.dwell_ms)).await;
                    return Self::set_tool_result_and_parse(
                        retry_ctx,
                        ToolResult::from_text(result_text),
                    );
                }
                Err(e) => {
                    self.log(format!("CDP hover failed, falling back to native: {e}"));
                }
            }
        }

        // Native path: resolve text target to coordinates, then move_mouse + dwell
        let owned_hover_type = NodeType::Hover(p.clone());
        let resolved_hover;
        let effective = if matches!(&p.target, Some(clickweave_core::ClickTarget::Text { .. })) {
            resolved_hover = self
                .resolve_hover_target(node_id, mcp, p, node_run, retry_ctx)
                .await?;
            &resolved_hover
        } else {
            &owned_hover_type
        };

        let inv = tool_mapping::node_type_to_tool_invocation(effective)
            .map_err(|e| ExecutorError::Validation(e.to_string()))?;

        self.record_event(
            node_run.as_deref(),
            "tool_call",
            serde_json::json!({"name": inv.name, "args": &inv.arguments}),
        );

        let result = mcp
            .call_tool(&inv.name, Some(inv.arguments))
            .await
            .map_err(|e| ExecutorError::ToolCall {
                tool: inv.name.clone(),
                message: e.to_string(),
            })?;

        Self::check_tool_error(&result, &inv.name)?;
        let result_text = crate::cdp_lifecycle::extract_text(&result);

        self.record_event(
            node_run.as_deref(),
            "tool_result",
            serde_json::json!({
                "name": inv.name,
                "text": Self::truncate_for_trace(&result_text, 8192),
                "text_len": result_text.len(),
            }),
        );

        // Dwell: hold position for the configured duration
        tokio::time::sleep(tokio::time::Duration::from_millis(p.dwell_ms)).await;

        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }
}
