use super::helpers::*;
use crate::planner::*;
use clickweave_core::{ClickParams, Edge, EdgeOutput, NodeType};

// ── Control flow PlanStep parsing tests ─────────────────────────

#[test]
fn test_parse_loop_plan_step() {
    let json = r#"{
        "step_type": "Loop",
        "name": "Multiply Loop",
        "exit_condition": {
            "left": {"type": "Variable", "name": "check_result.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        },
        "max_iterations": 20
    }"#;
    let step: PlanStep = serde_json::from_str(json).unwrap();
    assert!(matches!(step, PlanStep::Loop { .. }));
}

#[test]
fn test_parse_end_loop_plan_step() {
    let json = r#"{"step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"}"#;
    let step: PlanStep = serde_json::from_str(json).unwrap();
    assert!(matches!(step, PlanStep::EndLoop { .. }));
}

#[test]
fn test_parse_if_plan_step() {
    let json = r#"{
        "step_type": "If",
        "name": "Check Found",
        "condition": {
            "left": {"type": "Variable", "name": "find_text.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }
    }"#;
    let step: PlanStep = serde_json::from_str(json).unwrap();
    assert!(matches!(step, PlanStep::If { .. }));
}

#[test]
fn test_parse_planner_graph_output() {
    let json = r#"{
        "nodes": [
            {"id": "n1", "step_type": "Tool", "tool_name": "launch_app", "arguments": {"app_name": "Calculator"}, "name": "Launch Calculator"},
            {"id": "n2", "step_type": "Loop", "name": "Multiply", "exit_condition": {
                "left": {"type": "Variable", "name": "check.found"},
                "operator": "Equals",
                "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
            }, "max_iterations": 20},
            {"id": "n3", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "="}, "name": "Click Equals"},
            {"id": "n4", "step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"}
        ],
        "edges": [
            {"from": "n1", "to": "n2"},
            {"from": "n2", "to": "n3", "output": {"type": "LoopBody"}},
            {"from": "n2", "to": "n1", "output": {"type": "LoopDone"}},
            {"from": "n3", "to": "n4"},
            {"from": "n4", "to": "n2"}
        ]
    }"#;
    let output: PlannerGraphOutput = serde_json::from_str(json).unwrap();
    assert_eq!(output.nodes.len(), 4);
    assert_eq!(output.edges.len(), 5);
    let node1: PlanNode = serde_json::from_value(output.nodes[1].clone()).unwrap();
    assert!(matches!(node1.step, PlanStep::Loop { .. }));
    let edge1: PlanEdge = serde_json::from_value(output.edges[1].clone()).unwrap();
    assert_eq!(edge1.output, Some(clickweave_core::EdgeOutput::LoopBody));
}

// ── Control flow semantics tests ────────────────────────────────

#[test]
fn test_control_flow_steps_never_rejected() {
    use crate::planner::parse::step_rejected_reason;

    let condition = bool_condition("x.found");

    let loop_step = PlanStep::Loop {
        name: None,
        exit_condition: condition.clone(),
        max_iterations: Some(10),
    };
    let end_loop_step = PlanStep::EndLoop {
        name: None,
        loop_id: "n1".into(),
    };
    let if_step = PlanStep::If {
        name: None,
        condition,
    };

    // Even with all features disabled, control flow steps pass through
    assert!(step_rejected_reason(&loop_step, false, false).is_none());
    assert!(step_rejected_reason(&end_loop_step, false, false).is_none());
    assert!(step_rejected_reason(&if_step, false, false).is_none());
}

// ── Flat plan loop tests ────────────────────────────────────────

