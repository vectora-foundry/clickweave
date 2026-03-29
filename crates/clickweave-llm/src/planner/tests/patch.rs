use super::helpers::*;
use crate::planner::prompt::patcher_system_prompt;
use crate::planner::*;
use clickweave_core::{
    CdpPressKeyParams, ClickParams, ClickTarget, FindTextParams, FocusMethod, FocusWindowParams,
    MouseButton, NodeType, Position, ScreenshotMode, TakeScreenshotParams,
};

#[test]
fn test_patcher_prompt_includes_node_arguments() {
    let mut workflow = Workflow::new("Test");
    workflow.add_node(
        NodeType::FocusWindow(FocusWindowParams {
            method: FocusMethod::AppName,
            value: Some("Signal".into()),
            bring_to_front: true,
            app_kind: clickweave_core::AppKind::Native,
            chrome_profile_id: None,
            ..Default::default()
        }),
        Position { x: 0.0, y: 0.0 },
    );
    workflow.add_node(
        NodeType::FindText(FindTextParams {
            search_text: "Vesna".into(),
            ..Default::default()
        }),
        Position { x: 0.0, y: 100.0 },
    );
    workflow.add_node(
        NodeType::Click(ClickParams {
            target: Some(ClickTarget::Text {
                text: "Vesna".into(),
            }),
            ..Default::default()
        }),
        Position { x: 0.0, y: 200.0 },
    );

    let prompt = patcher_system_prompt(&workflow, &sample_tools(), false, false, false);

    // Must contain the actual tool arguments so the LLM knows what to change
    assert!(
        prompt.contains("\"text\": \"Vesna\""),
        "Patcher prompt must include find_text arguments"
    );
    assert!(
        prompt.contains("\"tool_name\": \"find_text\""),
        "Patcher prompt must include tool_name"
    );
    assert!(
        prompt.contains("\"tool_name\": \"focus_window\""),
        "Patcher prompt must include focus_window tool_name"
    );
    // Click target is internal but must appear in prompt for the LLM
    assert!(
        prompt.contains("\"target\": \"Vesna\""),
        "Patcher prompt must include click target"
    );
}

#[tokio::test]
async fn test_patch_adds_node() {
    let (_id, workflow) = single_node_workflow(
        NodeType::TakeScreenshot(TakeScreenshotParams {
            mode: ScreenshotMode::Window,
            target: None,
            include_ocr: true,
        }),
        "Screenshot",
    );

    let response = r#"{"add": [{"step_type": "Tool", "tool_name": "click", "arguments": {"x": 100, "y": 200}}]}"#;
    let mock = MockBackend::single(response);

    let result = patch_with_mock(&mock, &workflow, "Add a click after the screenshot")
        .await
        .unwrap();

    assert_eq!(result.added_nodes.len(), 1);
    assert!(matches!(
        result.added_nodes[0].node_type,
        NodeType::Click(_)
    ));
    assert_eq!(result.added_edges.len(), 1);
    assert_eq!(result.added_edges[0].from, workflow.nodes[0].id);
}

#[tokio::test]
async fn test_patch_removes_node() {
    let (node_id, workflow) = single_node_workflow(
        NodeType::Click(ClickParams {
            target: Some(ClickTarget::Coordinates { x: 100.0, y: 200.0 }),
            button: MouseButton::Left,
            click_count: 1,
            ..Default::default()
        }),
        "Click",
    );

    let response = format!(r#"{{"remove_node_ids": ["{}"]}}"#, node_id);
    let mock = MockBackend::single(&response);

    let result = patch_with_mock(&mock, &workflow, "Remove the click")
        .await
        .unwrap();

    assert_eq!(result.removed_node_ids.len(), 1);
    assert_eq!(result.removed_node_ids[0], node_id);
}

#[tokio::test]
async fn test_patch_add_filters_disallowed_step_types() {
    let (_id, workflow) = single_node_workflow(
        NodeType::TakeScreenshot(TakeScreenshotParams {
            mode: ScreenshotMode::Window,
            target: None,
            include_ocr: true,
        }),
        "Screenshot",
    );

    // Patcher tries to add an AiStep and an AiTransform, but both flags are disabled
    let response = r#"{"add": [
        {"step_type": "AiStep", "prompt": "Decide what to do"},
        {"step_type": "AiTransform", "kind": "summarize", "input_ref": "step1"},
        {"step_type": "Tool", "tool_name": "click", "arguments": {"x": 50, "y": 50}}
    ]}"#;
    let mock = MockBackend::single(response);

    let result = patch_with_mock(&mock, &workflow, "Add some steps")
        .await
        .unwrap();

    // Only the Tool step should survive
    assert_eq!(result.added_nodes.len(), 1);
    assert!(matches!(
        result.added_nodes[0].node_type,
        NodeType::Click(_)
    ));
    assert!(result.warnings.len() >= 2);
}

