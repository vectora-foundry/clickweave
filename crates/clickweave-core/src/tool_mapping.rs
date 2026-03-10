//! Shared mapping between `NodeType` and MCP tool invocations.
//!
//! Used by both the planner (tool args → NodeType) and the executor (NodeType → tool args).

use crate::{
    ClickParams, ClickTarget, FindImageParams, FindTextParams, FocusMethod, FocusWindowParams,
    HoverParams, ListWindowsParams, McpToolCallParams, MouseButton, NodeType, PressKeyParams,
    ScreenshotMode, ScrollParams, TakeScreenshotParams, TypeTextParams, walkthrough::AppKind,
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
            if let Some(x) = p.x {
                args["x"] = serde_json::json!(x);
            }
            if let Some(y) = p.y {
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
        NodeType::ListWindows(p) => {
            let mut args = serde_json::json!({});
            if let Some(app) = &p.app_name {
                args["app_name"] = Value::String(app.clone());
            }
            ("list_windows", args)
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
            if let Some(x) = p.x {
                args["x"] = serde_json::json!(x);
            }
            if let Some(y) = p.y {
                args["y"] = serde_json::json!(y);
            }
            ("move_mouse", args)
        }
        NodeType::McpToolCall(p) => {
            let args = if p.arguments.is_null() {
                serde_json::json!({})
            } else {
                p.arguments.clone()
            };
            (&*p.tool_name, args)
        }
        NodeType::AiStep(_)
        | NodeType::AppDebugKitOp(_)
        | NodeType::If(_)
        | NodeType::Switch(_)
        | NodeType::Loop(_)
        | NodeType::EndLoop(_) => {
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
        "click" => Ok(NodeType::Click(ClickParams {
            target: args
                .get("target")
                .or_else(|| args.get("text"))
                .and_then(|v| v.as_str())
                .map(|s| ClickTarget::Text {
                    text: s.to_string(),
                }),
            template_image: None,
            x: args.get("x").and_then(|v| v.as_f64()),
            y: args.get("y").and_then(|v| v.as_f64()),
            button: match args.get("button").and_then(|v| v.as_str()) {
                Some("right") => MouseButton::Right,
                Some("center") => MouseButton::Center,
                _ => MouseButton::Left,
            },
            click_count: args
                .get("click_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as u32,
        })),
        "type_text" => {
            let text = required_str(args, "type_text", "text")?;
            Ok(NodeType::TypeText(TypeTextParams {
                text: text.to_string(),
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
            }))
        }
        "scroll" => Ok(NodeType::Scroll(ScrollParams {
            delta_y: args.get("delta_y").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            x: args.get("x").and_then(|v| v.as_f64()),
            y: args.get("y").and_then(|v| v.as_f64()),
        })),
        "list_windows" => Ok(NodeType::ListWindows(ListWindowsParams {
            app_name: args
                .get("app_name")
                .and_then(|v| v.as_str())
                .map(String::from),
        })),
        "move_mouse" => Ok(NodeType::Hover(HoverParams {
            target: None,
            template_image: None,
            x: args.get("x").and_then(|v| v.as_f64()),
            y: args.get("y").and_then(|v| v.as_f64()),
            dwell_ms: args.get("dwell_ms").and_then(|v| v.as_u64()).unwrap_or(500),
        })),
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
            }))
        }
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
            serde_json::json!({"type": "function", "function": {"name": "list_windows"}}),
            serde_json::json!({"type": "function", "function": {"name": "focus_window"}}),
            serde_json::json!({"type": "function", "function": {"name": "custom_tool"}}),
        ]
    }

    // --- Round-trip tests for each deterministic node type ---

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
            target: None,
            x: Some(100.0),
            y: Some(200.0),
            button: MouseButton::Right,
            click_count: 2,
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "click");
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(
            matches!(back, NodeType::Click(p) if p.x == Some(100.0) && p.y == Some(200.0) && p.button == MouseButton::Right && p.click_count == 2)
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
        // target is a clickweave-internal field, not an MCP tool argument
        assert!(inv.arguments.get("target").is_none());
    }

    #[test]
    fn roundtrip_click_no_coords() {
        let nt = NodeType::Click(ClickParams {
            target: None,
            x: None,
            y: None,
            button: MouseButton::Left,
            click_count: 1,
            ..Default::default()
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(matches!(back, NodeType::Click(p) if p.x.is_none() && p.y.is_none()));
    }

    #[test]
    fn roundtrip_type_text() {
        let nt = NodeType::TypeText(TypeTextParams {
            text: "hello world".into(),
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
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "scroll");
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(matches!(back, NodeType::Scroll(p) if p.delta_y == -3 && p.x == Some(400.0)));
    }

    #[test]
    fn roundtrip_list_windows() {
        let nt = NodeType::ListWindows(ListWindowsParams {
            app_name: Some("Code".into()),
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "list_windows");
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(matches!(back, NodeType::ListWindows(p) if p.app_name.as_deref() == Some("Code")));
    }

    #[test]
    fn roundtrip_focus_window_app_name() {
        let nt = NodeType::FocusWindow(FocusWindowParams {
            method: FocusMethod::AppName,
            value: Some("Safari".into()),
            bring_to_front: true,
            app_kind: AppKind::Native,
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

    // --- Error cases ---

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
        let nt = NodeType::Hover(crate::HoverParams {
            target: None,
            template_image: None,
            x: Some(100.0),
            y: Some(200.0),
            dwell_ms: 500,
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "move_mouse");
        assert_eq!(inv.arguments["x"], 100.0);
        assert_eq!(inv.arguments["y"], 200.0);
    }

    #[test]
    fn hover_no_coords_maps_to_move_mouse_without_xy() {
        let nt = NodeType::Hover(crate::HoverParams::default());
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        assert_eq!(inv.name, "move_mouse");
        assert!(inv.arguments.get("x").is_none());
        assert!(inv.arguments.get("y").is_none());
    }

    #[test]
    fn roundtrip_move_mouse() {
        let nt = NodeType::Hover(crate::HoverParams {
            target: None,
            template_image: None,
            x: Some(100.0),
            y: Some(200.0),
            dwell_ms: 500,
        });
        let inv = node_type_to_tool_invocation(&nt).unwrap();
        let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
        assert!(matches!(back, NodeType::Hover(p) if p.x == Some(100.0) && p.y == Some(200.0)));
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
}