#[test]
fn test_flat_plan_with_loop_gets_control_flow_edges() {
    // Flat plans (sequential steps) with Loop/EndLoop need
    // infer_control_flow_edges to add LoopBody/LoopDone labels
    // and EndLoop→Loop back-edges.
    let raw_steps: Vec<serde_json::Value> = serde_json::from_str(r#"[
        {"step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"step_type": "Loop", "name": "Repeat", "exit_condition": {
            "left": {"type": "Variable", "name": "click_2.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "max_iterations": 10},
        {"step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2"},
        {"step_type": "Tool", "tool_name": "click", "arguments": {"target": "="}, "name": "Click Equals"},
        {"step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"}
    ]"#)
    .unwrap();

    let patch = crate::planner::build_plan_as_patch(&raw_steps, &sample_tools(), false, false);
    assert!(
        patch.warnings.is_empty(),
        "Unexpected warnings: {:?}",
        patch.warnings
    );

    // Build a workflow from the patch
    let mut wf = clickweave_core::Workflow::new("test");
    wf.nodes = patch.added_nodes;
    wf.edges = patch.added_edges;

    let loop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Loop(_)))
        .unwrap();
    let endloop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::EndLoop(_)))
        .unwrap();

    // Loop should have LoopBody edge
    let loop_body = wf
        .edges
        .iter()
        .find(|e| e.from == loop_node.id && e.output == Some(EdgeOutput::LoopBody));
    assert!(loop_body.is_some(), "Loop should have a LoopBody edge");

    // LoopDone is optional for terminal loops (no steps after EndLoop)

    // EndLoop should have back-edge to Loop
    let back_edge = wf
        .edges
        .iter()
        .find(|e| e.from == endloop_node.id && e.to == loop_node.id);
    assert!(back_edge.is_some(), "EndLoop should have back-edge to Loop");

    // Workflow should pass validation
    clickweave_core::validate_workflow(&wf).expect("Flat plan with loop should pass validation");
}

#[test]
fn test_flat_plan_with_post_loop_node_gets_loop_done() {
    // Flat plan: [Focus, Loop, Click, EndLoop, Screenshot]
    // EndLoop→Screenshot should be converted to Loop→Screenshot (LoopDone).
    let raw_steps: Vec<serde_json::Value> = serde_json::from_str(r#"[
        {"step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"step_type": "Loop", "name": "Repeat", "exit_condition": {
            "left": {"type": "Variable", "name": "click_2.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "max_iterations": 10},
        {"step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2"},
        {"step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"},
        {"step_type": "Tool", "tool_name": "take_screenshot", "arguments": {}, "name": "Final Screenshot"}
    ]"#)
    .unwrap();

    let patch = crate::planner::build_plan_as_patch(&raw_steps, &sample_tools(), false, false);
    assert!(
        patch.warnings.is_empty(),
        "Unexpected warnings: {:?}",
        patch.warnings
    );

    let mut wf = clickweave_core::Workflow::new("test");
    wf.nodes = patch.added_nodes;
    wf.edges = patch.added_edges;

    let loop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Loop(_)))
        .unwrap();
    let screenshot_node = wf
        .nodes
        .iter()
        .find(|n| n.name == "Final Screenshot")
        .unwrap();

    // Loop should have LoopDone edge pointing to Screenshot
    let loop_done = wf
        .edges
        .iter()
        .find(|e| e.from == loop_node.id && e.output == Some(EdgeOutput::LoopDone));
    assert!(loop_done.is_some(), "Loop should have a LoopDone edge");
    assert_eq!(
        loop_done.unwrap().to,
        screenshot_node.id,
        "LoopDone should target the post-loop node"
    );

    clickweave_core::validate_workflow(&wf)
        .expect("Flat plan with post-loop node should pass validation");
}

// ── Edge inference tests ────────────────────────────────────────

