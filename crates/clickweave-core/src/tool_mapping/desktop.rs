use super::*;

pub(super) fn desktop_tool_invocation_to_node_type(
    name: &str,
    args: &Value,
) -> Result<Option<NodeType>, ToolMappingError> {
    match name {
        "take_screenshot" => {
            let mode = match args.get("mode").and_then(|v| v.as_str()) {
                Some("screen") => ScreenshotMode::Screen,
                Some("region") => ScreenshotMode::Region,
                _ => ScreenshotMode::Window,
            };
            Ok(Some(NodeType::TakeScreenshot(TakeScreenshotParams {
                mode,
                target: args
                    .get("app_name")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                include_ocr: args
                    .get("include_ocr")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
            })))
        }
        "find_text" => {
            let text = required_str(args, "find_text", "text")?;
            Ok(Some(NodeType::FindText(FindTextParams {
                search_text: text.to_string(),
                scope: args
                    .get("app_name")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            })))
        }
        "find_image" => Ok(Some(NodeType::FindImage(FindImageParams {
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
        }))),
        "list_apps" => Ok(Some(NodeType::FindApp(FindAppParams {
            search: optional_str(args, "search"),
        }))),
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
            Ok(Some(NodeType::Click(ClickParams {
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
            })))
        }
        "type_text" => {
            let text = required_str(args, "type_text", "text")?;
            Ok(Some(NodeType::TypeText(TypeTextParams {
                text: text.to_string(),
                ..Default::default()
            })))
        }
        "press_key" => {
            let key = required_str(args, "press_key", "key")?;
            Ok(Some(NodeType::PressKey(PressKeyParams {
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
            })))
        }
        "scroll" => Ok(Some(NodeType::Scroll(ScrollParams {
            delta_y: args.get("delta_y").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            x: args.get("x").and_then(|v| v.as_f64()),
            y: args.get("y").and_then(|v| v.as_f64()),
            ..Default::default()
        }))),
        "move_mouse" => {
            let target = if let (Some(x), Some(y)) = (
                args.get("x").and_then(|v| v.as_f64()),
                args.get("y").and_then(|v| v.as_f64()),
            ) {
                Some(ClickTarget::Coordinates { x, y })
            } else {
                None
            };
            Ok(Some(NodeType::Hover(HoverParams {
                target,
                dwell_ms: args.get("dwell_ms").and_then(|v| v.as_u64()).unwrap_or(500),
                ..Default::default()
            })))
        }
        "focus_window" => {
            // Require exactly one of `app_name` / `window_id` / `pid`. Reject
            // malformed input (missing all three, or wrong types) with a typed
            // `MissingArgument` error rather than silently stringifying a parse
            // failure as was done before.
            let target = if let Some(app) = args.get("app_name").and_then(|v| v.as_str()) {
                FocusTarget::AppName(app.to_string())
            } else if args.get("window_id").is_some() {
                let wid = args
                    .get("window_id")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| ToolMappingError::MissingArgument {
                        tool: "focus_window".into(),
                        argument: "window_id".into(),
                    })?;
                FocusTarget::WindowId(wid)
            } else if args.get("pid").is_some() {
                let pid_u64 = args.get("pid").and_then(|v| v.as_u64()).ok_or_else(|| {
                    ToolMappingError::MissingArgument {
                        tool: "focus_window".into(),
                        argument: "pid".into(),
                    }
                })?;
                let pid =
                    u32::try_from(pid_u64).map_err(|_| ToolMappingError::MissingArgument {
                        tool: "focus_window".into(),
                        argument: "pid".into(),
                    })?;
                FocusTarget::Pid(pid)
            } else {
                return Err(ToolMappingError::MissingArgument {
                    tool: "focus_window".into(),
                    argument: "app_name|window_id|pid".into(),
                });
            };
            let app_kind = args
                .get("app_kind")
                .and_then(|v| v.as_str())
                .and_then(AppKind::parse)
                .unwrap_or(AppKind::Native);
            Ok(Some(NodeType::FocusWindow(FocusWindowParams {
                target,
                bring_to_front: true,
                app_kind,
                chrome_profile_id: None,
                ..Default::default()
            })))
        }
        "drag" => Ok(Some(NodeType::Drag(DragParams {
            from_x: args.get("from_x").and_then(|v| v.as_f64()),
            from_y: args.get("from_y").and_then(|v| v.as_f64()),
            to_x: args.get("to_x").and_then(|v| v.as_f64()),
            to_y: args.get("to_y").and_then(|v| v.as_f64()),
            ..Default::default()
        }))),
        "launch_app" => Ok(Some(NodeType::LaunchApp(LaunchAppParams {
            app_name: optional_str(args, "app_name"),
            ..Default::default()
        }))),
        "quit_app" => Ok(Some(NodeType::QuitApp(QuitAppParams {
            app_name: optional_str(args, "app_name"),
            ..Default::default()
        }))),
        _ => Ok(None),
    }
}
