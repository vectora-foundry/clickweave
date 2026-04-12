//! Shared mapping between `NodeType` and MCP tool invocations.
//!
//! Used by both the planner (tool args → NodeType) and the executor (NodeType → tool args).

use crate::{
    AppKind, CdpClickParams, CdpClosePageParams, CdpFillParams, CdpHandleDialogParams,
    CdpHoverParams, CdpNavigateParams, CdpNewPageParams, CdpPressKeyParams, CdpSelectPageParams,
    CdpTarget, CdpTypeParams, CdpWaitParams, ClickParams, ClickTarget, DragParams, FindAppParams,
    FindImageParams, FindTextParams, FocusMethod, FocusWindowParams, HoverParams, LaunchAppParams,
    McpToolCallParams, MouseButton, NodeType, PressKeyParams, QuitAppParams, ScreenshotMode,
    ScrollParams, TakeScreenshotParams, TypeTextParams,
};
use serde_json::Value;
use std::fmt;

/// A tool name and its arguments, ready to be sent to an MCP server.
#[derive(Debug, Clone)]
pub struct ToolInvocation {
    pub name: String,
    pub arguments: Value,
}

/// Errors that can occur during tool mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolMappingError {
    /// The tool name is not in the known tools list.
    UnknownTool(String),
    /// A required argument is missing.
    MissingArgument { tool: String, argument: String },
    /// The node type cannot be mapped to a tool invocation (e.g. AiStep, AppDebugKitOp).
    NotAToolNode,
}

impl fmt::Display for ToolMappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolMappingError::UnknownTool(name) => write!(f, "Unknown tool: {}", name),
            ToolMappingError::MissingArgument { tool, argument } => {
                write!(f, "{} requires non-empty '{}' argument", tool, argument)
            }
            ToolMappingError::NotAToolNode => write!(f, "Not a tool node"),
        }
    }
}

impl std::error::Error for ToolMappingError {}

fn required_str<'a>(
    args: &'a Value,
    tool: &str,
    argument: &str,
) -> Result<&'a str, ToolMappingError> {
    args.get(argument)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolMappingError::MissingArgument {
            tool: tool.into(),
            argument: argument.into(),
        })
}