#[tokio::test]
async fn test_infer_loop_edges_from_unlabeled() {
    // LLM produces unlabeled Loop edges — inference should label them.
    // Pattern: Loop→body (unlabeled), Loop→EndLoop (unlabeled)
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n2", "step_type": "Loop", "name": "My Loop", "exit_condition": {
            "left": {"type": "Variable", "name": "click_equals.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "max_iterations": 10},
        {"id": "n3", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "="}, "name": "Click Equals"},
        {"id": "n4", "step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"},
        {"id": "n5", "step_type": "Tool", "tool_name": "take_screenshot", "arguments": {}, "name": "Done Screenshot"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"},
        {"from": "n2", "to": "n5"},
        {"from": "n3", "to": "n4"},
        {"from": "n4", "to": "n2"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result =
        plan_workflow_with_backend(&mock, "Loop test", &sample_tools(), false, false, None)
            .await
            .unwrap();

    let wf = &result.workflow;
    let loop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Loop(_)))
        .unwrap();

    let loop_edges: Vec<_> = wf.edges.iter().filter(|e| e.from == loop_node.id).collect();
    assert_eq!(loop_edges.len(), 2);
    assert!(
        loop_edges
            .iter()
            .any(|e| e.output == Some(EdgeOutput::LoopBody)),
        "Should infer LoopBody edge"
    );
    assert!(
        loop_edges
            .iter()
            .any(|e| e.output == Some(EdgeOutput::LoopDone)),
        "Should infer LoopDone edge"
    );
}

#[tokio::test]
async fn test_infer_loop_reroutes_back_edge_through_endloop() {
    // LLM produces: body_end→Loop (bypassing EndLoop), Loop→EndLoop (as exit).
    // This is the exact pattern from the calculator bug.
    // Expected fix: body_end→EndLoop, EndLoop→Loop, LoopDone removed (terminal).
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n2", "step_type": "Loop", "name": "Multiply", "exit_condition": {
            "left": {"type": "Variable", "name": "check_result.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "max_iterations": 20},
        {"id": "n3", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2"},
        {"id": "n4", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "="}, "name": "Click Equals"},
        {"id": "n5", "step_type": "Tool", "tool_name": "find_text", "arguments": {"text": "1024"}, "name": "Check Result"},
        {"id": "n6", "step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"},
        {"from": "n3", "to": "n4"},
        {"from": "n4", "to": "n5"},
        {"from": "n5", "to": "n2"},
        {"from": "n2", "to": "n6"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Calculator multiply loop",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    let wf = &result.workflow;
    let loop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Loop(_)))
        .unwrap();
    let endloop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::EndLoop(_)))
        .unwrap();

    // Loop should have LoopBody edge (to Click 2) but no LoopDone (removed: targeted EndLoop)
    let loop_edges: Vec<_> = wf.edges.iter().filter(|e| e.from == loop_node.id).collect();
    assert_eq!(loop_edges.len(), 1, "Terminal loop: only LoopBody edge");
    assert_eq!(
        loop_edges[0].output,
        Some(EdgeOutput::LoopBody),
        "The single Loop edge should be LoopBody"
    );

    // EndLoop should have back-edge to Loop
    let endloop_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == endloop_node.id)
        .collect();
    assert_eq!(
        endloop_edges.len(),
        1,
        "EndLoop should have one outgoing edge"
    );
    assert_eq!(
        endloop_edges[0].to, loop_node.id,
        "EndLoop should point back to Loop"
    );

    // Check Result (n5) should connect to EndLoop, not directly to Loop
    let check_node = wf.nodes.iter().find(|n| n.name == "Check Result").unwrap();
    let check_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == check_node.id)
        .collect();
    assert_eq!(check_edges.len(), 1);
    assert_eq!(
        check_edges[0].to, endloop_node.id,
        "Back-edge should be rerouted through EndLoop"
    );
    assert_eq!(
        check_edges[0].output, None,
        "Rerouted edge should be a regular edge (no output label)"
    );
}