#[tokio::test]
async fn test_patch_update_with_flat_arguments_only() {
    let (node_id, workflow) = single_node_workflow(
        NodeType::FindText(FindTextParams {
            search_text: "Vesna".into(),
            ..Default::default()
        }),
        "Find Vesna",
    );

    // LLM returns only `arguments` (no tool_name, no node_type) -- tool inferred from existing node
    let response = format!(
        r#"{{"update": [{{"node_id": "{}", "name": "Find Me", "arguments": {{"text": "Me"}}}}]}}"#,
        node_id
    );
    let mock = MockBackend::single(&response);

    let result = patch_with_mock(&mock, &workflow, "Change target")
        .await
        .unwrap();

    assert_eq!(result.updated_nodes.len(), 1);
    assert_eq!(result.updated_nodes[0].name, "Find Me");
    match &result.updated_nodes[0].node_type {
        NodeType::FindText(p) => assert_eq!(p.search_text, "Me"),
        other => panic!("Expected FindText, got {:?}", other),
    }
}

#[tokio::test]
async fn test_patch_update_with_flat_tool_name_and_arguments() {
    let (node_id, workflow) = single_node_workflow(
        NodeType::FindText(FindTextParams {
            search_text: "old".into(),
            ..Default::default()
        }),
        "Find Old",
    );

    // LLM returns `tool_name` + `arguments` (no nested node_type)
    let response = format!(
        r#"{{"update": [{{"node_id": "{}", "tool_name": "type_text", "arguments": {{"text": "hello"}}}}]}}"#,
        node_id
    );
    let mock = MockBackend::single(&response);

    let result = patch_with_mock(&mock, &workflow, "Change to type_text")
        .await
        .unwrap();

    assert_eq!(result.updated_nodes.len(), 1);
    match &result.updated_nodes[0].node_type {
        NodeType::TypeText(p) => assert_eq!(p.text, "hello"),
        other => panic!("Expected TypeText, got {:?}", other),
    }
}

#[tokio::test]
async fn test_patch_update_rejects_disallowed_node_type_change() {
    let (node_id, workflow) = single_node_workflow(
        NodeType::Click(ClickParams {
            target: Some(ClickTarget::Coordinates { x: 100.0, y: 200.0 }),
            button: MouseButton::Left,
            click_count: 1,
            ..Default::default()
        }),
        "Click",
    );

    // Try to update the node to an AiStep with agent steps disabled
    let response = format!(
        r#"{{"update": [{{"node_id": "{}", "node_type": {{"step_type": "AiStep", "prompt": "do something"}}}}]}}"#,
        node_id
    );
    let mock = MockBackend::single(&response);

    let result = patch_with_mock(&mock, &workflow, "Change to AI")
        .await
        .unwrap();

    // Node should still be in updated_nodes (name update still applies) but type unchanged
    assert_eq!(result.updated_nodes.len(), 1);
    assert!(matches!(
        result.updated_nodes[0].node_type,
        NodeType::Click(_)
    ));
    assert!(!result.warnings.is_empty());
}