fn optional_str(args: &Value, field: &str) -> String {
    args.get(field)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

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
            if let Some(val) = &p.value {
                match p.method {
                    FocusMethod::AppName => args["app_name"] = Value::String(val.clone()),
                    FocusMethod::WindowId => {
                        if let Ok(id) = val.parse::<u64>() {
                            args["window_id"] = serde_json::json!(id);
                        }
                    }
                    FocusMethod::Pid => {
                        if let Ok(pid) = val.parse::<u64>() {
                            args["pid"] = serde_json::json!(pid);
                        }
                    }
                }
            }
            if p.app_kind.uses_cdp() {
                args["app_kind"] = serde_json::to_value(p.app_kind).unwrap();
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

/// Convert a tool name and JSON arguments to a `NodeType`.
///
/// Maps known tool names to typed `NodeType` variants. Unknown tools that exist
/// in `known_tools` are mapped to `McpToolCall`. Unknown tools not in
/// `known_tools` return `Err(UnknownTool)`.
pub fn tool_invocation_to_node_type(
    name: &str,
    args: &Value,
    known_tools: &[Value],
) -> Result<NodeType, ToolMappingError> {
    match name {
        "take_screenshot" => {
            let mode = match args.get("mode").and_then(|v| v.as_str()) {
                Some("screen") => ScreenshotMode::Screen,
                Some("region") => ScreenshotMode::Region,
                _ => ScreenshotMode::Window,
            };
            Ok(NodeType::TakeScreenshot(TakeScreenshotParams {
                mode,
                target: args
                    .get("app_name")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                include_ocr: args
                    .get("include_ocr")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
            }))
        }
        "find_text" => {
            let text = required_str(args, "find_text", "text")?;
            Ok(NodeType::FindText(FindTextParams {
                search_text: text.to_string(),
                scope: args
                    .get("app_name")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                ..Default::default()
            }))
        }
        "find_image" => Ok(NodeType::FindImage(FindImageParams {
            template_image: args
                .get("template_image_base64")
                .or_else(|| args.get("template_id"))
                .and_then(|v| v.as_str())
                .map(String::from),
            threshold: args
                .get("threshold")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.75),
            max_results: args
                .get("max_results")
                .and_then(|v| v.as_u64())
                .unwrap_or(3) as u32,
        })),
        "list_apps" => Ok(NodeType::FindApp(FindAppParams {
            search: optional_str(args, "search"),
        })),
        "click" => {
            let target = if let Some(text) = args
                .get("target")
                .or_else(|| args.get("text"))
                .and_then(|v| v.as_str())
            {
                Some(ClickTarget::Text {
                    text: text.to_string(),
                })
            } else if let (Some(x), Some(y)) = (
                args.get("x").and_then(|v| v.as_f64()),
                args.get("y").and_then(|v| v.as_f64()),
            ) {
                Some(ClickTarget::Coordinates { x, y })
            } else {
                None
            };
            Ok(NodeType::Click(ClickParams {
                target,
                button: match args.get("button").and_then(|v| v.as_str()) {
                    Some("right") => MouseButton::Right,
                    Some("center") => MouseButton::Center,
                    _ => MouseButton::Left,
                },
                click_count: args
                    .get("click_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1) as u32,
                ..Default::default()
            }))
        }
        "type_text" => {
            let text = required_str(args, "type_text", "text")?;
            Ok(NodeType::TypeText(TypeTextParams {
                text: text.to_string(),
                ..Default::default()
            }))
        }
        "press_key" => {
            let key = required_str(args, "press_key", "key")?;
            Ok(NodeType::PressKey(PressKeyParams {
                key: key.to_string(),
                modifiers: args
                    .get("modifiers")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                ..Default::default()
            }))
        }
        "scroll" => Ok(NodeType::Scroll(ScrollParams {
            delta_y: args.get("delta_y").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            x: args.get("x").and_then(|v| v.as_f64()),
            y: args.get("y").and_then(|v| v.as_f64()),
            ..Default::default()
        })),
        "move_mouse" => {
            let target = if let (Some(x), Some(y)) = (
                args.get("x").and_then(|v| v.as_f64()),
                args.get("y").and_then(|v| v.as_f64()),
            ) {
                Some(ClickTarget::Coordinates { x, y })
            } else {
                None
            };
            Ok(NodeType::Hover(HoverParams {
                target,
                dwell_ms: args.get("dwell_ms").and_then(|v| v.as_u64()).unwrap_or(500),
                ..Default::default()
            }))
        }
        "focus_window" => {
            let (method, value) = if let Some(app) = args.get("app_name").and_then(|v| v.as_str()) {
                (FocusMethod::AppName, Some(app.to_string()))
            } else if let Some(wid) = args.get("window_id").and_then(|v| v.as_u64()) {
                (FocusMethod::WindowId, Some(wid.to_string()))
            } else if let Some(pid) = args.get("pid").and_then(|v| v.as_u64()) {
                (FocusMethod::Pid, Some(pid.to_string()))
            } else {
                (FocusMethod::AppName, None)
            };
            let app_kind = args
                .get("app_kind")
                .and_then(|v| v.as_str())
                .and_then(AppKind::parse)
                .unwrap_or(AppKind::Native);
            Ok(NodeType::FocusWindow(FocusWindowParams {
                method,
                value,
                bring_to_front: true,
                app_kind,
                chrome_profile_id: None,
                ..Default::default()
            }))
        }
        "drag" => Ok(NodeType::Drag(DragParams {
            from_x: args.get("from_x").and_then(|v| v.as_f64()),
            from_y: args.get("from_y").and_then(|v| v.as_f64()),
            to_x: args.get("to_x").and_then(|v| v.as_f64()),
            to_y: args.get("to_y").and_then(|v| v.as_f64()),
            ..Default::default()
        })),
        "launch_app" => Ok(NodeType::LaunchApp(LaunchAppParams {
            app_name: optional_str(args, "app_name"),
            ..Default::default()
        })),
        "quit_app" => Ok(NodeType::QuitApp(QuitAppParams {
            app_name: optional_str(args, "app_name"),
            ..Default::default()
        })),
        // CDP tool mappings — prefixed names for planner disambiguation
        "cdp_click" => {
            let uid = optional_str(args, "uid");
            let target_str = args
                .get("target")
                .or_else(|| args.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Both `uid` and `target`/`text` from the planner are exact element
            // labels (the prompt instructs "use the exact element name from
            // cdp_find_elements"). Intent is only constructed programmatically
            // for runtime-resolution paths, never from planner output.
            let label = if !uid.is_empty() { uid } else { target_str };
            let target = if label.is_empty() {
                CdpTarget::default()
            } else {
                CdpTarget::ExactLabel(label)
            };
            Ok(NodeType::CdpClick(CdpClickParams {
                target,
                ..Default::default()
            }))
        }
        "cdp_hover" => {
            let uid = optional_str(args, "uid");
            let target_str = args
                .get("target")
                .or_else(|| args.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let label = if !uid.is_empty() { uid } else { target_str };
            let target = if label.is_empty() {
                CdpTarget::default()
            } else {
                CdpTarget::ExactLabel(label)
            };
            Ok(NodeType::CdpHover(CdpHoverParams {
                target,
                ..Default::default()
            }))
        }
        "cdp_type_text" => Ok(NodeType::CdpType(CdpTypeParams {
            text: optional_str(args, "text"),
            ..Default::default()
        })),
        "cdp_press_key" => Ok(NodeType::CdpPressKey(CdpPressKeyParams {
            key: optional_str(args, "key"),
            modifiers: args
                .get("modifiers")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            ..Default::default()
        })),
        // CDP tool mappings — accept both old (fill, navigate_page) and new (cdp_fill, cdp_navigate) names
        "fill" | "cdp_fill" => Ok(NodeType::CdpFill(CdpFillParams {
            target: CdpTarget::ExactLabel(optional_str(args, "uid")),
            value: optional_str(args, "value"),
            ..Default::default()
        })),
        "navigate_page" | "cdp_navigate" => Ok(NodeType::CdpNavigate(CdpNavigateParams {
            url: optional_str(args, "url"),
            ..Default::default()
        })),
        "new_page" | "cdp_new_page" => Ok(NodeType::CdpNewPage(CdpNewPageParams {
            url: optional_str(args, "url"),
            ..Default::default()
        })),
        "close_page" | "cdp_close_page" => Ok(NodeType::CdpClosePage(CdpClosePageParams {
            page_index: args
                .get("page_index")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            ..Default::default()
        })),
        "select_page" | "cdp_select_page" => Ok(NodeType::CdpSelectPage(CdpSelectPageParams {
            page_index: args.get("page_index").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            ..Default::default()
        })),
        "wait_for" | "cdp_wait_for" => Ok(NodeType::CdpWait(CdpWaitParams {
            text: optional_str(args, "text"),
            timeout_ms: args
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(10_000),
        })),
        "handle_dialog" | "cdp_handle_dialog" => {
            Ok(NodeType::CdpHandleDialog(CdpHandleDialogParams {
                accept: args.get("accept").and_then(|v| v.as_bool()).unwrap_or(true),
                prompt_text: args
                    .get("prompt_text")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                ..Default::default()
            }))
        }
        // CDP inspection tools — available after cdp_connect, not always in known_tools
        "cdp_take_snapshot" | "cdp_list_pages" => Ok(NodeType::McpToolCall(McpToolCallParams {
            tool_name: name.to_string(),
            arguments: args.clone(),
        })),
        _ if known_tools
            .iter()
            .any(|t| t["function"]["name"].as_str() == Some(name)) =>
        {
            Ok(NodeType::McpToolCall(McpToolCallParams {
                tool_name: name.to_string(),
                arguments: args.clone(),
            }))
        }
        _ => Err(ToolMappingError::UnknownTool(name.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppDebugKitParams;

    fn sample_tools() -> Vec<Value> {
        vec![
            serde_json::json!({"type": "function", "function": {"name": "take_screenshot"}}),
            serde_json::json!({"type": "function", "function": {"name": "find_text"}}),
            serde_json::json!({"type": "function", "function": {"name": "find_image"}}),
            serde_json::json!({"type": "function", "function": {"name": "click"}}),
            serde_json::json!({"type": "function", "function": {"name": "type_text"}}),
            serde_json::json!({"type": "function", "function": {"name": "press_key"}}),
            serde_json::json!({"type": "function", "function": {"name": "scroll"}}),
            serde_json::json!({"type": "function", "function": {"name": "focus_window"}}),
            serde_json::json!({"type": "function", "function": {"name": "custom_tool"}}),
        ]
    }

    #[test]
    fn roundtrip_take_screenshot() {
        let nt = NodeType::TakeScreenshot(TakeScreenshotParams {
            mode: ScreenshotMode::Window,
            target: Some("Safari".into()),
            include_ocr: true,
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "take_screenshot");
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(
            matches!(back, NodeType::TakeScreenshot(p) if p.target.as_deref() == Some("Safari"))
        );
    }

    #[test]
    fn roundtrip_take_screenshot_screen_mode() {
        let nt = NodeType::TakeScreenshot(TakeScreenshotParams {
            mode: ScreenshotMode::Screen,
            target: None,
            include_ocr: false,
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(
            matches!(back, NodeType::TakeScreenshot(p) if p.mode == ScreenshotMode::Screen && !p.include_ocr)
        );
    }

    #[test]
    fn roundtrip_find_text() {
        let nt = NodeType::FindText(FindTextParams {
            search_text: "Login".into(),
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "find_text");
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(matches!(back, NodeType::FindText(p) if p.search_text == "Login"));
    }

    #[test]
    fn roundtrip_find_image() {
        let nt = NodeType::FindImage(FindImageParams {
            template_image: Some("abc123".into()),
            threshold: 0.9,
            max_results: 5,
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "find_image");
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(matches!(back, NodeType::FindImage(p) if p.threshold == 0.9 && p.max_results == 5));
    }

    #[test]
    fn roundtrip_click() {
        let nt = NodeType::Click(ClickParams {
            target: Some(ClickTarget::Coordinates { x: 100.0, y: 200.0 }),
            button: MouseButton::Right,
            click_count: 2,
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "click");
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(
            matches!(back, NodeType::Click(p) if p.button == MouseButton::Right && p.click_count == 2)
        );
    }

    #[test]
    fn click_with_target_omits_target_from_invocation() {
        let nt = NodeType::Click(ClickParams {
            target: Some(ClickTarget::Text {
                text: "Submit".into(),
            }),
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "click");
        assert!(inv.arguments.get("target").is_none());
    }

    #[test]
    fn roundtrip_click_no_coords() {
        let nt = NodeType::Click(ClickParams {
            target: None,
            button: MouseButton::Left,
            click_count: 1,
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(matches!(back, NodeType::Click(p) if p.target.is_none()));
    }

    #[test]
    fn roundtrip_type_text() {
        let nt = NodeType::TypeText(TypeTextParams {
            text: "hello world".into(),
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "type_text");
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(matches!(back, NodeType::TypeText(p) if p.text == "hello world"));
    }

    #[test]
    fn roundtrip_press_key() {
        let nt = NodeType::PressKey(PressKeyParams {
            key: "return".into(),
            modifiers: vec!["command".into(), "shift".into()],
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "press_key");
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(
            matches!(back, NodeType::PressKey(p) if p.key == "return" && p.modifiers.len() == 2)
        );
    }

    #[test]
    fn roundtrip_scroll() {
        let nt = NodeType::Scroll(ScrollParams {
            delta_y: -3,
            x: Some(400.0),
            y: Some(300.0),
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "scroll");
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(matches!(back, NodeType::Scroll(p) if p.delta_y == -3 && p.x == Some(400.0)));
    }

    #[test]
    fn roundtrip_focus_window_app_name() {
        let nt = NodeType::FocusWindow(FocusWindowParams {
            method: FocusMethod::AppName,
            value: Some("Safari".into()),
            bring_to_front: true,
            app_kind: AppKind::Native,
            chrome_profile_id: None,
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "focus_window");
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(
            matches!(back, NodeType::FocusWindow(p) if p.method == FocusMethod::AppName && p.value.as_deref() == Some("Safari"))
        );
    }

    #[test]
    fn roundtrip_focus_window_window_id() {
        let nt = NodeType::FocusWindow(FocusWindowParams {
            method: FocusMethod::WindowId,
            value: Some("42".into()),
            bring_to_front: true,
            app_kind: AppKind::Native,
            chrome_profile_id: None,
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(
            matches!(back, NodeType::FocusWindow(p) if p.method == FocusMethod::WindowId && p.value.as_deref() == Some("42"))
        );
    }

    #[test]
    fn roundtrip_focus_window_pid() {
        let nt = NodeType::FocusWindow(FocusWindowParams {
            method: FocusMethod::Pid,
            value: Some("1234".into()),
            bring_to_front: true,
            app_kind: AppKind::Native,
            chrome_profile_id: None,
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(
            matches!(back, NodeType::FocusWindow(p) if p.method == FocusMethod::Pid && p.value.as_deref() == Some("1234"))
        );
    }

    #[test]
    fn roundtrip_mcp_tool_call() {
        let tools = sample_tools();
        let nt = NodeType::McpToolCall(McpToolCallParams {
            tool_name: "custom_tool".into(),
            arguments: serde_json::json!({"foo": "bar"}),
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "custom_tool");
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &tools).unwrap();
        assert!(matches!(back, NodeType::McpToolCall(p) if p.tool_name == "custom_tool"));
    }

    #[test]
    fn ai_step_is_not_a_tool_node() {
        let nt = NodeType::AiStep(crate::AiStepParams::default());
        assert!(matches!(
            node_type_to_tool_invocation(&nt),
            Err(ToolMappingError::NotAToolNode)
        ));
    }

    #[test]
    fn app_debug_kit_is_not_a_tool_node() {
        let nt = NodeType::AppDebugKitOp(AppDebugKitParams::default());
        assert!(matches!(
            node_type_to_tool_invocation(&nt),
            Err(ToolMappingError::NotAToolNode)
        ));
    }

    #[test]
    fn unknown_tool_not_in_schema_errors() {
        assert!(matches!(
            tool_invocation_to_node_type("nonexistent", &serde_json::json!({}), &[]),
            Err(ToolMappingError::UnknownTool(_))
        ));
    }

    #[test]
    fn find_text_missing_text_errors() {
        assert!(matches!(
            tool_invocation_to_node_type("find_text", &serde_json::json!({}), &[]),
            Err(ToolMappingError::MissingArgument { .. })
        ));
    }

    #[test]
    fn type_text_empty_text_errors() {
        assert!(matches!(
            tool_invocation_to_node_type("type_text", &serde_json::json!({"text": ""}), &[]),
            Err(ToolMappingError::MissingArgument { .. })
        ));
    }

    #[test]
    fn press_key_missing_key_errors() {
        assert!(matches!(
            tool_invocation_to_node_type("press_key", &serde_json::json!({}), &[]),
            Err(ToolMappingError::MissingArgument { .. })
        ));
    }

    #[test]
    fn unknown_tool_in_schema_maps_to_mcp_tool_call() {
        let tools = sample_tools();
        let result = tool_invocation_to_node_type(
            "custom_tool",
            &serde_json::json!({"key": "value"}),
            &tools,
        )
        .unwrap();
        assert!(matches!(result, NodeType::McpToolCall(p) if p.tool_name == "custom_tool"));
    }

    #[test]
    fn roundtrip_focus_window_preserves_app_kind() {
        let nt = NodeType::FocusWindow(FocusWindowParams {
            method: FocusMethod::AppName,
            value: Some("Chrome".into()),
            bring_to_front: true,
            app_kind: AppKind::ChromeBrowser,
            chrome_profile_id: None,
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.arguments["app_kind"], "ChromeBrowser");
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        match back {
            NodeType::FocusWindow(p) => assert_eq!(p.app_kind, AppKind::ChromeBrowser),
            _ => panic!("expected FocusWindow"),
        }
    }

    #[test]
    fn roundtrip_focus_window_omits_native_app_kind() {
        let nt = NodeType::FocusWindow(FocusWindowParams {
            method: FocusMethod::AppName,
            value: Some("Calculator".into()),
            bring_to_front: true,
            app_kind: AppKind::Native,
            chrome_profile_id: None,
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert!(inv.arguments.get("app_kind").is_none());
    }

    #[test]
    fn focus_window_with_app_kind_chrome() {
        let args = serde_json::json!({"app_name": "Chrome", "app_kind": "ChromeBrowser"});
        let nt = tool_invocation_to_node_type("focus_window", &args, &[]).unwrap();
        match nt {
            NodeType::FocusWindow(p) => {
                assert_eq!(p.value.as_deref(), Some("Chrome"));
                assert_eq!(p.app_kind, AppKind::ChromeBrowser);
            }
            _ => panic!("expected FocusWindow"),
        }
    }

    #[test]
    fn hover_maps_to_move_mouse() {
        let nt = NodeType::Hover(HoverParams {
            target: Some(ClickTarget::Coordinates { x: 100.0, y: 200.0 }),
            dwell_ms: 500,
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "move_mouse");
        assert_eq!(inv.arguments["x"], 100.0);
        assert_eq!(inv.arguments["y"], 200.0);
    }

    #[test]
    fn hover_no_coords_maps_to_move_mouse_without_xy() {
        let nt = NodeType::Hover(HoverParams::default());
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "move_mouse");
        assert!(inv.arguments.get("x").is_none());
        assert!(inv.arguments.get("y").is_none());
    }

    #[test]
    fn roundtrip_move_mouse() {
        let nt = NodeType::Hover(HoverParams {
            target: Some(ClickTarget::Coordinates { x: 100.0, y: 200.0 }),
            dwell_ms: 500,
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(matches!(back, NodeType::Hover(_)));
    }

    #[test]
    fn focus_window_without_app_kind_defaults_native() {
        let args = serde_json::json!({"app_name": "Calculator"});
        let nt = tool_invocation_to_node_type("focus_window", &args, &[]).unwrap();
        match nt {
            NodeType::FocusWindow(p) => {
                assert_eq!(p.app_kind, AppKind::Native);
            }
            _ => panic!("expected FocusWindow"),
        }
    }

    /// CDP inspection tools that appear after cdp_connect must resolve
    /// without known_tools. The full CDP_ACTION_TOOLS list is tested in
    /// clickweave-llm (which owns the constant and depends on this crate).
    #[test]
    fn cdp_inspection_tools_resolve_without_known_tools() {
        let empty: Vec<Value> = vec![];
        for name in ["cdp_take_snapshot", "cdp_list_pages"] {
            let result = tool_invocation_to_node_type(name, &serde_json::json!({}), &empty);
            assert!(
                result.is_ok(),
                "'{}' must be hardcoded — it appears after cdp_connect \
                 and known_tools may be stale",
                name
            );
        }
    }
}