#[tokio::test]
async fn test_infer_loop_clears_stale_output_on_rerouted_back_edge() {
    // LLM produces: body_end→Loop with LoopBody label (stale), plus EndLoop node.
    // This is the exact pattern from the 2x2=128 calculator bug where the LLM
    // labeled the back-edge as LoopBody instead of routing through EndLoop.
    // Without clearing the output, follow_single_edge can't find the edge.
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "launch_app", "arguments": {"app_name": "Calculator"}, "name": "Launch"},
        {"id": "n2", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n3", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2 Setup"},
        {"id": "n4", "step_type": "Loop", "name": "Multiply Loop", "exit_condition": {
            "left": {"type": "Variable", "name": "click_equals.result"},
            "operator": "GreaterThan",
            "right": {"type": "Literal", "value": {"type": "Number", "value": 128}}
        }, "max_iterations": 10},
        {"id": "n5", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "×"}, "name": "Click Multiply"},
        {"id": "n6", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2"},
        {"id": "n7", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "="}, "name": "Click Equals"},
        {"id": "n8", "step_type": "EndLoop", "loop_id": "n4", "name": "End Loop"},
        {"id": "n9", "step_type": "Tool", "tool_name": "take_screenshot", "arguments": {}, "name": "Screenshot"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"},
        {"from": "n3", "to": "n4"},
        {"from": "n4", "to": "n5", "output": {"type": "LoopBody"}},
        {"from": "n5", "to": "n6"},
        {"from": "n6", "to": "n7"},
        {"from": "n7", "to": "n4", "output": {"type": "LoopBody"}},
        {"from": "n4", "to": "n9", "output": {"type": "LoopDone"}}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Calculator 2x2 loop",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    let wf = &result.workflow;
    let endloop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::EndLoop(_)))
        .unwrap();

    // Click Equals should connect to EndLoop with a regular (unlabeled) edge
    let equals_node = wf.nodes.iter().find(|n| n.name == "Click Equals").unwrap();
    let equals_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == equals_node.id)
        .collect();
    assert_eq!(equals_edges.len(), 1, "Click Equals should have one edge");
    assert_eq!(
        equals_edges[0].to, endloop_node.id,
        "Click Equals should connect to EndLoop"
    );
    assert_eq!(
        equals_edges[0].output, None,
        "Rerouted edge must have output cleared (was LoopBody)"
    );
}

