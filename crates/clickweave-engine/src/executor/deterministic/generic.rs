use super::*;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Generic tool-call tail: convert the (possibly resolved) node type to
    /// an invocation, apply arg massaging (find_text app scoping, image-path
    /// resolution), run the Chrome-profile fast-path for `launch_app` when
    /// applicable, call the tool, apply post-call side effects (launch/focus
    /// bookkeeping, CDP auto-connect, quit_app cleanup, find_text retry),
    /// and assemble the trace event + return value.
    pub(super) async fn execute_generic_tool_call(
        &mut self,
        node_id: Uuid,
        node_type: &NodeType,
        effective: &NodeType,
        mcp: &(impl Mcp + ?Sized),
        mut node_run: Option<&mut NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        let invocation = tool_mapping::node_type_to_tool_invocation(effective)
            .map_err(|e| ExecutorError::Validation(format!("Tool mapping failed: {}", e)))?;
        let tool_name = &invocation.name;

        self.log(format!("Calling MCP tool: {}", tool_name));
        let mut args = self.resolve_image_paths(Some(invocation.arguments));

        // Scope find_text to the focused app when no explicit app_name is set
        if tool_name == "find_text"
            && let Some(ref mut a) = args
            && a.get("app_name").is_none()
            && let Some(app_name) = self.focused_app_name()
        {
            a["app_name"] = serde_json::Value::String(app_name);
        }

        // Save original args for find_text retry fallback (args will be moved into call_tool)
        let find_text_original_args = if tool_name == "find_text" {
            args.clone()
        } else {
            None
        };

        let hints = GenericCallHints::from_args(tool_name, node_type, args.as_ref());

        // For Chrome-family launch_app with a configured profile: kill only the
        // Chrome instance running this profile (leave the user's default Chrome
        // alone), then launch Chrome directly with --user-data-dir. We bypass the
        // MCP launch_app tool which refuses when any Chrome is already running.
        if tool_name == "launch_app"
            && hints.launch_app_kind == AppKind::ChromeBrowser
            && let Some(profile_path) =
                self.resolve_chrome_profile_path(hints.launch_chrome_profile.as_deref())?
        {
            return self
                .execute_chrome_profile_launch(
                    node_id,
                    hints.launch_app_name.as_deref(),
                    hints.launch_app_kind,
                    profile_path,
                    mcp,
                    node_run.as_deref(),
                    retry_ctx,
                )
                .await;
        }

        self.record_event(
            node_run.as_deref(),
            "tool_call",
            serde_json::json!({"name": tool_name, "args": args}),
        );
        let result = mcp
            .call_tool(tool_name, args)
            .await
            .map_err(|e| ExecutorError::ToolCall {
                tool: tool_name.to_string(),
                message: e.to_string(),
            })?;

        Self::check_tool_error(&result, tool_name)?;

        // launch_app implies the app is now focused.
        // Auto-detect app kind from the running process, since the agent
        // may not include app_kind in the launch_app arguments.
        if let Some(name) = &hints.launch_app_name {
            let (detected_kind, detected_pid) = if hints.launch_app_kind == AppKind::Native {
                // Try to detect actual app kind from the running process
                match self.lookup_app_pid(name, mcp).await {
                    Ok(pid) => {
                        let detected = clickweave_core::app_detection::classify_app_by_pid(pid);
                        if detected != AppKind::Native {
                            self.log(format!(
                                "Detected app_kind for '{}': {:?} (pid {})",
                                name, detected, pid
                            ));
                        }
                        (detected, pid)
                    }
                    Err(_) => (AppKind::Native, 0),
                }
            } else {
                if hints.launch_app_kind != AppKind::Native {
                    self.log(format!(
                        "App '{}' has app_kind: {:?}",
                        name, hints.launch_app_kind
                    ));
                }
                // PID lookup not needed when app_kind is already known.
                (hints.launch_app_kind, 0)
            };

            *self.write_focused_app() = Some((name.clone(), detected_kind, detected_pid));

            // Lazy CDP connection for Electron/Chrome apps (same as FocusWindow path).
            if detected_kind.uses_cdp() && mcp.has_tool("cdp_connect") {
                let profile_path =
                    self.resolve_chrome_profile_path_for_app(detected_kind, name, None)?;
                self.ensure_cdp_connected(
                    node_id,
                    name,
                    detected_pid,
                    mcp,
                    node_run.as_deref(),
                    profile_path.as_deref(),
                )
                .await?;
            }
        }

        // Generic McpToolCall focus_window: PID is not resolvable inline,
        // mark focus_dirty so run_loop refreshes kind+PID post-step.
        if let Some(ref app_name) = hints.mcp_focus_window_app {
            *self.write_focused_app() = Some((app_name.clone(), AppKind::Native, 0));
            retry_ctx.focus_dirty = true;
        }

        // quit_app clears focused_app and the shared CDP state when the
        // app being quit is the currently focused or connected app.
        if let Some(ref app_name) = hints.quit_app_name {
            if self.focused_app_name().as_deref() == Some(app_name.as_str())
                || self.focused_app_name().is_none()
            {
                *self.write_focused_app() = None;
            }
            // Clears the active connection (when bound to this app) and
            // every remembered tab URL for any PID of this app name.
            self.cdp_state.mark_app_quit(app_name);
            self.write_app_cache().remove(app_name.as_str());
        }

        let images = self.save_result_images(&result, "result", &mut node_run);
        let result_text = crate::cdp_lifecycle::extract_text(&result);

        // For find_text: if empty matches + available_elements, resolve element name via LLM and retry.
        let find_text_empty = tool_name == "find_text"
            && serde_json::from_str::<Vec<Value>>(&result_text)
                .unwrap_or_default()
                .is_empty();
        let result_text =
            if find_text_empty && let Some(ref original_args) = find_text_original_args {
                self.try_resolve_find_text(
                    node_id,
                    original_args,
                    &result_text,
                    mcp,
                    node_run.as_deref(),
                    retry_ctx.cache_mode,
                )
                .await
                .unwrap_or(result_text)
            } else {
                result_text
            };

        self.record_event(
            node_run.as_deref(),
            "tool_result",
            serde_json::json!({
                "name": tool_name,
                "text": Self::truncate_for_trace(&result_text, 8192),
                "text_len": result_text.len(),
                "image_count": images.len(),
            }),
        );

        self.log(format!(
            "Tool result: {} chars, {} images",
            result_text.len(),
            images.len()
        ));

        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }
}
