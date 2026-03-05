use super::WorkflowExecutor;
use clickweave_core::decision_cache::cache_key;
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
        &self,
        node_id: Uuid,
        node_type: &NodeType,
        mcp: &McpRouter,
        mut node_run: Option<&mut NodeRun>,
    ) -> Result<Value, String> {
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
            let result = mcp
                .call_tool(&p.operation_name, args)
                .await
                .map_err(|e| format!("AppDebugKit op {} failed: {}", p.operation_name, e))?;
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
            return Err("McpToolCall has empty tool_name".to_string());
        }

        let resolved_click;
        let effective = if let NodeType::Click(p) = node_type
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
            let app = self
                .resolve_app_name(node_id, user_input, mcp, node_run.as_deref())
                .await?;
            *self.focused_app.write().unwrap_or_else(|e| e.into_inner()) = Some(app.name.clone());
            resolved_fw = NodeType::FocusWindow(FocusWindowParams {
                method: FocusMethod::Pid,
                value: Some(app.pid.to_string()),
                bring_to_front: p.bring_to_front,
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
            .map_err(|e| format!("Tool mapping failed: {}", e))?;
        let tool_name = &invocation.name;

        self.log(format!("Calling MCP tool: {}", tool_name));
        let mut args = self.resolve_image_paths(Some(invocation.arguments));

        // Scope find_text to the focused app when no explicit app_name is set
        if tool_name == "find_text"
            && let Some(ref mut a) = args
            && a.get("app_name").is_none()
        {
            let scoped_app = self
                .focused_app
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(app_name) = scoped_app {
                a["app_name"] = serde_json::Value::String(app_name);
            }
        }

        // Save original args for find_text retry fallback (args will be moved into call_tool)
        let find_text_original_args = if tool_name == "find_text" {
            args.clone()
        } else {
            None
        };

        // Extract app_name before args is moved into call_tool
        let launch_app_name = if tool_name == "launch_app" {
            args.as_ref()
                .and_then(|a| a.get("app_name"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        };

        self.record_event(
            node_run.as_deref(),
            "tool_call",
            serde_json::json!({"name": tool_name, "args": args}),
        );
        let result = mcp
            .call_tool(tool_name, args)
            .await
            .map_err(|e| format!("MCP tool {} failed: {}", tool_name, e))?;

        Self::check_tool_error(&result, tool_name)?;

        // launch_app implies the app is now focused
        if let Some(name) = &launch_app_name {
            *self.focused_app.write().unwrap_or_else(|e| e.into_inner()) = Some(name.clone());
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

    fn check_tool_error(result: &ToolCallResult, tool_name: &str) -> Result<(), String> {
        if result.is_error == Some(true) {
            let error_text = Self::extract_result_text(result);
            return Err(format!(
                "MCP tool {} returned error: {}",
                tool_name, error_text
            ));
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
            .or_else(|| {
                self.focused_app
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
            });
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
    ) -> Result<NodeType, String> {
        let target = params
            .target
            .as_deref()
            .ok_or("resolve_click_target called with no target")?;

        let scoped_app = self
            .focused_app
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

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
            .map_err(|e| format!("find_text for '{}' failed: {}", target, e))?;

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
            return Err(format!(
                "Could not find text '{}' on screen (find_text returned: {})",
                target,
                truncate_for_error(&result_text, 120),
            ));
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

        let x = best["x"]
            .as_f64()
            .ok_or_else(|| format!("find_text match for '{}' missing 'x' coordinate", target))?;
        let y = best["y"]
            .as_f64()
            .ok_or_else(|| format!("find_text match for '{}' missing 'y' coordinate", target))?;
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
    ) -> Result<NodeType, String> {
        let b64 = params
            .template_image
            .as_deref()
            .ok_or("resolve_click_target_by_image called without template_image")?;

        self.log("Resolving click target by image template".to_string());

        // Take a screenshot first — find_image needs both a template and a
        // screenshot to search within. Use screenshot_id when available so
        // find_image has the screenshot metadata for screen coordinate conversion.
        let app_name = self.focused_app.read().ok().and_then(|g| g.clone());
        let screenshot_args = match &app_name {
            Some(name) => serde_json::json!({ "app_name": name }),
            None => serde_json::json!({}),
        };
        let (screenshot_b64, screenshot_id) = self
            .take_screenshot_with_id(mcp, screenshot_args)
            .await
            .ok_or("Failed to take screenshot for image template matching")?;

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
            .map_err(|e| format!("find_image failed: {}", e))?;
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

        let parsed: Value = serde_json::from_str(&result_text)
            .map_err(|e| format!("Failed to parse find_image result: {}", e))?;

        // find_image returns { "matches": [...] }.
        let matches = parsed["matches"]
            .as_array()
            .ok_or("find_image returned no matches array")?;
        let best = matches.first().ok_or("find_image found no matches")?;

        // Prefer screen coordinates (available when screenshot_id was used).
        // Fall back to center pixel coordinates.
        let x = best["screen_x"]
            .as_f64()
            .or_else(|| best["center"]["x"].as_f64())
            .ok_or("Missing x in find_image match")?;
        let y = best["screen_y"]
            .as_f64()
            .or_else(|| best["center"]["y"].as_f64())
            .ok_or("Missing y in find_image match")?;

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
}

fn truncate_for_error(s: &str, max_len: usize) -> &str {
    match s.char_indices().nth(max_len) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}