#[tokio::test]
async fn test_infer_if_edges_from_unlabeled() {
    // LLM produces If node with two unlabeled edges — first should be IfTrue, second IfFalse.
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "If", "name": "Check Found", "condition": {
            "left": {"type": "Variable", "name": "click_ok.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "OK"}, "name": "Click OK"},
        {"id": "n3", "step_type": "Tool", "tool_name": "take_screenshot", "arguments": {}, "name": "Screenshot"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n1", "to": "n3"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(&mock, "If check", &sample_tools(), false, false, None)
        .await
        .unwrap();

    let wf = &result.workflow;
    let if_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::If(_)))
        .unwrap();

    let if_edges: Vec<_> = wf.edges.iter().filter(|e| e.from == if_node.id).collect();
    assert_eq!(if_edges.len(), 2);
    assert!(
        if_edges
            .iter()
            .any(|e| e.output == Some(EdgeOutput::IfTrue)),
        "Should infer IfTrue edge"
    );
    assert!(
        if_edges
            .iter()
            .any(|e| e.output == Some(EdgeOutput::IfFalse)),
        "Should infer IfFalse edge"
    );
}

#[test]
fn test_infer_noop_for_already_labeled_edges() {
    // Edges that already have labels should not be modified.
    use crate::planner::infer_control_flow_edges;
    use clickweave_core::{EndLoopParams, LoopParams, Node, NodeType, Position};

    let pos = |y| Position { x: 0.0, y };
    let loop_node = Node::new(
        NodeType::Loop(LoopParams {
            exit_condition: bool_condition("x"),
            max_iterations: 10,
        }),
        pos(0.0),
        "Loop",
    );
    let body_node = Node::new(NodeType::Click(ClickParams::default()), pos(100.0), "Body");
    let endloop_node = Node::new(
        NodeType::EndLoop(EndLoopParams {
            loop_id: loop_node.id,
        }),
        pos(200.0),
        "EndLoop",
    );
    let done_node = Node::new(NodeType::Click(ClickParams::default()), pos(300.0), "Done");

    let mut edges = vec![
        Edge {
            from: loop_node.id,
            to: body_node.id,
            output: Some(EdgeOutput::LoopBody),
        },
        Edge {
            from: loop_node.id,
            to: done_node.id,
            output: Some(EdgeOutput::LoopDone),
        },
        Edge {
            from: body_node.id,
            to: endloop_node.id,
            output: None,
        },
        Edge {
            from: endloop_node.id,
            to: loop_node.id,
            output: None,
        },
    ];
    let nodes = vec![loop_node, body_node, endloop_node, done_node];
    let mut warnings = Vec::new();

    infer_control_flow_edges(&nodes, &mut edges, &mut warnings);

    assert_eq!(edges.len(), 4);
    assert_eq!(edges[0].output, Some(EdgeOutput::LoopBody));
    assert_eq!(edges[1].output, Some(EdgeOutput::LoopDone));
    assert!(warnings.is_empty());
}

#[tokio::test]
async fn test_infer_loop_reroutes_body_back_edge_when_endloop_edge_already_exists() {
    // LLM produces both EndLoop→Loop AND body→Loop edges. Previously the
    // `continue` in Phase 2 skipped rerouting when the back-edge existed,
    // leaving a body→Loop cycle that validation rejected.
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n2", "step_type": "Loop", "name": "Multiply", "exit_condition": {
            "left": {"type": "Variable", "name": "check_result.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "max_iterations": 20},
        {"id": "n3", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2"},
        {"id": "n4", "step_type": "Tool", "tool_name": "find_text", "arguments": {"text": "1024"}, "name": "Check Result"},
        {"id": "n5", "step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"},
        {"from": "n3", "to": "n4"},
        {"from": "n4", "to": "n2"},
        {"from": "n5", "to": "n2"},
        {"from": "n2", "to": "n5"}
    ]}"#;
    // Both n4→n2 (body→Loop) and n5→n2 (EndLoop→Loop) are present.

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Calculator multiply with both back-edges",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    let wf = &result.workflow;
    let loop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Loop(_)))
        .unwrap();
    let endloop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::EndLoop(_)))
        .unwrap();

    // n4 (Check Result) should be rerouted to EndLoop, not Loop
    let check_node = wf.nodes.iter().find(|n| n.name == "Check Result").unwrap();
    let check_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == check_node.id)
        .collect();
    assert_eq!(check_edges.len(), 1);
    assert_eq!(
        check_edges[0].to, endloop_node.id,
        "body→Loop should be rerouted through EndLoop even when EndLoop→Loop already exists"
    );

    // EndLoop should have exactly one back-edge (no duplicates)
    let endloop_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == endloop_node.id && e.to == loop_node.id)
        .collect();
    assert_eq!(
        endloop_edges.len(),
        1,
        "Should not duplicate EndLoop→Loop back-edge"
    );

    // Workflow should pass validation (no cycle error)
    clickweave_core::validate_workflow(wf).expect("Workflow with both back-edges should validate");
}