#[tokio::test]
async fn test_patch_repair_pass_fixes_invalid_json() {
    let (_id, workflow) = single_node_workflow(
        NodeType::TakeScreenshot(TakeScreenshotParams {
            mode: ScreenshotMode::Window,
            target: None,
            include_ocr: true,
        }),
        "Screenshot",
    );

    let bad_response = r#"Here's the patch: {"add": [invalid}]}"#;
    let good_response = r#"{"add": [{"step_type": "Tool", "tool_name": "click", "arguments": {"x": 50, "y": 50}}]}"#;
    let mock = MockBackend::new(vec![bad_response, good_response]);

    let result = patch_with_mock(&mock, &workflow, "Add a click")
        .await
        .unwrap();

    assert_eq!(result.added_nodes.len(), 1);
    assert_eq!(mock.call_count(), 2);
}

#[tokio::test]
async fn test_patch_repair_pass_fails_after_max_attempts() {
    let (_id, workflow) = single_node_workflow(
        NodeType::TakeScreenshot(TakeScreenshotParams {
            mode: ScreenshotMode::Window,
            target: None,
            include_ocr: true,
        }),
        "Screenshot",
    );

    let bad = r#"not json at all"#;
    let mock = MockBackend::new(vec![bad, bad]);

    let result = patch_with_mock(&mock, &workflow, "Add a click").await;

    assert!(result.is_err());
    assert_eq!(mock.call_count(), 2);
}

#[tokio::test]
async fn test_patch_adds_loop() {
    let (_id, workflow) = single_node_workflow(
        NodeType::FocusWindow(FocusWindowParams {
            method: FocusMethod::AppName,
            value: Some("Calculator".into()),
            bring_to_front: true,
            app_kind: clickweave_core::AppKind::Native,
            chrome_profile_id: None,
            ..Default::default()
        }),
        "Focus Calculator",
    );

    let response = format!(
        r#"{{
        "add_nodes": [
            {{"id": "n1", "step_type": "Loop", "name": "Repeat", "exit_condition": {{
                "left": {{"node": "check", "field": "found"}},
                "operator": "Equals",
                "right": {{"type": "Literal", "value": {{"type": "Bool", "value": true}}}}
            }}, "max_iterations": 10}},
            {{"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {{"target": "="}}, "name": "Click"}},
            {{"id": "n3", "step_type": "EndLoop", "loop_id": "n1", "name": "End Loop"}}
        ],
        "add_edges": [
            {{"from": "{}", "to": "n1"}},
            {{"from": "n1", "to": "n2", "output": {{"type": "LoopBody"}}}},
            {{"from": "n2", "to": "n3"}},
            {{"from": "n3", "to": "n1"}}
        ]
    }}"#,
        workflow.nodes[0].id
    );

    let mock = MockBackend::single(&response);
    let result = patch_with_mock(&mock, &workflow, "Add a loop")
        .await
        .unwrap();

    assert_eq!(result.added_nodes.len(), 3);
    assert!(
        result
            .added_nodes
            .iter()
            .any(|n| matches!(n.node_type, NodeType::Loop(_)))
    );
    assert!(
        result
            .added_nodes
            .iter()
            .any(|n| matches!(n.node_type, NodeType::EndLoop(_)))
    );
    // Verify edges were created (from existing node to new loop + internal edges)
    assert!(result.added_edges.len() >= 3);
}

#[test]
fn test_mixed_add_and_add_nodes_warns_and_skips_flat() {
    let (_id, workflow) = single_node_workflow(
        NodeType::FocusWindow(FocusWindowParams {
            method: FocusMethod::AppName,
            value: Some("Calculator".to_string()),
            bring_to_front: true,
            app_kind: clickweave_core::AppKind::Native,
            chrome_profile_id: None,
            ..Default::default()
        }),
        "Focus Calculator",
    );

    let output: PatcherOutput = serde_json::from_str(r#"{
        "add": [{"step_type": "Tool", "tool_name": "click", "arguments": {"x": 10, "y": 20}}],
        "add_nodes": [
            {"id": "n1", "step_type": "Tool", "tool_name": "click", "arguments": {"x": 30, "y": 40}, "name": "Click 2"}
        ],
        "add_edges": []
    }"#).unwrap();

    let patch = build_patch_from_output(&output, &workflow, &sample_tools(), false, false);

    // Only graph-based node should be added; flat add should be skipped
    assert_eq!(
        patch.added_nodes.len(),
        1,
        "flat add items should be skipped"
    );
    assert!(
        patch
            .warnings
            .iter()
            .any(|w| w.contains("flat 'add' steps")),
        "should warn about ignored flat adds: {:?}",
        patch.warnings,
    );
}

