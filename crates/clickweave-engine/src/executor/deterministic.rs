use super::{ExecutorError, ExecutorResult, WorkflowExecutor};
use clickweave_core::decision_cache::cache_key;
use clickweave_core::walkthrough::AppKind;
use clickweave_core::{
    ClickParams, FocusMethod, FocusWindowParams, NodeRun, NodeType, ScreenshotMode,
    TakeScreenshotParams, tool_mapping,
};
use clickweave_llm::ChatBackend;
use clickweave_mcp::{McpRouter, ToolCallResult};
use serde_json::Value;
use uuid::Uuid;

impl<C: ChatBackend> WorkflowExecutor<C> {
    pub(crate) async fn execute_deterministic(
        &mut self,
        node_id: Uuid,
        node_type: &NodeType,
        mcp: &mut McpRouter,
        mut node_run: Option<&mut NodeRun>,
    ) -> ExecutorResult<Value> {
        // Reset per-execution; set to true only on CDP click success.
        self.last_click_was_cdp = false;

        if let NodeType::AppDebugKitOp(p) = node_type {
            self.log(format!("AppDebugKit operation: {}", p.operation_name));
            let args = if p.parameters.is_null() {
                None
            } else {
                Some(p.parameters.clone())
            };
            self.record_event(
                node_run.as_deref(),
                "tool_call",
                serde_json::json!({"name": p.operation_name, "args": args}),
            );
            let result = mcp.call_tool(&p.operation_name, args).await.map_err(|e| {
                ExecutorError::ToolCall {
                    tool: p.operation_name.clone(),
                    message: e.to_string(),
                }
            })?;
            Self::check_tool_error(&result, &p.operation_name)?;
            let result_text = Self::extract_result_text(&result);
            self.record_event(
                node_run.as_deref(),
                "tool_result",
                serde_json::json!({
                    "name": p.operation_name,
                    "text": Self::truncate_for_trace(&result_text, 8192),
                    "text_len": result_text.len(),
                }),
            );
            return Ok(Self::parse_result_text(&result_text));
        }

        if let NodeType::McpToolCall(p) = node_type
            && p.tool_name.is_empty()
        {
            return Err(ExecutorError::Validation(
                "McpToolCall has empty tool_name".to_string(),
            ));
        }

        let resolved_click;
        let effective = if let NodeType::Click(p) = node_type
            && let Some(clickweave_core::ClickTarget::WindowControl { action }) = &p.target
        {
            // Window control buttons are resolved to window-relative coordinates.
            resolved_click = self
                .resolve_window_control_click(*action, mcp, p, &mut node_run)
                .await?;
            &resolved_click
        } else if let NodeType::Click(p) = node_type
            && p.template_image.is_some()
            && p.x.is_none()
        {
            resolved_click = self
                .resolve_click_target_by_image(node_id, mcp, p, &mut node_run)
                .await?;
            &resolved_click
        } else if let NodeType::Click(p) = node_type
            && p.target.is_some()
            && p.x.is_none()
        {
            // For Electron/Chrome apps, try CDP click first (snapshot + uid click).
            let click_target = p.target.as_ref().unwrap();
            let target = click_target.text();
            let app_kind = self.focused_app_kind();

            if app_kind.uses_cdp()
                && let Some(cdp_server) = self.focused_cdp_server()
            {
                let (expected_role, expected_href, expected_parent_role, expected_parent_name) =
                    match click_target {
                        clickweave_core::ClickTarget::CdpElement {
                            role,
                            href,
                            parent_role,
                            parent_name,
                            ..
                        } => (
                            role.as_deref(),
                            href.as_deref(),
                            parent_role.as_deref(),
                            parent_name.as_deref(),
                        ),
                        _ => (None, None, None, None),
                    };
                match self
                    .resolve_and_click_cdp(
                        target,
                        expected_role,
                        expected_href,
                        expected_parent_role,
                        expected_parent_name,
                        &cdp_server,
                        mcp,
                        node_run.as_deref(),
                    )
                    .await
                {
                    Ok(result_text) => {
                        self.last_click_was_cdp = true;
                        self.record_event(
                            node_run.as_deref(),
                            "tool_result",
                            serde_json::json!({
                                "tool": "click",
                                "method": "cdp",
                                "result": Self::truncate_for_trace(&result_text, 8192),
                            }),
                        );
                        return Ok(Self::parse_result_text(&result_text));
                    }
                    Err(e) => {
                        self.log(format!("CDP click failed, falling back to native: {e}"));
                    }
                }
            }

            resolved_click = self
                .resolve_click_target(node_id, mcp, p, &mut node_run)
                .await?;
            &resolved_click
        } else {
            node_type
        };

        let resolved_fw;
        let effective = if let NodeType::FocusWindow(p) = effective
            && p.method == FocusMethod::AppName
            && p.value.is_some()
        {
            let user_input = p.value.as_deref().unwrap();
            let mut app = self
                .resolve_app_name(node_id, user_input, mcp, node_run.as_deref())
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

            // Lazy CDP spawn for Electron/Chrome apps.
            if app_kind.uses_cdp() {
                self.ensure_cdp_server(node_id, &app.name, mcp, node_run.as_deref())
                    .await?;
                // Re-resolve PID — it may have changed if the app was relaunched.
                app = self
                    .resolve_app_name(node_id, user_input, mcp, node_run.as_deref())
                    .await?;
            }

            *self.focused_app.write().unwrap_or_else(|e| e.into_inner()) =
                Some((app.name.clone(), app_kind));

            resolved_fw = NodeType::FocusWindow(FocusWindowParams {
                method: FocusMethod::Pid,
                value: Some(app.pid.to_string()),
                bring_to_front: p.bring_to_front,
                app_kind,
            });
            &resolved_fw
        } else {
            effective
        };

        let resolved_ss;
        let effective = if let NodeType::TakeScreenshot(p) = effective
            && p.target.is_some()
            && p.mode == ScreenshotMode::Window
        {
            let user_input = p.target.as_deref().unwrap();
            let app = self
                .resolve_app_name(node_id, user_input, mcp, node_run.as_deref())
                .await?;
            resolved_ss = NodeType::TakeScreenshot(TakeScreenshotParams {
                mode: p.mode,
                target: Some(app.name.clone()),
                include_ocr: p.include_ocr,
            });
            &resolved_ss
        } else {
            effective
        };

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

        // Extract app_name and app_kind before args is moved into call_tool
        let launch_app_name = if tool_name == "launch_app" {
            args.as_ref()
                .and_then(|a| a.get("app_name"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        };

        let launch_app_kind = if tool_name == "launch_app" {
            args.as_ref()
                .and_then(|a| a.get("app_kind"))
                .and_then(|v| v.as_str())
                .and_then(AppKind::parse)
                .unwrap_or(AppKind::Native)
        } else {
            AppKind::Native
        };

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

        // launch_app implies the app is now focused
        if let Some(name) = &launch_app_name {
            *self.focused_app.write().unwrap_or_else(|e| e.into_inner()) =
                Some((name.clone(), launch_app_kind));

            if launch_app_kind != AppKind::Native {
                self.log(format!(
                    "App '{}' has app_kind: {:?}",
                    name, launch_app_kind
                ));
            }

            // Lazy CDP spawn for Electron/Chrome apps (same as FocusWindow path).
            if launch_app_kind.uses_cdp() {
                self.ensure_cdp_server(node_id, name, mcp, node_run.as_deref())
                    .await?;
            }
        }

        let images = self.save_result_images(&result, "result", &mut node_run);
        let result_text = Self::extract_result_text(&result);

        // For find_text: if empty matches + available_elements, resolve element name via LLM and retry.
        // Skip resolution inside loops — FindText nodes in loops act as condition checks
        // where accurate found/not-found results are needed for exit conditions.
        // Element resolution would map e.g. "128" → "8" (a button), masking the fact
        // that "128" is not yet on screen and preventing the loop from exiting.
        let inside_loop = !self.context.loop_counters.is_empty();
        let find_text_empty = tool_name == "find_text"
            && serde_json::from_str::<Vec<Value>>(&result_text)
                .unwrap_or_default()
                .is_empty();
        let result_text = if find_text_empty
            && !inside_loop
            && let Some(ref original_args) = find_text_original_args
        {
            self.try_resolve_find_text(
                node_id,
                original_args,
                &result_text,
                mcp,
                node_run.as_deref(),
            )
            .await
            .unwrap_or(result_text)
        } else if find_text_empty && inside_loop {
            // Inside loops, return just the empty array so parse_result_text
            // produces Value::Array([]) and extract_result_variables sets found=false.
            "[]".to_string()
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

        Ok(Self::parse_result_text(&result_text))
    }

    fn check_tool_error(result: &ToolCallResult, tool_name: &str) -> ExecutorResult<()> {
        if result.is_error == Some(true) {
            let error_text = Self::extract_result_text(result);
            return Err(ExecutorError::ToolCall {
                tool: tool_name.to_string(),
                message: error_text,
            });
        }
        Ok(())
    }

    /// Parse a tool result text as JSON, falling back to a string Value or Null.
    fn parse_result_text(text: &str) -> Value {
        serde_json::from_str(text).unwrap_or(if text.is_empty() {
            Value::Null
        } else {
            Value::String(text.to_string())
        })
    }

    /// Try to resolve a failed find_text query by asking the LLM to match
    /// against available accessibility element names, then retry.
    /// Returns the retry result text on success, or None if resolution wasn't
    /// possible or the retry also failed.
    ///
    /// Preserves the original call arguments (e.g. `app_name`, `match_mode`)
    /// and only replaces the `text` field with the resolved name.
    async fn try_resolve_find_text(
        &self,
        node_id: Uuid,
        original_args: &Value,
        original_result_text: &str,
        mcp: &McpRouter,
        node_run: Option<&NodeRun>,
    ) -> Option<String> {
        let retry_args = self
            .prepare_find_text_retry(node_id, original_args, original_result_text, node_run)
            .await?;
        let retry_result = mcp.call_tool("find_text", Some(retry_args)).await.ok()?;
        if retry_result.is_error == Some(true) {
            return None;
        }
        Some(Self::extract_result_text(&retry_result))
    }

    /// Parse available_elements from a failed find_text response, resolve the
    /// element name via LLM, and build retry arguments.
    ///
    /// Returns `Some(retry_args)` with the resolved name swapped in, or `None`
    /// if resolution wasn't possible. This is the pure-logic core of the
    /// find_text fallback path, separated from the MCP I/O for testability.
    pub(crate) async fn prepare_find_text_retry(
        &self,
        node_id: Uuid,
        original_args: &Value,
        original_result_text: &str,
        node_run: Option<&NodeRun>,
    ) -> Option<Value> {
        let target = original_args.get("text")?.as_str()?;
        let available = super::element_resolve::parse_available_elements(original_result_text)?;
        // Prefer explicit app_name from call args; fall back to focused_app.
        let scoped_app = original_args
            .get("app_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| self.focused_app_name());
        let resolved_name = self
            .resolve_element_name(node_id, target, &available, scoped_app.as_deref(), node_run)
            .await
            .ok()?;

        self.log(format!(
            "Retrying find_text with resolved name '{}' for '{}'",
            resolved_name, target
        ));

        let mut retry_args = original_args.clone();
        retry_args["text"] = Value::String(resolved_name);
        Some(retry_args)
    }

    async fn resolve_click_target(
        &self,
        node_id: Uuid,
        mcp: &McpRouter,
        params: &ClickParams,
        node_run: &mut Option<&mut NodeRun>,
    ) -> ExecutorResult<NodeType> {
        let target = params.target.as_ref().map(|t| t.text()).ok_or_else(|| {
            ExecutorError::ClickTarget("resolve_click_target called with no target".to_string())
        })?;

        let scoped_app = self.focused_app_name();

        // Use cached element resolution if available (e.g. × → Multiply) to
        // avoid matching display text that happens to contain the symbol.
        let element_key = (target.to_string(), scoped_app.clone());
        let search_text = self
            .element_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&element_key)
            .cloned()
            .unwrap_or_else(|| target.to_string());

        let mut find_args = serde_json::json!({"text": search_text});
        if let Some(ref app_name) = scoped_app {
            find_args["app_name"] = serde_json::Value::String(app_name.clone());
        }

        match &scoped_app {
            Some(app) => self.log(format!("Resolving click target: '{}' in '{}'", target, app)),
            None => self.log(format!(
                "Resolving click target: '{}' (screen-wide)",
                target
            )),
        }

        let find_result = mcp
            .call_tool("find_text", Some(find_args.clone()))
            .await
            .map_err(|e| {
                ExecutorError::ClickTarget(format!("find_text for '{}' failed: {}", target, e))
            })?;

        Self::check_tool_error(&find_result, "find_text")?;

        let result_text = Self::extract_result_text(&find_result);
        let mut matches: Vec<Value> = serde_json::from_str(&result_text).unwrap_or_default();

        // Fallback: if no matches but available_elements present, ask LLM to resolve
        if matches.is_empty()
            && let Some(retry_text) = self
                .try_resolve_find_text(node_id, &find_args, &result_text, mcp, node_run.as_deref())
                .await
        {
            matches = serde_json::from_str(&retry_text).unwrap_or_default();
        }

        let best = if matches.is_empty() {
            return Err(ExecutorError::ClickTarget(format!(
                "Could not find text '{}' on screen (find_text returned: {})",
                target,
                truncate_for_error(&result_text, 120),
            )));
        } else if matches.len() == 1 {
            &matches[0]
        } else {
            // Check decision cache first
            let ck = cache_key(node_id, target, scoped_app.as_deref());
            let cached_idx = self
                .decision_cache
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .click_disambiguation
                .get(&ck)
                .and_then(|cached| {
                    matches.iter().position(|m| {
                        m["text"].as_str() == Some(cached.chosen_text.as_str())
                            && m["role"].as_str() == Some(cached.chosen_role.as_str())
                    })
                });

            let idx = if let Some(idx) = cached_idx {
                self.log(format!("Using cached disambiguation for '{}'", target));
                idx
            } else {
                self.disambiguate_click_matches(
                    node_id,
                    target,
                    &matches,
                    scoped_app.as_deref(),
                    node_run.as_deref(),
                )
                .await?
            };
            &matches[idx]
        };

        let x = best["x"].as_f64().ok_or_else(|| {
            ExecutorError::ClickTarget(format!(
                "find_text match for '{}' missing 'x' coordinate",
                target
            ))
        })?;
        let y = best["y"].as_f64().ok_or_else(|| {
            ExecutorError::ClickTarget(format!(
                "find_text match for '{}' missing 'y' coordinate",
                target
            ))
        })?;
        let matched_text = best["text"].as_str().unwrap_or(target);

        self.log(format!(
            "Resolved target '{}' -> ({}, {}) from '{}'",
            target, x, y, matched_text
        ));

        self.record_event(
            node_run.as_deref(),
            "target_resolved",
            serde_json::json!({
                "target": target,
                "x": x,
                "y": y,
                "matched_text": matched_text,
                "app_name": scoped_app,
            }),
        );

        Ok(NodeType::Click(ClickParams {
            target: params.target.clone(),
            x: Some(x),
            y: Some(y),
            button: params.button,
            click_count: params.click_count,
            ..Default::default()
        }))
    }

    async fn resolve_click_target_by_image(
        &self,
        _node_id: Uuid,
        mcp: &McpRouter,
        params: &ClickParams,
        node_run: &mut Option<&mut NodeRun>,
    ) -> ExecutorResult<NodeType> {
        let b64 = params.template_image.as_deref().ok_or_else(|| {
            ExecutorError::ClickTarget(
                "resolve_click_target_by_image called without template_image".to_string(),
            )
        })?;

        self.log("Resolving click target by image template".to_string());

        // Take a screenshot first — find_image needs both a template and a
        // screenshot to search within. Use screenshot_id when available so
        // find_image has the screenshot metadata for screen coordinate conversion.
        let app_name = self.focused_app_name();
        let screenshot_args = match &app_name {
            Some(name) => serde_json::json!({ "app_name": name }),
            None => serde_json::json!({}),
        };
        let (screenshot_b64, screenshot_id) = self
            .take_screenshot_with_id(mcp, screenshot_args)
            .await
            .ok_or(ExecutorError::ClickTarget(
                "Failed to take screenshot for image template matching".to_string(),
            ))?;

        let mut find_args = serde_json::json!({
            "template_image_base64": b64,
            "threshold": 0.75,
            "max_results": 1,
        });
        // Prefer screenshot_id (avoids re-sending the full image and provides
        // metadata for screen coordinate conversion). Fall back to base64.
        if let Some(id) = &screenshot_id {
            find_args["screenshot_id"] = serde_json::Value::String(id.clone());
        } else {
            find_args["screenshot_image_base64"] = serde_json::Value::String(screenshot_b64);
        }

        self.record_event(
            node_run.as_deref(),
            "tool_call",
            serde_json::json!({"name": "find_image", "args": {
                "threshold": 0.75,
                "max_results": 1,
                "has_template": true,
            }}),
        );

        let result = mcp
            .call_tool("find_image", Some(find_args))
            .await
            .map_err(|e| ExecutorError::ClickTarget(format!("find_image failed: {}", e)))?;
        Self::check_tool_error(&result, "find_image")?;

        let result_text = Self::extract_result_text(&result);

        self.record_event(
            node_run.as_deref(),
            "tool_result",
            serde_json::json!({
                "name": "find_image",
                "text": Self::truncate_for_trace(&result_text, 8192),
                "text_len": result_text.len(),
            }),
        );

        let parsed: Value = serde_json::from_str(&result_text).map_err(|e| {
            ExecutorError::ClickTarget(format!("Failed to parse find_image result: {}", e))
        })?;

        // find_image returns { "matches": [...] }.
        let matches = parsed["matches"].as_array().ok_or_else(|| {
            ExecutorError::ClickTarget("find_image returned no matches array".to_string())
        })?;
        let best = matches
            .first()
            .ok_or_else(|| ExecutorError::ClickTarget("find_image found no matches".to_string()))?;

        // Prefer screen coordinates (available when screenshot_id was used).
        // Fall back to center pixel coordinates.
        let x = best["screen_x"]
            .as_f64()
            .or_else(|| best["center"]["x"].as_f64())
            .ok_or(ExecutorError::ClickTarget(
                "Missing x in find_image match".to_string(),
            ))?;
        let y = best["screen_y"]
            .as_f64()
            .or_else(|| best["center"]["y"].as_f64())
            .ok_or(ExecutorError::ClickTarget(
                "Missing y in find_image match".to_string(),
            ))?;

        self.log(format!(
            "Resolved image target -> ({}, {}), score={}",
            x,
            y,
            best["score"].as_f64().unwrap_or(0.0)
        ));

        self.record_event(
            node_run.as_deref(),
            "target_resolved",
            serde_json::json!({
                "method": "find_image",
                "x": x,
                "y": y,
                "score": best["score"],
            }),
        );

        Ok(NodeType::Click(ClickParams {
            target: params.target.clone(),
            x: Some(x),
            y: Some(y),
            button: params.button,
            click_count: params.click_count,
            ..Default::default()
        }))
    }

    /// Resolve a window control click (close/minimize/maximize) to absolute
    /// screen coordinates by querying the focused window's bounds and applying
    /// the standard macOS traffic-light button offset.
    async fn resolve_window_control_click(
        &self,
        action: clickweave_core::WindowControlAction,
        mcp: &McpRouter,
        params: &ClickParams,
        node_run: &mut Option<&mut NodeRun>,
    ) -> ExecutorResult<NodeType> {
        let app_name = self.focused_app_name();
        self.log(format!(
            "Resolving window control '{}' for app {:?}",
            action.display_name(),
            app_name
        ));

        // Focus the window first — it may be off-screen (different Space,
        // behind other windows) after a CDP relaunch or app switch.
        if let Some(ref name) = app_name {
            let focus_args = Some(serde_json::json!({"app_name": name}));
            let _ = mcp.call_tool("focus_window", focus_args).await;
        }

        // Call list_windows to get window bounds.
        let args = app_name
            .as_ref()
            .map(|name| serde_json::json!({"app_name": name}));
        self.record_event(
            node_run.as_deref(),
            "tool_call",
            serde_json::json!({"name": "list_windows", "args": args}),
        );
        let result = mcp
            .call_tool("list_windows", args)
            .await
            .map_err(|e| ExecutorError::ClickTarget(format!("list_windows failed: {}", e)))?;
        Self::check_tool_error(&result, "list_windows")?;

        let result_text = Self::extract_result_text(&result);
        let windows: Vec<Value> = serde_json::from_str(&result_text).map_err(|e| {
            ExecutorError::ClickTarget(format!("Failed to parse list_windows response: {e}"))
        })?;

        // Find the best window: prefer on-screen windows, but accept off-screen
        // ones (the focus_window call above may not have taken effect yet).
        // Bounds are valid regardless of is_on_screen — we just need the
        // window's position to compute the traffic light button coordinates.
        let window = if let Some(ref name) = app_name {
            let candidates: Vec<_> = windows
                .iter()
                .filter(|w| w["owner_name"].as_str() == Some(name))
                .collect();
            // Prefer on-screen, fall back to any matching window.
            candidates
                .iter()
                .copied()
                .filter(|w| w["is_on_screen"].as_bool().unwrap_or(false))
                .min_by_key(|w| w["layer"].as_i64().unwrap_or(i64::MAX))
                .or_else(|| {
                    candidates
                        .into_iter()
                        .min_by_key(|w| w["layer"].as_i64().unwrap_or(i64::MAX))
                })
        } else {
            let candidates: Vec<_> = windows.iter().collect();
            candidates
                .iter()
                .copied()
                .filter(|w| w["is_on_screen"].as_bool().unwrap_or(false))
                .min_by_key(|w| w["layer"].as_i64().unwrap_or(i64::MAX))
                .or_else(|| {
                    candidates
                        .into_iter()
                        .min_by_key(|w| w["layer"].as_i64().unwrap_or(i64::MAX))
                })
        };

        let window = window.ok_or_else(|| {
            ExecutorError::ClickTarget(format!(
                "No window found for app {:?} to resolve {}",
                app_name,
                action.display_name()
            ))
        })?;

        let bounds = &window["bounds"];
        let win_x = bounds["x"]
            .as_f64()
            .ok_or_else(|| ExecutorError::ClickTarget("Window bounds missing 'x'".to_string()))?;
        let win_y = bounds["y"]
            .as_f64()
            .ok_or_else(|| ExecutorError::ClickTarget("Window bounds missing 'y'".to_string()))?;

        let (offset_x, offset_y) = action.window_offset();
        let click_x = win_x + offset_x;
        let click_y = win_y + offset_y;

        self.log(format!(
            "Resolved {} -> ({click_x}, {click_y}) (window at {win_x}, {win_y})",
            action.display_name()
        ));

        self.record_event(
            node_run.as_deref(),
            "target_resolved",
            serde_json::json!({
                "method": "window_control",
                "action": action.display_name(),
                "window_x": win_x,
                "window_y": win_y,
                "click_x": click_x,
                "click_y": click_y,
            }),
        );

        Ok(NodeType::Click(ClickParams {
            target: params.target.clone(),
            x: Some(click_x),
            y: Some(click_y),
            button: params.button,
            click_count: params.click_count,
            ..Default::default()
        }))
    }
}

use clickweave_core::cdp::{SnapshotMatch, find_elements_in_snapshot};

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Try to resolve and click a text target via CDP (chrome-devtools).
    ///
    /// Takes a snapshot of the page's accessibility tree, finds the element
    /// matching the target text, and clicks it via CDP. Returns the click
    /// result text on success, or an error string to trigger native fallback.
    async fn resolve_and_click_cdp(
        &self,
        target: &str,
        expected_role: Option<&str>,
        expected_href: Option<&str>,
        expected_parent_role: Option<&str>,
        expected_parent_name: Option<&str>,
        cdp_server: &str,
        mcp: &McpRouter,
        node_run: Option<&NodeRun>,
    ) -> ExecutorResult<String> {
        // 1. Ensure a page is selected (list_pages triggers auto-selection
        //    inside chrome-devtools-mcp). This is needed because the CDP server
        //    may have been spawned in an earlier node (e.g. FocusWindow) and the
        //    selected page could have changed since.
        let _ = mcp
            .call_tool_on(cdp_server, "list_pages", Some(serde_json::json!({})))
            .await;

        // 2. Take CDP snapshot
        self.log(format!("CDP: taking snapshot to find '{}'", target));
        let snapshot_result = mcp
            .call_tool_on(cdp_server, "take_snapshot", Some(serde_json::json!({})))
            .await
            .map_err(|e| ExecutorError::Cdp(format!("take_snapshot failed: {e}")))?;

        if snapshot_result.is_error == Some(true) {
            let error_text = Self::extract_result_text(&snapshot_result);
            self.log(format!("CDP take_snapshot error: {}", error_text));
            return Err(ExecutorError::Cdp(format!(
                "take_snapshot error: {}",
                error_text
            )));
        }

        let snapshot_text = Self::extract_result_text(&snapshot_result);

        // 3. Find matching elements
        let mut matches = find_elements_in_snapshot(&snapshot_text, target);

        // Narrow by role/href then parent context for disambiguation.
        clickweave_core::cdp::narrow_matches(&mut matches, expected_role, expected_href);
        clickweave_core::cdp::narrow_by_parent(
            &mut matches,
            expected_parent_role,
            expected_parent_name,
        );

        let uid = if matches.is_empty() {
            self.log(format!(
                "CDP: no exact match for '{}', trying LLM resolution",
                target
            ));
            self.resolve_cdp_element_name(target, &snapshot_text)
                .await?
        } else if matches.len() == 1 {
            matches[0].uid.clone()
        } else {
            self.log(format!(
                "CDP: {} matches for '{}', disambiguating",
                matches.len(),
                target
            ));
            self.disambiguate_cdp_elements(target, &matches).await?
        };

        // 4. Click the element
        self.log(format!("CDP: clicking element uid='{}'", uid));
        let click_args = serde_json::json!({ "uid": uid });
        let click_result = mcp
            .call_tool_on(cdp_server, "click", Some(click_args))
            .await
            .map_err(|e| ExecutorError::Cdp(format!("click failed: {e}")))?;

        if click_result.is_error == Some(true) {
            return Err(ExecutorError::Cdp(format!(
                "click error: {}",
                Self::extract_result_text(&click_result)
            )));
        }

        self.record_event(
            node_run,
            "cdp_click",
            serde_json::json!({ "target": target, "uid": uid }),
        );

        Ok(Self::extract_result_text(&click_result))
    }

    /// Ask the LLM to find the best matching element in the CDP snapshot.
    async fn resolve_cdp_element_name(
        &self,
        target: &str,
        snapshot_text: &str,
    ) -> ExecutorResult<String> {
        let truncated = &snapshot_text[..snapshot_text.floor_char_boundary(4000)];

        let prompt = format!(
            "Find the element in this page snapshot that best matches the target '{target}'.\n\
             Return ONLY the uid value, nothing else.\n\n\
             Page snapshot:\n{truncated}"
        );

        let response = self
            .reasoning_backend()
            .chat(vec![clickweave_llm::Message::user(prompt)], None)
            .await
            .map_err(|e| ExecutorError::Cdp(format!("LLM resolution failed: {e}")))?;

        let raw_text = response
            .choices
            .first()
            .and_then(|c| c.message.content_text())
            .ok_or_else(|| ExecutorError::Cdp("LLM returned empty content".to_string()))?;

        let uid = raw_text.trim().trim_matches('"').to_string();
        if uid.is_empty() {
            return Err(ExecutorError::Cdp(format!(
                "LLM could not resolve '{}' in CDP snapshot",
                target
            )));
        }

        // Validate that the UID actually appears in the snapshot.
        let uid_exists = snapshot_text.contains(&format!("uid=\"{}\"", uid))
            || snapshot_text.contains(&format!("uid={} ", uid))
            || snapshot_text.ends_with(&format!("uid={}", uid));
        if !uid_exists {
            return Err(ExecutorError::Cdp(format!(
                "LLM returned uid '{}' which does not exist in the CDP snapshot",
                uid
            )));
        }

        self.log(format!("CDP: LLM resolved '{}' -> uid='{}'", target, uid));
        Ok(uid)
    }

    /// Disambiguate between multiple CDP element matches using the LLM.
    async fn disambiguate_cdp_elements(
        &self,
        target: &str,
        matches: &[SnapshotMatch],
    ) -> ExecutorResult<String> {
        let valid_uids: std::collections::HashSet<&str> =
            matches.iter().map(|m| m.uid.as_str()).collect();

        let options: Vec<String> = matches
            .iter()
            .enumerate()
            .map(|(i, m)| format!("{}: uid={} — {}", i + 1, m.uid, m.label))
            .collect();

        let hint_context = self.format_supervision_hint("A previous click attempt failed. ");

        let tried_context = {
            let tried = self
                .tried_cdp_uids
                .read()
                .unwrap_or_else(|e| e.into_inner());
            Self::format_tried_context(&tried, "UIDs")
        };

        let prompt = format!(
            "Multiple elements match the target '{target}'. Which one is the best match?\n\
             Return ONLY the uid value, nothing else.\n\n{}{hint_context}{tried_context}",
            options.join("\n")
        );

        let response = self
            .reasoning_backend()
            .chat(vec![clickweave_llm::Message::user(prompt)], None)
            .await
            .map_err(|e| ExecutorError::Cdp(format!("LLM disambiguation failed: {e}")))?;

        let raw_text = response
            .choices
            .first()
            .and_then(|c| c.message.content_text())
            .unwrap_or_default();

        let uid = raw_text.trim().trim_matches('"').to_string();
        if valid_uids.contains(uid.as_str()) {
            self.tried_cdp_uids
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push(uid.clone());
            Ok(uid)
        } else {
            self.log(format!(
                "CDP: LLM returned '{}' which is not in candidate set, using first match",
                uid
            ));
            Ok(matches[0].uid.clone())
        }
    }
}

fn truncate_for_error(s: &str, max_len: usize) -> &str {
    match s.char_indices().nth(max_len) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Pick a random port in the ephemeral range (49152–65535).
fn rand_ephemeral_port() -> u16 {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let raw = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    let range = 65535 - 49152;
    49152 + (raw % range) as u16
}

/// Build the McpServerConfig for a chrome-devtools-mcp connected to a specific port.
fn cdp_server_config(server_name: &str, port: u16) -> clickweave_mcp::McpServerConfig {
    clickweave_mcp::McpServerConfig {
        name: server_name.to_string(),
        command: "npx".into(),
        args: vec![
            "-y".into(),
            "chrome-devtools-mcp".into(),
            format!("--browserUrl=http://127.0.0.1:{}", port),
        ],
    }
}

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Ensure a CDP server is available for the given Electron/Chrome app.
    ///
    /// If no CDP server is registered for this app:
    /// - Test mode: quit the app, relaunch with --remote-debugging-port, spawn
    ///   a chrome-devtools-mcp server, poll until ready, store port in cache.
    /// - Run mode: read port from decision cache, try connecting, relaunch if needed.
    ///
    /// Returns the CDP server name on success.
    async fn ensure_cdp_server(
        &mut self,
        _node_id: Uuid,
        app_name: &str,
        mcp: &mut McpRouter,
        node_run: Option<&NodeRun>,
    ) -> ExecutorResult<String> {
        use clickweave_core::ExecutionMode;
        use clickweave_core::cdp::cdp_server_name;
        use clickweave_core::decision_cache::CdpPort;

        let server_name = cdp_server_name(app_name);

        // Already have a CDP server for this app — nothing to do.
        if self.cdp_servers.contains_key(app_name) {
            return Ok(server_name);
        }

        let port = if self.execution_mode == ExecutionMode::Test {
            // Test mode: pick a random port, relaunch the app.
            let port = rand_ephemeral_port();
            self.log(format!(
                "Restarting '{}' with DevTools enabled (port {})...",
                app_name, port
            ));
            self.relaunch_with_debug_port(app_name, port, mcp).await?;
            // App was restarted — evict stale PID from app cache.
            self.evict_app_cache(app_name);
            // Store in decision cache for Run mode replay.
            self.decision_cache
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .cdp_port
                .insert(app_name.to_string(), CdpPort { port });
            port
        } else {
            // Run mode: read cached port, try connecting, relaunch if needed.
            let cached = self
                .decision_cache
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .cdp_port
                .get(app_name)
                .map(|e| e.port);

            let port = cached.ok_or_else(|| {
                ExecutorError::Cdp(format!(
                    "No cached CDP port for '{}'. Run in Test mode first.",
                    app_name
                ))
            })?;

            // Try spawning CDP server with cached port (app may still be running).
            let config = cdp_server_config(&server_name, port);
            let connect_ok = mcp.spawn_server(&config).await.is_ok()
                && self.poll_cdp_ready(&server_name, mcp, 5).await.is_ok();

            if !connect_ok {
                self.log(format!(
                    "CDP connection failed for '{}', relaunching with port {}...",
                    app_name, port
                ));
                self.relaunch_with_debug_port(app_name, port, mcp).await?;
                // App was restarted — evict stale PID from app cache.
                self.evict_app_cache(app_name);
            }
            port
        };

        // Spawn the CDP server if not already connected.
        if !mcp.has_server(&server_name) {
            let config = cdp_server_config(&server_name, port);
            mcp.spawn_server(&config).await.map_err(|e| {
                ExecutorError::Cdp(format!(
                    "Failed to start CDP server for '{}': {}",
                    app_name, e
                ))
            })?;
        }

        // Poll until the app is ready for CDP.
        self.poll_cdp_ready(&server_name, mcp, 30).await?;

        self.log(format!(
            "CDP connected to '{}' (port {}, server '{}')",
            app_name, port, server_name
        ));
        self.record_event(
            node_run,
            "cdp_connected",
            serde_json::json!({
                "app_name": app_name,
                "port": port,
                "server_name": server_name,
            }),
        );

        self.cdp_servers
            .insert(app_name.to_string(), server_name.clone());
        Ok(server_name)
    }

    /// Quit the app, confirm it exited, relaunch with --remote-debugging-port.
    async fn relaunch_with_debug_port(
        &self,
        app_name: &str,
        port: u16,
        mcp: &McpRouter,
    ) -> ExecutorResult<()> {
        // Quit (best-effort — app might not be running).
        let quit_args = serde_json::json!({ "app_name": app_name });
        if let Err(e) = mcp.call_tool("quit_app", Some(quit_args)).await {
            self.log(format!(
                "quit_app for '{}' failed (continuing): {}",
                app_name, e
            ));
        }

        // Poll list_apps until the app is no longer running (up to 10s).
        let poll_args = serde_json::json!({ "app_name": app_name, "user_apps_only": true });
        let mut quit_confirmed = false;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if let Ok(r) = mcp.call_tool("list_apps", Some(poll_args.clone())).await {
                let text = Self::extract_result_text(&r);
                if text.trim() == "[]" {
                    quit_confirmed = true;
                    break;
                }
            }
        }

        if !quit_confirmed {
            self.log(format!(
                "'{}' did not quit within 10s, force-killing",
                app_name
            ));
            let force_args = serde_json::json!({ "app_name": app_name, "force": true });
            let _ = mcp.call_tool("quit_app", Some(force_args)).await;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        // Relaunch with debug port.
        let launch_args = serde_json::json!({
            "app_name": app_name,
            "args": [format!("--remote-debugging-port={}", port)],
        });
        let result = mcp
            .call_tool("launch_app", Some(launch_args))
            .await
            .map_err(|e| {
                ExecutorError::Cdp(format!(
                    "Failed to launch '{}' with debug port: {}",
                    app_name, e
                ))
            })?;

        if result.is_error == Some(true) {
            return Err(ExecutorError::Cdp(format!(
                "launch_app error for '{}': {}",
                app_name,
                Self::extract_result_text(&result)
            )));
        }

        // Wait for the app to start up.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        Ok(())
    }

    /// Poll `list_pages` on a CDP server until it returns at least one page.
    async fn poll_cdp_ready(
        &self,
        server_name: &str,
        mcp: &McpRouter,
        timeout_secs: u64,
    ) -> ExecutorResult<()> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

        loop {
            match mcp
                .call_tool_on(server_name, "list_pages", Some(serde_json::json!({})))
                .await
            {
                Ok(result) if result.is_error != Some(true) => {
                    let text = Self::extract_result_text(&result);
                    // Page index may be 0-based or 1-based depending on MCP
                    // server version — check for any "N: <url>" page entry.
                    if text.lines().any(|l| {
                        l.as_bytes().first().is_some_and(|b| b.is_ascii_digit()) && l.contains(": ")
                    }) {
                        self.log(format!("CDP pages for '{}': {}", server_name, text.trim()));
                        return Ok(());
                    }
                    tracing::debug!(
                        "CDP list_pages for '{}' returned but no pages yet: {:?}",
                        server_name,
                        &text[..text.len().min(500)]
                    );
                }
                Ok(result) => {
                    let text = Self::extract_result_text(&result);
                    tracing::debug!(
                        "CDP list_pages error for '{}': {}",
                        server_name,
                        &text[..text.len().min(500)]
                    );
                }
                Err(e) => {
                    tracing::debug!("CDP list_pages call failed for '{}': {}", server_name, e);
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(ExecutorError::Cdp(format!(
                    "Timed out waiting for CDP server '{}' to be ready ({}s)",
                    server_name, timeout_secs
                )));
            }

            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
}