#[tokio::test]
async fn test_infer_loop_reroutes_if_false_back_edge_to_loop() {
    // LLM produces If(IfFalse)→Loop edge inside a loop body. This should be
    // rerouted through EndLoop, otherwise cycle detection rejects it.
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus Calculator"},
        {"id": "n2", "step_type": "Loop", "name": "Repeat Multiply", "exit_condition": {
            "left": {"type": "Variable", "name": "check_result.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "max_iterations": 10},
        {"id": "n3", "step_type": "Tool", "tool_name": "find_text", "arguments": {"text": "1024"}, "name": "Check Result"},
        {"id": "n4", "step_type": "If", "condition": {
            "left": {"type": "Variable", "name": "check_result.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "name": "Is 1024?"},
        {"id": "n5", "step_type": "Tool", "tool_name": "press_key", "arguments": {"key": "escape"}, "name": "Done"},
        {"id": "n6", "step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"},
        {"from": "n3", "to": "n4"},
        {"from": "n4", "to": "n5", "output": {"type": "IfTrue"}},
        {"from": "n4", "to": "n2", "output": {"type": "IfFalse"}},
        {"from": "n5", "to": "n6"},
        {"from": "n6", "to": "n2"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Calculator multiply with If→Loop back-edge",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    let wf = &result.workflow;
    let endloop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::EndLoop(_)))
        .unwrap();
    let if_node = wf.nodes.iter().find(|n| n.name == "Is 1024?").unwrap();

    // The IfFalse edge from n4 should now target EndLoop, not Loop
    let if_false_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == if_node.id && e.output == Some(EdgeOutput::IfFalse))
        .collect();
    assert_eq!(if_false_edges.len(), 1);
    assert_eq!(
        if_false_edges[0].to, endloop_node.id,
        "IfFalse→Loop should be rerouted through EndLoop"
    );

    // Workflow should pass validation (no cycle error)
    clickweave_core::validate_workflow(wf)
        .expect("Workflow with IfFalse rerouted through EndLoop should validate");
}

#[tokio::test]
async fn test_infer_loop_removes_stray_endloop_forward_edge_when_loop_done_exists() {
    // LLM emits LoopDone on the Loop node AND a forward edge from EndLoop to a
    // post-loop node. The forward edge is stray — EndLoop must only point back to
    // its paired Loop. Phase 2 should remove it.
    //
    // This matches the exact pattern seen with Qwen 30B:
    //   n4→n5 (LoopBody), n5→n6, n6→n7, n7→n4 (LoopBody),  // body→Loop
    //   n4→n8 (LoopDone),                                     // exit already present
    //   n10→n9                                                 // EndLoop→post-loop (WRONG)
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "launch_app", "arguments": {"app_name": "Calculator"}, "name": "Launch Calculator"},
        {"id": "n2", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus Calculator"},
        {"id": "n3", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2 Start"},
        {"id": "n4", "step_type": "Loop", "name": "Loop Until > 128", "exit_condition": {
            "left": {"type": "Variable", "name": "click_equals.result"},
            "operator": "GreaterThan",
            "right": {"type": "Literal", "value": {"type": "Number", "value": 128}}
        }, "max_iterations": 10},
        {"id": "n5", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "×"}, "name": "Click Multiply"},
        {"id": "n6", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2"},
        {"id": "n7", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "="}, "name": "Click Equals"},
        {"id": "n8", "step_type": "Tool", "tool_name": "take_screenshot", "arguments": {"app_name": "Calculator"}, "name": "Final Screenshot"},
        {"id": "n9", "step_type": "Tool", "tool_name": "find_text", "arguments": {"text": "128"}, "name": "Verify Result"},
        {"id": "n10", "step_type": "EndLoop", "loop_id": "n4", "name": "End Loop"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"},
        {"from": "n3", "to": "n4"},
        {"from": "n4", "to": "n5", "output": {"type": "LoopBody"}},
        {"from": "n5", "to": "n6"},
        {"from": "n6", "to": "n7"},
        {"from": "n7", "to": "n4", "output": {"type": "LoopBody"}},
        {"from": "n4", "to": "n8", "output": {"type": "LoopDone"}},
        {"from": "n8", "to": "n9"},
        {"from": "n10", "to": "n9"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Calculator multiply loop with stray EndLoop edge",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    let wf = &result.workflow;
    let loop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Loop(_)))
        .unwrap();
    let endloop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::EndLoop(_)))
        .unwrap();

    // EndLoop should have exactly one outgoing edge pointing to Loop
    let endloop_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == endloop_node.id)
        .collect();
    assert_eq!(
        endloop_edges.len(),
        1,
        "EndLoop should have exactly 1 outgoing edge"
    );
    assert_eq!(
        endloop_edges[0].to, loop_node.id,
        "EndLoop's only edge must point back to its paired Loop"
    );

    // Last body step (Click Equals) should be rerouted to EndLoop
    let equals_node = wf.nodes.iter().find(|n| n.name == "Click Equals").unwrap();
    let equals_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == equals_node.id)
        .collect();
    assert_eq!(equals_edges.len(), 1);
    assert_eq!(
        equals_edges[0].to, endloop_node.id,
        "Last body step should route to EndLoop, not directly to Loop"
    );

    // LoopDone should still exist on the Loop node
    let loop_done = wf
        .edges
        .iter()
        .find(|e| e.from == loop_node.id && e.output == Some(EdgeOutput::LoopDone));
    assert!(loop_done.is_some(), "LoopDone edge should be preserved");

    // Workflow should pass validation
    clickweave_core::validate_workflow(wf)
        .expect("Workflow with stray EndLoop forward edge removed should validate");
}

