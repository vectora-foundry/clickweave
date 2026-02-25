use super::helpers::*;
use crate::planner::*;
use clickweave_core::NodeType;

#[tokio::test]
async fn test_plan_focus_screenshot_click() {
    let response = r#"{"steps": [
        {"step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Safari"}},
        {"step_type": "Tool", "tool_name": "take_screenshot", "arguments": {"mode": "window", "app_name": "Safari", "include_ocr": true}},
        {"step_type": "Tool", "tool_name": "find_text", "arguments": {"text": "Login"}},
        {"step_type": "Tool", "tool_name": "click", "arguments": {"x": 100, "y": 200}}
    ]}"#;
    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Focus Safari and click the Login button",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.workflow.nodes.len(), 4);
    assert_eq!(result.workflow.edges.len(), 3);
    assert!(result.warnings.is_empty());
    assert!(matches!(
        result.workflow.nodes[0].node_type,
        NodeType::FocusWindow(_)
    ));
    assert!(matches!(
        result.workflow.nodes[1].node_type,
        NodeType::TakeScreenshot(_)
    ));
    assert!(matches!(
        result.workflow.nodes[2].node_type,
        NodeType::FindText(_)
    ));
    assert!(matches!(
        result.workflow.nodes[3].node_type,
        NodeType::Click(_)
    ));
    assert_eq!(mock.call_count(), 1);
}

#[tokio::test]
async fn test_plan_with_code_fence_wrapping() {
    let response = r#"```json
{"steps": [
    {"step_type": "Tool", "tool_name": "type_text", "arguments": {"text": "hello"}}
]}
```"#;
    let mock = MockBackend::single(response);
    let result =
        plan_workflow_with_backend(&mock, "Type hello", &sample_tools(), false, false, None)
            .await
            .unwrap();

    assert_eq!(result.workflow.nodes.len(), 1);
    assert!(matches!(
        result.workflow.nodes[0].node_type,
        NodeType::TypeText(_)
    ));
}

#[tokio::test]
async fn test_plan_agent_steps_filtered_when_disabled() {
    let response = r#"{"steps": [
        {"step_type": "Tool", "tool_name": "take_screenshot", "arguments": {}},
        {"step_type": "AiStep", "prompt": "Decide what to do"}
    ]}"#;
    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Take a screenshot and decide",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.workflow.nodes.len(), 1);
    assert!(!result.warnings.is_empty());
}

#[tokio::test]
async fn test_plan_agent_steps_kept_when_enabled() {
    let response = r#"{"steps": [
        {"step_type": "Tool", "tool_name": "take_screenshot", "arguments": {}},
        {"step_type": "AiStep", "prompt": "Decide what to do", "allowed_tools": ["click"]}
    ]}"#;
    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Take a screenshot and decide",
        &sample_tools(),
        false,
        true,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.workflow.nodes.len(), 2);
    assert!(matches!(
        result.workflow.nodes[1].node_type,
        NodeType::AiStep(_)
    ));
}

#[tokio::test]
async fn test_repair_pass_fixes_invalid_json() {
    let bad_response = r#"Here is the plan: {"steps": [invalid json}]}"#;
    let good_response = r#"{"steps": [{"step_type": "Tool", "tool_name": "click", "arguments": {"x": 50, "y": 50}}]}"#;
    let mock = MockBackend::new(vec![bad_response, good_response]);

    let result = plan_workflow_with_backend(
        &mock,
        "Click somewhere",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.workflow.nodes.len(), 1);
    // Should have called the backend twice (initial + repair)
    assert_eq!(mock.call_count(), 2);
}

#[tokio::test]
async fn test_repair_pass_fails_after_max_attempts() {
    let bad = r#"not json at all"#;
    let mock = MockBackend::new(vec![bad, bad]);

    let result = plan_workflow_with_backend(
        &mock,
        "Click somewhere",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await;

    assert!(result.is_err());
    assert_eq!(mock.call_count(), 2);
}

#[tokio::test]
async fn test_plan_empty_steps_returns_error() {
    let response = r#"{"steps": []}"#;
    let mock = MockBackend::single(response);
    let result =
        plan_workflow_with_backend(&mock, "Do nothing", &sample_tools(), false, false, None).await;

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("no steps"));
}

#[tokio::test]
async fn test_plan_calculator_loop_scenario() {
    // Simulates what the LLM should produce for:
    // "Open the calculator app and keep calculating 2x2 until you get to 1024"
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus Calculator"},
        {"id": "n2", "step_type": "Loop", "name": "Multiply Loop", "exit_condition": {
            "left": {"type": "Variable", "name": "check_for_1024.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "max_iterations": 20},
        {"id": "n3", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2"},
        {"id": "n4", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "×"}, "name": "Click Multiply"},
        {"id": "n5", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2 Again"},
        {"id": "n6", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "="}, "name": "Click Equals"},
        {"id": "n7", "step_type": "Tool", "tool_name": "find_text", "arguments": {"text": "1024", "app_name": "Calculator"}, "name": "Check for 1024"},
        {"id": "n8", "step_type": "EndLoop", "loop_id": "n2", "name": "End Multiply Loop"},
        {"id": "n9", "step_type": "Tool", "tool_name": "take_screenshot", "arguments": {"app_name": "Calculator"}, "name": "Final Screenshot"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3", "output": {"type": "LoopBody"}},
        {"from": "n2", "to": "n9", "output": {"type": "LoopDone"}},
        {"from": "n3", "to": "n4"},
        {"from": "n4", "to": "n5"},
        {"from": "n5", "to": "n6"},
        {"from": "n6", "to": "n7"},
        {"from": "n7", "to": "n8"},
        {"from": "n8", "to": "n2"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Open the calculator app and keep calculating 2x2 until you get to 1024",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    let wf = &result.workflow;
    assert_eq!(wf.nodes.len(), 9);
    assert_eq!(wf.edges.len(), 9);

    // Verify structure
    let loop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Loop(_)))
        .unwrap();
    let end_loop = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::EndLoop(_)))
        .unwrap();
    if let NodeType::EndLoop(p) = &end_loop.node_type {
        assert_eq!(
            p.loop_id, loop_node.id,
            "EndLoop must reference Loop's UUID"
        );
    }
    if let NodeType::Loop(p) = &loop_node.node_type {
        assert_eq!(p.max_iterations, 20);
    }

    // Verify LoopBody and LoopDone edges on Loop node
    let loop_edges: Vec<_> = wf.edges.iter().filter(|e| e.from == loop_node.id).collect();
    assert_eq!(loop_edges.len(), 2);
    assert!(
        loop_edges
            .iter()
            .any(|e| e.output == Some(clickweave_core::EdgeOutput::LoopBody))
    );
    assert!(
        loop_edges
            .iter()
            .any(|e| e.output == Some(clickweave_core::EdgeOutput::LoopDone))
    );

    assert!(result.warnings.is_empty());
}
