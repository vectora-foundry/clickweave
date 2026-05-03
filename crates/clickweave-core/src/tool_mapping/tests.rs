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
    assert!(matches!(back, NodeType::TakeScreenshot(p) if p.target.as_deref() == Some("Safari")));
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
    assert!(matches!(back, NodeType::PressKey(p) if p.key == "return" && p.modifiers.len() == 2));
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
        target: FocusTarget::AppName("Safari".into()),
        bring_to_front: true,
        app_kind: AppKind::Native,
        chrome_profile_id: None,
        ..Default::default()
    });
    let inv = node_type_to_tool_invocation(&nt).unwrap();
    assert_eq!(inv.name, "focus_window");
    let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
    assert!(matches!(
        back,
        NodeType::FocusWindow(p) if p.target == FocusTarget::AppName("Safari".into())
    ));
}

#[test]
fn roundtrip_focus_window_window_id() {
    let nt = NodeType::FocusWindow(FocusWindowParams {
        target: FocusTarget::WindowId(42),
        bring_to_front: true,
        app_kind: AppKind::Native,
        chrome_profile_id: None,
        ..Default::default()
    });
    let inv = node_type_to_tool_invocation(&nt).unwrap();
    let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
    assert!(matches!(
        back,
        NodeType::FocusWindow(p) if p.target == FocusTarget::WindowId(42)
    ));
}

#[test]
fn roundtrip_focus_window_pid() {
    let nt = NodeType::FocusWindow(FocusWindowParams {
        target: FocusTarget::Pid(1234),
        bring_to_front: true,
        app_kind: AppKind::Native,
        chrome_profile_id: None,
        ..Default::default()
    });
    let inv = node_type_to_tool_invocation(&nt).unwrap();
    let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
    assert!(matches!(
        back,
        NodeType::FocusWindow(p) if p.target == FocusTarget::Pid(1234)
    ));
}

#[test]
fn focus_window_rejects_empty_args() {
    let result = tool_invocation_to_node_type("focus_window", &serde_json::json!({}), &[]);
    assert!(matches!(
        result,
        Err(ToolMappingError::MissingArgument { .. })
    ));
}

#[test]
fn focus_window_rejects_non_numeric_window_id() {
    let result = tool_invocation_to_node_type(
        "focus_window",
        &serde_json::json!({"window_id": "not a number"}),
        &[],
    );
    assert!(matches!(
        result,
        Err(ToolMappingError::MissingArgument { .. })
    ));
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
    let result =
        tool_invocation_to_node_type("custom_tool", &serde_json::json!({"key": "value"}), &tools)
            .unwrap();
    assert!(matches!(result, NodeType::McpToolCall(p) if p.tool_name == "custom_tool"));
}

#[test]
fn roundtrip_focus_window_preserves_app_kind() {
    let nt = NodeType::FocusWindow(FocusWindowParams {
        target: FocusTarget::AppName("Chrome".into()),
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
        target: FocusTarget::AppName("Calculator".into()),
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
            assert_eq!(p.target, FocusTarget::AppName("Chrome".into()));
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

// ── AX dispatch roundtrips ───────────────────────────────────────────

#[test]
fn roundtrip_ax_click_descriptor() {
    // Descriptor on the node → outbound sends the descriptor's `name` as
    // `uid`. Inbound reconstructs as `ResolvedUid` because the inbound
    // path does not see the snapshot; agent-loop enrichment later
    // re-upgrades it.
    let nt = NodeType::AxClick(AxClickParams {
        target: AxTarget::Descriptor {
            role: "AXButton".into(),
            name: "Submit".into(),
            parent_name: None,
        },
        ..Default::default()
    });
    let inv = node_type_to_tool_invocation(&nt).unwrap();
    assert_eq!(inv.name, "ax_click");
    assert_eq!(inv.arguments["uid"], "Submit");
    let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
    assert!(
        matches!(back, NodeType::AxClick(p) if p.target == AxTarget::ResolvedUid("Submit".into()))
    );
}

#[test]
fn roundtrip_ax_click_resolved_uid() {
    let nt = NodeType::AxClick(AxClickParams {
        target: AxTarget::ResolvedUid("a42g3".into()),
        ..Default::default()
    });
    let inv = node_type_to_tool_invocation(&nt).unwrap();
    assert_eq!(inv.name, "ax_click");
    assert_eq!(inv.arguments["uid"], "a42g3");
    let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
    assert!(
        matches!(back, NodeType::AxClick(p) if p.target == AxTarget::ResolvedUid("a42g3".into()))
    );
}

#[test]
fn roundtrip_ax_set_value() {
    let nt = NodeType::AxSetValue(AxSetValueParams {
        target: AxTarget::ResolvedUid("a10g1".into()),
        value: "hello".into(),
        ..Default::default()
    });
    let inv = node_type_to_tool_invocation(&nt).unwrap();
    assert_eq!(inv.name, "ax_set_value");
    assert_eq!(inv.arguments["uid"], "a10g1");
    assert_eq!(inv.arguments["value"], "hello");
    let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
    match back {
        NodeType::AxSetValue(p) => {
            assert_eq!(p.target, AxTarget::ResolvedUid("a10g1".into()));
            assert_eq!(p.value, "hello");
        }
        _ => panic!("expected AxSetValue"),
    }
}

#[test]
fn roundtrip_ax_select() {
    let nt = NodeType::AxSelect(AxSelectParams {
        target: AxTarget::ResolvedUid("a7g2".into()),
        ..Default::default()
    });
    let inv = node_type_to_tool_invocation(&nt).unwrap();
    assert_eq!(inv.name, "ax_select");
    assert_eq!(inv.arguments["uid"], "a7g2");
    let back = tool_invocation_to_node_type(&inv.name, &inv.arguments, &[]).unwrap();
    assert!(
        matches!(back, NodeType::AxSelect(p) if p.target == AxTarget::ResolvedUid("a7g2".into()))
    );
}

#[test]
fn ax_tools_resolve_without_known_tools() {
    // AX tools are macOS-only on the MCP server side, so in a non-macOS
    // known_tools list they won't appear — but when replayed on macOS or
    // deserialized from a persisted workflow, we still need to map them.
    let empty: Vec<Value> = vec![];
    for (name, args) in [
        ("ax_click", serde_json::json!({"uid": "a1g1"})),
        (
            "ax_set_value",
            serde_json::json!({"uid": "a2g1", "value": "x"}),
        ),
        ("ax_select", serde_json::json!({"uid": "a3g1"})),
    ] {
        let result = tool_invocation_to_node_type(name, &args, &empty);
        assert!(result.is_ok(), "'{}' must be hardcoded", name);
    }
}
