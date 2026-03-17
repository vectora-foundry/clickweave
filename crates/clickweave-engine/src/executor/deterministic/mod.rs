mod cdp;
mod click;
mod hover;
mod window;

use super::{ExecutorError, ExecutorResult, WorkflowExecutor};
use clickweave_core::AppKind;
use clickweave_core::{
    FocusMethod, FocusWindowParams, NodeRun, NodeType, ScreenshotMode, TakeScreenshotParams,
    tool_mapping,
};
use clickweave_llm::ChatBackend;
use clickweave_mcp::{McpRouter, ToolCallResult};
use serde_json::Value;
use uuid::Uuid;

/// Select the best window from a `list_windows` response for window control resolution.
///
/// Filters by `app_name` (case-insensitive) if provided. Among matches, prefers
/// on-screen windows at the lowest layer. Uses array index as z-order tiebreaker
/// since `list_windows` returns windows in front-to-back order.
fn select_best_window<'a>(windows: &'a [Value], app_name: Option<&str>) -> Option<&'a Value> {
    let rank = |i: usize, w: &Value| (w["layer"].as_i64().unwrap_or(i64::MAX), i);

    let mut best_onscreen: Option<(usize, &Value)> = None;
    let mut best_any: Option<(usize, &Value)> = None;

    for (i, w) in windows.iter().enumerate() {
        let matches = app_name.is_none_or(|name| {
            w["owner_name"]
                .as_str()
                .is_some_and(|o| o.eq_ignore_ascii_case(name))
        });
        if !matches {
            continue;
        }

        let key = rank(i, w);
        if best_any.is_none_or(|(bi, bw)| key < rank(bi, bw)) {
            best_any = Some((i, w));
        }
        if w["is_on_screen"].as_bool().unwrap_or(false)
            && best_onscreen.is_none_or(|(bi, bw)| key < rank(bi, bw))
        {
            best_onscreen = Some((i, w));
        }
    }

    best_onscreen.or(best_any).map(|(_, w)| w)
}

fn truncate_for_error(s: &str, max_len: usize) -> &str {
    match s.char_indices().nth(max_len) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

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

        // --- Hover: CDP path + native fallback + dwell ---
        if let NodeType::Hover(p) = node_type {
            self.log(format!("Hover: {}", node_type.action_description()));

            let app_kind = self.focused_app_kind();

            // CDP path: try hover via chrome-devtools-mcp for Electron/Chrome apps
            if app_kind.uses_cdp()
                && let Some(cdp_server) = self.focused_cdp_server()
                && let Some(target) = &p.target
            {
                let expected = cdp::CdpExpected::from_click_target(target);
                match self
                    .resolve_and_hover_cdp(
                        target.text(),
                        &expected,
                        &cdp_server,
                        mcp,
                        node_run.as_deref(),
                    )
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
                        return Ok(Self::parse_result_text(&result_text));
                    }
                    Err(e) => {
                        self.log(format!("CDP hover failed, falling back to native: {e}"));
                    }
                }
            }

            // Native path: resolve coordinates, then move_mouse + dwell
            let resolved_hover;
            let effective = if p.template_image.is_some() && p.x.is_none() {
                resolved_hover = self
                    .resolve_hover_target_by_image(node_id, mcp, p, &mut node_run)
                    .await?;
                &resolved_hover
            } else if p.target.is_some() && p.x.is_none() {
                resolved_hover = self
                    .resolve_hover_target(node_id, mcp, p, &mut node_run)
                    .await?;
                &resolved_hover
            } else {
                node_type
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
            let result_text = Self::extract_result_text(&result);

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

            return Ok(Self::parse_result_text(&result_text));
        }

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
                let expected = cdp::CdpExpected::from_click_target(click_target);
                match self
                    .resolve_and_click_cdp(
                        target,
                        &expected,
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
                // Re-resolve PID -- it may have changed if the app was relaunched.
                app = self
                    .resolve_app_name(node_id, user_input, mcp, node_run.as_deref())
                    .await?;
            }

            *self.write_focused_app() = Some((app.name.clone(), app_kind));

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
            *self.write_focused_app() = Some((name.clone(), launch_app_kind));

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
        // Skip resolution inside loops -- FindText nodes in loops act as condition checks
        // where accurate found/not-found results are needed for exit conditions.
        // Element resolution would map e.g. "128" -> "8" (a button), masking the fact
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
}
