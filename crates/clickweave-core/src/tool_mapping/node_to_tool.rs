use super::*;

/// Convert a `NodeType` to a `ToolInvocation` (tool name + JSON arguments).
///
/// Returns `Err(NotAToolNode)` for `AiStep` and `AppDebugKitOp`, which are not
/// direct MCP tool calls.
pub fn node_type_to_tool_invocation(
    node_type: &NodeType,
) -> Result<ToolInvocation, ToolMappingError> {
    let (name, arguments) = match node_type {
        NodeType::TakeScreenshot(p) => {
            let mode = match p.mode {
                ScreenshotMode::Screen => "screen",
                ScreenshotMode::Window => "window",
                ScreenshotMode::Region => "region",
            };
            let mut args = serde_json::json!({
                "mode": mode,
                "include_ocr": p.include_ocr,
            });
            if let Some(target) = &p.target {
                args["app_name"] = Value::String(target.clone());
            }
            ("take_screenshot", args)
        }
        NodeType::FindText(p) => {
            let mut args = serde_json::json!({"text": p.search_text});
            if let Some(ref scope) = p.scope {
                args["app_name"] = Value::String(scope.clone());
            }
            ("find_text", args)
        }
        NodeType::FindImage(p) => {
            let mut args = serde_json::json!({
                "threshold": p.threshold,
                "max_results": p.max_results,
            });
            if let Some(img) = &p.template_image {
                args["template_image_base64"] = Value::String(img.clone());
            }
            ("find_image", args)
        }
        NodeType::FindApp(p) => {
            let args = serde_json::json!({"search": p.search});
            ("list_apps", args)
        }
        NodeType::Click(p) => {
            let button = match p.button {
                MouseButton::Left => "left",
                MouseButton::Right => "right",
                MouseButton::Center => "center",
            };
            let mut args = serde_json::json!({
                "button": button,
                "click_count": p.click_count,
            });
            if let Some(ClickTarget::Coordinates { x, y }) = &p.target {
                args["x"] = serde_json::json!(x);
                args["y"] = serde_json::json!(y);
            }
            ("click", args)
        }
        NodeType::TypeText(p) => ("type_text", serde_json::json!({"text": p.text})),
        NodeType::PressKey(p) => {
            let mut args = serde_json::json!({"key": p.key});
            if !p.modifiers.is_empty() {
                args["modifiers"] = serde_json::json!(p.modifiers);
            }
            ("press_key", args)
        }
        NodeType::Scroll(p) => {
            let mut args = serde_json::json!({"delta_y": p.delta_y});
            if let Some(x) = p.x {
                args["x"] = serde_json::json!(x);
            }
            if let Some(y) = p.y {
                args["y"] = serde_json::json!(y);
            }
            ("scroll", args)
        }
        NodeType::FocusWindow(p) => {
            let mut args = serde_json::json!({});
            match &p.target {
                FocusTarget::AppName(name) if !name.is_empty() => {
                    args["app_name"] = Value::String(name.clone());
                }
                FocusTarget::WindowId(id) => {
                    args["window_id"] = serde_json::json!(id);
                }
                FocusTarget::Pid(pid) => {
                    args["pid"] = serde_json::json!(pid);
                }
                FocusTarget::AppName(_) => {}
            }
            if p.app_kind.uses_cdp() {
                args["app_kind"] = serde_json::json!(p.app_kind);
            }
            ("focus_window", args)
        }
        NodeType::Hover(p) => {
            let mut args = serde_json::json!({});
            if let Some(ClickTarget::Coordinates { x, y }) = &p.target {
                args["x"] = serde_json::json!(x);
                args["y"] = serde_json::json!(y);
            }
            ("move_mouse", args)
        }
        NodeType::Drag(p) => {
            let mut args = serde_json::json!({});
            if let Some(x) = p.from_x {
                args["from_x"] = serde_json::json!(x);
            }
            if let Some(y) = p.from_y {
                args["from_y"] = serde_json::json!(y);
            }
            if let Some(x) = p.to_x {
                args["to_x"] = serde_json::json!(x);
            }
            if let Some(y) = p.to_y {
                args["to_y"] = serde_json::json!(y);
            }
            ("drag", args)
        }
        NodeType::LaunchApp(p) => ("launch_app", serde_json::json!({"app_name": p.app_name})),
        NodeType::QuitApp(p) => ("quit_app", serde_json::json!({"app_name": p.app_name})),
        // CDP nodes — use cdp_ prefixed MCP tool names
        NodeType::CdpClick(p) => ("cdp_click", serde_json::json!({"uid": p.target.as_str()})),
        NodeType::CdpHover(p) => ("cdp_hover", serde_json::json!({"uid": p.target.as_str()})),
        NodeType::CdpFill(p) => (
            "cdp_fill",
            serde_json::json!({"uid": p.target.as_str(), "value": p.value}),
        ),
        NodeType::CdpType(p) => ("cdp_type_text", serde_json::json!({"text": p.text})),
        NodeType::CdpPressKey(p) => {
            let mut args = serde_json::json!({"key": p.key});
            if !p.modifiers.is_empty() {
                args["modifiers"] = serde_json::json!(p.modifiers);
            }
            ("cdp_press_key", args)
        }
        NodeType::CdpNavigate(p) => ("cdp_navigate", serde_json::json!({"url": p.url})),
        NodeType::CdpNewPage(p) => {
            let mut args = serde_json::json!({});
            if !p.url.is_empty() {
                args["url"] = Value::String(p.url.clone());
            }
            ("cdp_new_page", args)
        }
        NodeType::CdpClosePage(p) => {
            let mut args = serde_json::json!({});
            if let Some(idx) = p.page_index {
                args["page_index"] = serde_json::json!(idx);
            }
            ("cdp_close_page", args)
        }
        NodeType::CdpSelectPage(p) => (
            "cdp_select_page",
            serde_json::json!({"page_index": p.page_index}),
        ),
        NodeType::CdpWait(p) => (
            "cdp_wait_for",
            serde_json::json!({"text": p.text, "timeout_ms": p.timeout_ms}),
        ),
        NodeType::CdpHandleDialog(p) => {
            let mut args = serde_json::json!({"accept": p.accept});
            if let Some(text) = &p.prompt_text {
                args["prompt_text"] = Value::String(text.clone());
            }
            ("cdp_handle_dialog", args)
        }
        // AX dispatch — uid is always provided because the executor resolves
        // the descriptor to a fresh uid immediately before dispatch. The
        // descriptor itself lives on the node and is not sent over the wire.
        NodeType::AxClick(p) => ("ax_click", serde_json::json!({"uid": p.target.as_str()})),
        NodeType::AxSetValue(p) => (
            "ax_set_value",
            serde_json::json!({"uid": p.target.as_str(), "value": p.value}),
        ),
        NodeType::AxSelect(p) => ("ax_select", serde_json::json!({"uid": p.target.as_str()})),
        NodeType::McpToolCall(p) => {
            let args = if p.arguments.is_null() {
                serde_json::json!({})
            } else {
                p.arguments.clone()
            };
            (&*p.tool_name, args)
        }
        NodeType::AiStep(_) | NodeType::AppDebugKitOp(_) | NodeType::Unknown => {
            return Err(ToolMappingError::NotAToolNode);
        }
    };

    Ok(ToolInvocation {
        name: name.to_string(),
        arguments,
    })
}
