use super::*;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Handle the Chrome/CDP URL-typing + Enter flow. On TypeText the helper
    /// only updates `retry_ctx.last_typed_url` and returns `None`; on the
    /// Enter variant following a typed URL it runs the full press_key +
    /// cdp_list_pages polling loop and returns `Some(result)` so the caller
    /// can short-circuit. The `None` branch also clears `last_typed_url` for
    /// every non-matching node type — ordering identical to the original.
    pub(super) async fn maybe_handle_chrome_url_navigation(
        &mut self,
        node_type: &NodeType,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Option<Value>> {
        if let NodeType::TypeText(p) = node_type {
            let app_kind = self.focused_app_kind();
            if app_kind == AppKind::ChromeBrowser
                && self.cdp_connected_to_focused_app()
                && looks_like_browser_url_input(&p.text)
            {
                // Store the text so the subsequent press_key return knows to wait
                // for Chrome to visually start loading before supervision fires.
                retry_ctx.last_typed_url = Some(p.text.clone());

                // Make URL typing idempotent on retries/reruns: bring Chrome to
                // front and focus/select the omnibox before typing.
                if let Some(app_name) = self.focused_app_name() {
                    let _ = mcp
                        .call_tool(
                            "focus_window",
                            Some(serde_json::json!({"app_name": app_name})),
                        )
                        .await;
                }
                #[cfg(target_os = "macos")]
                let modifiers = vec!["command"];
                #[cfg(not(target_os = "macos"))]
                let modifiers = vec!["control"];
                let _ = mcp
                    .call_tool(
                        "press_key",
                        Some(serde_json::json!({
                            "key": "l",
                            "modifiers": modifiers,
                        })),
                    )
                    .await;
            } else {
                retry_ctx.last_typed_url = None;
            }
            return Ok(None);
        }

        if let NodeType::PressKey(p) = node_type {
            let app_kind = self.focused_app_kind();
            if app_kind == AppKind::ChromeBrowser
                && self.cdp_connected_to_focused_app()
                && is_return_key(&p.key)
                && p.modifiers.is_empty()
                && retry_ctx.last_typed_url.is_some()
            {
                let value = self
                    .execute_chrome_url_press_key_enter(mcp, node_run, retry_ctx)
                    .await?;
                return Ok(Some(value));
            }
            retry_ctx.last_typed_url = None;
            return Ok(None);
        }

        retry_ctx.last_typed_url = None;
        Ok(None)
    }

    /// Run the Chrome-URL Enter path: re-focus the omnibox, fire press_key
    /// Return, then poll `cdp_list_pages` until a navigation-like transition
    /// is observed or the deadline elapses.
    pub(super) async fn execute_chrome_url_press_key_enter(
        &mut self,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        // Re-focus the target app before sending Enter. In Test mode,
        // per-step screenshot/supervision can occasionally leave key
        // focus elsewhere, causing Enter to miss Chrome.
        if let Some(app_name) = self.focused_app_name() {
            let _ = mcp
                .call_tool(
                    "focus_window",
                    Some(serde_json::json!({"app_name": app_name})),
                )
                .await;
        }

        // URL was just typed into the Chrome Omnibox. Fire the native
        // press_key return (which Chrome handles as Omnibox navigation),
        // then poll cdp_list_pages until the URL changes away from NTP.
        //
        // We cannot use cdp_navigate here: Chrome's NTP auto-focuses the
        // Omnibox, which causes Chrome to silently ignore Page.navigate
        // CDP commands, making cdp_navigate always time out.
        let navigation_baseline = match mcp
            .call_tool("cdp_list_pages", Some(serde_json::json!({})))
            .await
        {
            Ok(r) if r.is_error != Some(true) => {
                let text = crate::cdp_lifecycle::extract_text(&r);
                // Only use the baseline if it contains at least one
                // parseable page entry. An empty map would cause every
                // HTTP tab in the next poll to look "new".
                if parse_cdp_page_payloads(&text).is_empty() {
                    self.log(
                        "Chrome URL navigation: baseline has no page entries — \
                         navigation observation disabled",
                    );
                    None
                } else {
                    Some(text)
                }
            }
            _ => {
                self.log(
                    "Chrome URL navigation: baseline cdp_list_pages failed — \
                     navigation observation disabled",
                );
                None
            }
        };

        let press_args = serde_json::json!({"key": "return"});
        self.record_event(
            node_run,
            "tool_call",
            serde_json::json!({"name": "press_key", "args": &press_args}),
        );
        let result = mcp
            .call_tool("press_key", Some(press_args))
            .await
            .map_err(|e| ExecutorError::ToolCall {
                tool: "press_key".to_string(),
                message: e.to_string(),
            })?;
        Self::check_tool_error(&result, "press_key")?;
        let result_text = crate::cdp_lifecycle::extract_text(&result);
        self.record_event(
            node_run,
            "tool_result",
            serde_json::json!({
                "name": "press_key",
                "text": Self::truncate_for_trace(&result_text, 8192),
            }),
        );

        // Poll cdp_list_pages until Chrome moves away from NTP/blank.
        // This gives a structural "navigation started" signal without
        // waiting for full page load, which can be long on Gmail/YouTube.
        //
        // We skip the observation loop when the baseline is unavailable:
        // without a before-snapshot we cannot distinguish existing tabs
        // from newly-navigated ones (every http tab would look "new").
        if let Some(ref baseline) = navigation_baseline {
            self.log("Chrome URL navigation: polling for URL change...");
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            let mut poll_ms: u64 = 100;
            // Poll until the URL changes or the deadline expires.
            // last_typed_url stays armed through supervision retries
            // (cleared by run_loop after supervision passes) so that a
            // false-failure retry still enters the navigation-aware
            // PressKey path instead of sending a raw Enter to the
            // destination page.
            loop {
                if self.cancel_token.is_cancelled() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
                poll_ms = (poll_ms * 2).min(500);
                if tokio::time::Instant::now() >= deadline {
                    self.log("Chrome URL navigation: timeout waiting for URL change");
                    break;
                }
                if let Ok(r) = mcp
                    .call_tool("cdp_list_pages", Some(serde_json::json!({})))
                    .await
                    && r.is_error != Some(true)
                {
                    let text = crate::cdp_lifecycle::extract_text(&r);
                    if cdp_pages_show_navigation_progress(baseline, &text) {
                        self.log("Chrome URL navigation: page URL changed");
                        break;
                    }
                }
            }
        } else {
            self.log("Chrome URL navigation: baseline unavailable, skipping observation");
        }

        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }

    /// Chrome-profile launch: kills only the profile-scoped Chrome instance,
    /// spawns Chrome directly with `--user-data-dir` (optionally with a
    /// debug port when CDP is available), and wires up CDP when needed.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_chrome_profile_launch(
        &mut self,
        node_id: Uuid,
        launch_app_name: Option<&str>,
        launch_app_kind: AppKind,
        profile_path: std::path::PathBuf,
        mcp: &(impl Mcp + ?Sized),
        node_run: Option<&NodeRun>,
        retry_ctx: &mut RetryContext,
    ) -> ExecutorResult<Value> {
        let dir = profile_path.to_string_lossy().to_string();
        self.log(format!("Launching Chrome with profile: {}", dir));

        let use_cdp = launch_app_kind.uses_cdp() && mcp.has_tool("cdp_connect");

        if !use_cdp {
            // No CDP available: launch now without debug port.
            kill_chrome_profile_instance(&dir).await;
            launch_chrome_with_profile(&dir)
                .await
                .map_err(|e| ExecutorError::ToolCall {
                    tool: "launch_app".to_string(),
                    message: format!("Failed to launch Chrome with profile: {e}"),
                })?;
            // Wait for Chrome to start up before continuing.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        self.record_event(
            node_run,
            "tool_call",
            serde_json::json!({
                "name": "launch_app",
                "args": {"app_name": launch_app_name, "user_data_dir": dir},
            }),
        );

        if let Some(name) = launch_app_name {
            // PID is not yet available immediately after launch; use 0 as placeholder.
            *self.write_focused_app() = Some((name.to_string(), launch_app_kind, 0));
            if use_cdp {
                // Force-disconnect any existing CDP session: a new profile
                // launch kills the previous Chrome instance, so any old CDP
                // connection is stale. Without this, ensure_cdp_connected
                // short-circuits on the app name match and never connects
                // to the new profile's Chrome instance.
                if let Some((prev_name, _)) = self.cdp_state.take_connected() {
                    best_effort::best_effort_tool_call(
                        mcp,
                        "cdp_disconnect",
                        None,
                        "launch_app profile branch: force-disconnect before relaunch",
                    )
                    .await;
                    // The app was about to be killed for a profile
                    // relaunch — forget every remembered tab URL for any
                    // instance of this app name; they're all stale after
                    // the kill. The active-connection slot was already
                    // cleared by `take_connected`.
                    self.cdp_state.mark_app_quit(&prev_name);
                }
                self.ensure_cdp_connected(
                    node_id,
                    name,
                    0,
                    mcp,
                    node_run,
                    Some(profile_path.as_path()),
                )
                .await?;
            }
        }

        let result_text = format!("Launched Chrome with profile {}", dir);
        self.record_event(
            node_run,
            "tool_result",
            serde_json::json!({"name": "launch_app", "text": &result_text}),
        );
        Self::set_tool_result_and_parse(retry_ctx, ToolResult::from_text(result_text))
    }
}
