use super::*;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Resolve a Click node into either an early-return result (CDP fast
    /// path succeeded), a rewritten NodeType (coords resolved), or a
    /// passthrough (not a click-with-target — leave as-is).
    pub(super) async fn resolve_click_effective(
        &mut self,
        node_id: Uuid,
        node_type: &NodeType,
        mcp: &(impl Mcp + ?Sized),
        node_run: &mut Option<&mut NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<ClickResolution> {
        let NodeType::Click(p) = node_type else {
            return Ok(ClickResolution::Passthrough);
        };
        if let Some(clickweave_core::ClickTarget::WindowControl { action }) = &p.target {
            // Window control buttons are resolved to window-relative coordinates.
            let resolved = self
                .resolve_window_control_click(*action, mcp, p, node_run)
                .await?;
            return Ok(ClickResolution::Resolved(resolved));
        }
        if matches!(&p.target, Some(clickweave_core::ClickTarget::Text { .. })) {
            // For Electron/Chrome apps, try CDP click first (snapshot + uid click).
            let click_target = p.target.as_ref().ok_or_else(|| {
                ExecutorError::ClickTarget(
                    "Click::target vanished between match and unwrap".to_string(),
                )
            })?;
            let target = click_target.text();
            let app_kind = self.focused_app_kind();

            if app_kind.uses_cdp() && self.cdp_connected_to_focused_app() {
                match self
                    .resolve_and_click_cdp(node_id, target, mcp, node_run.as_deref(), retry_ctx)
                    .await
                {
                    Ok(result_text) => {
                        self.record_event(
                            node_run.as_deref(),
                            "tool_result",
                            serde_json::json!({
                                "tool": "click",
                                "method": "cdp",
                                "result": Self::truncate_for_trace(&result_text, 8192),
                            }),
                        );
                        return Ok(ClickResolution::EarlyReturn(result_text));
                    }
                    Err(e) => {
                        self.log(format!("CDP click failed, falling back to native: {e}"));
                    }
                }
            }

            let resolved = self
                .resolve_click_target(node_id, mcp, p, node_run, retry_ctx)
                .await?;
            return Ok(ClickResolution::Resolved(resolved));
        }
        Ok(ClickResolution::Passthrough)
    }

    /// Resolve a FocusWindow node with an AppName target: resolve the app,
    /// upgrade its kind, lazily connect CDP for Electron/Chrome, update
    /// `focused_app`, and rewrite the node to a PID target.
    ///
    /// Returns `None` for any other shape so the caller can keep the
    /// current `effective`.
    pub(super) async fn resolve_focus_window_effective(
        &mut self,
        node_id: Uuid,
        effective: &NodeType,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<Option<NodeType>> {
        let NodeType::FocusWindow(p) = effective else {
            return Ok(None);
        };
        let FocusTarget::AppName(user_input) = &p.target else {
            return Ok(None);
        };
        if user_input.is_empty() {
            return Ok(None);
        }
        let user_input = user_input.as_str();
        let mut app = self
            .resolve_app_name(node_id, user_input, mcp, node_run, retry_ctx.cache_mode)
            .await?;
        // Upgrade app_kind if the node says Native but detection disagrees.
        let app_kind = if p.app_kind == AppKind::Native {
            let detected = clickweave_core::app_detection::classify_app_by_pid(app.pid);
            if detected != AppKind::Native {
                self.log(format!(
                    "Upgraded app_kind for '{}' from Native to {:?}",
                    app.name, detected
                ));
            }
            detected
        } else {
            p.app_kind
        };

        // Lazy CDP connection for Electron/Chrome apps.
        if app_kind.uses_cdp() && mcp.has_tool("cdp_connect") {
            let profile_path = self.resolve_chrome_profile_path_for_app(
                app_kind,
                &app.name,
                p.chrome_profile_id.as_deref(),
            )?;
            self.ensure_cdp_connected(
                node_id,
                &app.name,
                app.pid,
                mcp,
                node_run,
                profile_path.as_deref(),
            )
            .await?;
            // Re-resolve PID -- it may have changed if the app was relaunched.
            app = self
                .resolve_app_name(node_id, user_input, mcp, node_run, retry_ctx.cache_mode)
                .await?;
            // Sync the CDP connection PID to the freshly resolved PID.
            // `ensure_cdp_connected` ran above with the pre-resolve PID;
            // if the resolver now reports a different PID (typical after
            // a relaunch that picked up a new process), rebind the
            // stored identity to the new PID so later lookups match.
            self.cdp_state.rebind_pid(&app.name, app.pid);
        }

        *self.write_focused_app() = Some((app.name.clone(), app_kind, app.pid));

        // `app.pid` is i32 from the MCP app listing; coerce to u32 for the
        // typed target. Negative/overflow values fall back to the resolved
        // app name so the downstream tool mapping still targets the correct
        // app (the executor treats an empty AppName as "no target" only).
        let pid_target = u32::try_from(app.pid)
            .map(FocusTarget::Pid)
            .unwrap_or_else(|_| FocusTarget::AppName(app.name.clone()));
        Ok(Some(NodeType::FocusWindow(FocusWindowParams {
            target: pid_target,
            bring_to_front: p.bring_to_front,
            app_kind,
            chrome_profile_id: p.chrome_profile_id.clone(),
            ..Default::default()
        })))
    }

    /// Resolve a TakeScreenshot node with `mode=Window` and a user-supplied
    /// app-name target: re-resolve the app and return a rewritten node with
    /// the canonical name. Returns `None` otherwise.
    pub(super) async fn resolve_take_screenshot_effective(
        &mut self,
        node_id: Uuid,
        effective: &NodeType,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<Option<NodeType>> {
        let NodeType::TakeScreenshot(p) = effective else {
            return Ok(None);
        };
        if p.target.is_none() || p.mode != ScreenshotMode::Window {
            return Ok(None);
        }
        let user_input = p.target.as_deref().ok_or_else(|| {
            ExecutorError::Validation(
                "TakeScreenshot target vanished between check and unwrap".to_string(),
            )
        })?;
        let app = self
            .resolve_app_name(node_id, user_input, mcp, node_run, retry_ctx.cache_mode)
            .await?;
        Ok(Some(NodeType::TakeScreenshot(TakeScreenshotParams {
            mode: p.mode,
            target: Some(app.name.clone()),
            include_ocr: p.include_ocr,
        })))
    }
}