// ── Malformed input resilience tests ────────────────────────────

#[test]
fn test_unknown_step_type_deserializes() {
    // Verify that an unknown step_type like "End" deserializes as PlanStep::Unknown
    // via #[serde(other)], allowing the rest of the graph to parse.
    let json = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "launch_app", "arguments": {"app_name": "Calculator"}, "name": "Launch"},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "5"}, "name": "Click 5"},
        {"id": "n3", "step_type": "End"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"}
    ]}"#;

    let parsed: Result<PlannerGraphOutput, _> = serde_json::from_str(json);
    assert!(
        parsed.is_ok(),
        "PlannerGraphOutput should parse with unknown step type: {:?}",
        parsed.err()
    );
    let graph = parsed.unwrap();
    assert_eq!(graph.nodes.len(), 3);
    // The third node ("End") parses as PlanStep::Unknown via #[serde(other)]
    let node2: PlanNode = serde_json::from_value(graph.nodes[2].clone()).unwrap();
    assert!(matches!(node2.step, PlanStep::Unknown));
}

#[tokio::test]
async fn test_unknown_step_type_skipped_not_fatal() {
    // LLM invents "End" step type — should be skipped, not crash the parse.
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "5"}, "name": "Click 5"},
        {"id": "n3", "step_type": "End"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Calculator click with End node",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    // n3 (End) should be skipped; workflow should have 2 valid nodes
    assert_eq!(result.workflow.nodes.len(), 2);
    assert!(result.warnings.iter().any(|w| w.contains("skipped")));
}

#[tokio::test]
async fn test_malformed_node_missing_fields_skipped() {
    // EndLoop without loop_id — should be skipped during node parse, not crash the graph.
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "5"}, "name": "Click 5"},
        {"id": "n3", "step_type": "EndLoop"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "EndLoop without loop_id",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    // n3 (EndLoop without loop_id) should be skipped; workflow has 2 valid nodes
    assert_eq!(result.workflow.nodes.len(), 2);
    assert!(
        result.warnings.iter().any(|w| w.contains("malformed")),
        "Expected malformed warning, got: {:?}",
        result.warnings
    );
}

#[tokio::test]
async fn test_malformed_edge_unknown_output_type_skipped() {
    // LLM invents "LoopCondition" edge output type — should be skipped, not crash.
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "5"}, "name": "Click 5"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n1", "output": {"type": "LoopCondition"}}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Calculator click with malformed edge",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    // Workflow should have 2 nodes; the malformed edge is skipped
    assert_eq!(result.workflow.nodes.len(), 2);
    assert_eq!(result.workflow.edges.len(), 1);
    assert!(
        result.warnings.iter().any(|w| w.contains("malformed")),
        "Expected malformed edge warning, got: {:?}",
        result.warnings
    );
}

#[tokio::test]
async fn test_malformed_flat_step_skipped() {
    // One valid step and one missing required fields — malformed step skipped.
    let response = r#"{"steps": [
        {"step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}},
        {"step_type": "Tool"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Mixed valid and malformed steps",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.workflow.nodes.len(), 1);
    assert!(
        result.warnings.iter().any(|w| w.contains("malformed")),
        "Expected malformed step warning, got: {:?}",
        result.warnings
    );
}
