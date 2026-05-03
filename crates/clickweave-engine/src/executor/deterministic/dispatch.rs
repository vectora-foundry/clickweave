use super::*;

impl<C: ChatBackend> WorkflowExecutor<C> {
    pub(crate) async fn execute_deterministic(
        &mut self,
        node_id: Uuid,
        node_type: &NodeType,
        mcp: &(impl Mcp + ?Sized),
        mut node_run: Option<&mut NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        retry_ctx.last_tool_result = None;

        // Check CDP scope — nodes that require a CDP connection fail early
        // if no CDP-capable app has been focused.
        if node_type.node_context() == NodeContext::Cdp && !self.cdp_connected_to_focused_app() {
            return Err(ExecutorError::NoCdpConnection {
                node_type: node_type.display_name().to_string(),
            });
        }

        // --- TypeText / PressKey on Chrome/CDP: omnibox URL typing + Enter + navigation wait ---
        // Maintains `retry_ctx.last_typed_url` state and, for the Enter branch,
        // executes the full press_key + cdp_list_pages polling early-return.
        if let Some(result) = self
            .maybe_handle_chrome_url_navigation(node_type, mcp, node_run.as_deref(), retry_ctx)
            .await?
        {
            return Ok(result);
        }

        // --- Hover: CDP path + native fallback + dwell ---
        if let NodeType::Hover(p) = node_type {
            return self
                .execute_hover(node_id, p, mcp, &mut node_run, retry_ctx)
                .await;
        }

        if let NodeType::FindApp(p) = node_type {
            return self.execute_find_app(&p.search, mcp).await;
        }

        if let NodeType::CdpWait(p) = node_type {
            return self.execute_cdp_wait(&p.text, p.timeout_ms, mcp).await;
        }

        // CDP Click: resolve target via snapshot
        if let NodeType::CdpClick(p) = node_type {
            let result_text = self
                .resolve_and_click_cdp(
                    node_id,
                    p.target.as_str(),
                    mcp,
                    node_run.as_deref(),
                    retry_ctx,
                )
                .await?;
            return Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text));
        }

        // CDP Hover: same resolve path as CdpClick
        if let NodeType::CdpHover(p) = node_type {
            let result_text = self
                .resolve_and_hover_cdp(
                    node_id,
                    p.target.as_str(),
                    mcp,
                    node_run.as_deref(),
                    retry_ctx,
                )
                .await?;
            return Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text));
        }

        // CDP Fill: resolve target against the live snapshot so a UID baked in
        // at planning time stays valid after relaunch.
        if let NodeType::CdpFill(p) = node_type {
            return self
                .execute_cdp_fill(p, mcp, node_run.as_deref(), retry_ctx)
                .await;
        }

        // CDP Type: call cdp_type_text directly
        if let NodeType::CdpType(p) = node_type {
            return self
                .execute_cdp_type(p, mcp, node_run.as_deref(), retry_ctx)
                .await;
        }

        // CDP Press Key: call cdp_press_key directly
        if let NodeType::CdpPressKey(p) = node_type {
            return self
                .execute_cdp_press_key(p, mcp, node_run.as_deref(), retry_ctx)
                .await;
        }

        // AX dispatch (macOS): snapshot + descriptor-resolve + dispatch,
        // retrying once on `snapshot_expired`. The executor owns the
        // snapshot lifecycle here because a cached node from a prior run
        // has a uid that is definitely stale, and deterministic replay
        // must re-resolve by role+name.
        if let NodeType::AxClick(p) = node_type {
            return self
                .resolve_and_ax_click(node_id, &p.target, mcp, node_run.as_deref(), retry_ctx)
                .await;
        }
        if let NodeType::AxSetValue(p) = node_type {
            return self
                .resolve_and_ax_set_value(
                    node_id,
                    &p.target,
                    &p.value,
                    mcp,
                    node_run.as_deref(),
                    retry_ctx,
                )
                .await;
        }
        if let NodeType::AxSelect(p) = node_type {
            return self
                .resolve_and_ax_select(node_id, &p.target, mcp, node_run.as_deref(), retry_ctx)
                .await;
        }

        if let NodeType::AppDebugKitOp(p) = node_type {
            return self
                .execute_app_debug_kit_op(p, mcp, node_run.as_deref(), retry_ctx)
                .await;
        }

        if let NodeType::McpToolCall(p) = node_type
            && p.tool_name.is_empty()
        {
            return Err(ExecutorError::Validation(
                "McpToolCall has empty tool_name".to_string(),
            ));
        }

        // Resolve Click targets (window-control / CDP-first / text) and fall
        // through to the generic tool-call path. CDP-first click returns
        // early inside the helper when the CDP path succeeds.
        let resolved_click;
        let effective = match self
            .resolve_click_effective(node_id, node_type, mcp, &mut node_run, retry_ctx)
            .await?
        {
            ClickResolution::EarlyReturn(result_text) => {
                return Self::set_tool_result_and_parse(
                    retry_ctx,
                    ToolResult::from_text(result_text),
                );
            }
            ClickResolution::Resolved(nt) => {
                resolved_click = nt;
                &resolved_click
            }
            ClickResolution::Passthrough => node_type,
        };

        let resolved_fw;
        let effective = match self
            .resolve_focus_window_effective(node_id, effective, mcp, node_run.as_deref(), retry_ctx)
            .await?
        {
            Some(nt) => {
                resolved_fw = nt;
                &resolved_fw
            }
            None => effective,
        };

        let resolved_ss;
        let effective = match self
            .resolve_take_screenshot_effective(
                node_id,
                effective,
                mcp,
                node_run.as_deref(),
                retry_ctx,
            )
            .await?
        {
            Some(nt) => {
                resolved_ss = nt;
                &resolved_ss
            }
            None => effective,
        };

        self.execute_generic_tool_call(node_id, node_type, effective, mcp, node_run, retry_ctx)
            .await
    }
}