#[tokio::test]
async fn test_malformed_patcher_update_skipped() {
    // Patcher returns a malformed update entry — should be skipped, not crash.
    let (node_id, workflow) = single_node_workflow(
        NodeType::FindText(FindTextParams {
            search_text: "test".into(),
            ..Default::default()
        }),
        "Find test",
    );

    let response = format!(
        r#"{{
        "update": [
            {{"node_id": "{}", "name": "Find Updated", "arguments": {{"text": "updated"}}}},
            {{"completely": "invalid", "garbage": true}}
        ]
    }}"#,
        node_id
    );
    let mock = MockBackend::single(&response);

    let result = patch_with_mock(&mock, &workflow, "Update and garbage")
        .await
        .unwrap();

    // Valid update should apply, malformed entry skipped
    assert_eq!(result.updated_nodes.len(), 1);
    assert_eq!(result.updated_nodes[0].name, "Find Updated");
    assert!(
        result.warnings.iter().any(|w| w.contains("malformed")),
        "Expected malformed update warning, got: {:?}",
        result.warnings
    );
}

/// Regression test: a resolution update that changes tool_name from
/// cdp_press_key to cdp_click must produce NodeType::CdpClick, not
/// silently drop the change. This was the root cause of the Signal
/// "Press Enter" bug where the LLM renamed the node but the action
/// stayed as PressKey because the prompt forbade tool changes.
#[test]
fn update_tool_name_changes_cdp_press_key_to_cdp_click() {
    let mut workflow = Workflow::new("test-resolution");
    let node_id = workflow.add_node(
        NodeType::CdpPressKey(CdpPressKeyParams {
            key: "Enter".to_string(),
            ..Default::default()
        }),
        Position { x: 0.0, y: 0.0 },
    );

    let patcher_output = serde_json::from_value::<PatcherOutput>(serde_json::json!({
        "update": [{
            "node_id": node_id.to_string(),
            "name": "Click Note to Self",
            "tool_name": "cdp_click",
            "arguments": { "target": "Note to Self" }
        }]
    }))
    .unwrap();

    let result = build_patch_from_output(&patcher_output, &workflow, &sample_tools(), false, false);

    assert!(
        result.warnings.is_empty(),
        "Expected no warnings, got: {:?}",
        result.warnings
    );
    assert_eq!(result.updated_nodes.len(), 1);

    let updated = &result.updated_nodes[0];
    assert_eq!(updated.name, "Click Note to Self");
    assert!(
        matches!(&updated.node_type, NodeType::CdpClick(p) if p.target.as_str() == "Note to Self"),
        "Expected CdpClick with target 'Note to Self', got {:?}",
        updated.node_type
    );
}

/// Verify the resolution prompt contains tool change guidance and the
/// available tool_name list.
#[test]
fn resolution_prompt_allows_tool_changes() {
    let wf = Workflow::default();
    let prompt = crate::planner::resolution::resolution_system_prompt(&wf);

    assert!(
        prompt.contains("tool_name"),
        "Resolution prompt must mention tool_name for node type changes"
    );
    assert!(
        prompt.contains("cdp_click"),
        "Resolution prompt must list cdp_click as an available tool"
    );
    assert!(
        prompt.contains("cdp_press_key"),
        "Resolution prompt must list cdp_press_key as an available tool"
    );
    assert!(
        !prompt.contains("MUST stay the same"),
        "Resolution prompt must NOT forbid node type changes"
    );
}

#[test]
fn resolution_prompt_requires_exact_labels() {
    let wf = Workflow::default();
    let prompt = crate::planner::resolution::resolution_system_prompt(&wf);

    assert!(
        prompt.contains("exact element label"),
        "Resolution prompt must require exact element labels for cdp_click targets"
    );
}
