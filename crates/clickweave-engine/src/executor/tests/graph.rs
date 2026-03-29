use super::helpers::*;
use clickweave_core::output_schema::{ConditionValue, OutputRef};
use clickweave_core::{
    CdpClickParams, CdpFillParams, CdpTypeParams, ClickParams, Condition, EdgeOutput,
    EndLoopParams, IfParams, LiteralValue, LoopParams, NodeType, Operator, Position,
    TypeTextParams, Workflow,
};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Graph walker tests
// ---------------------------------------------------------------------------

#[test]
fn test_entry_points_linear() {
    let mut workflow = Workflow::new("test");
    let a = workflow.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 0.0, y: 0.0 },
    );
    let b = workflow.add_node(
        NodeType::TypeText(TypeTextParams::default()),
        Position { x: 100.0, y: 0.0 },
    );
    workflow.add_edge(a, b);

    let exec = make_executor_with_workflow(workflow);
    let entries = exec.entry_points();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], a);
}

#[test]
fn test_entry_points_ignores_endloop_backedge() {
    let mut workflow = Workflow::new("test-loop");

    // Loop → (body) → Click → EndLoop → (back to Loop)
    let loop_id = workflow.add_node(
        NodeType::Loop(LoopParams {
            exit_condition: dummy_condition(),
            max_iterations: 10,
        }),
        Position { x: 0.0, y: 0.0 },
    );
    let click_id = workflow.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 100.0, y: 0.0 },
    );
    let endloop_id = workflow.add_node(
        NodeType::EndLoop(EndLoopParams { loop_id }),
        Position { x: 200.0, y: 0.0 },
    );

    // Loop --LoopBody--> Click
    workflow.add_edge_with_output(loop_id, click_id, EdgeOutput::LoopBody);
    // Click --> EndLoop
    workflow.add_edge(click_id, endloop_id);
    // EndLoop --> Loop (back-edge)
    workflow.add_edge(endloop_id, loop_id);

    let exec = make_executor_with_workflow(workflow);
    let entries = exec.entry_points();

    // The Loop node should be the only entry point.
    // The EndLoop back-edge to Loop should NOT disqualify Loop as an entry point.
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], loop_id);
}

#[test]
fn test_follow_single_edge() {
    let mut workflow = Workflow::new("test");
    let a = workflow.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 0.0, y: 0.0 },
    );
    let b = workflow.add_node(
        NodeType::TypeText(TypeTextParams::default()),
        Position { x: 100.0, y: 0.0 },
    );
    workflow.add_edge(a, b);

    let exec = make_executor_with_workflow(workflow);
    assert_eq!(exec.follow_single_edge(a), Some(b));
}

#[test]
fn test_follow_single_edge_no_edge() {
    let mut workflow = Workflow::new("test");
    let a = workflow.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 0.0, y: 0.0 },
    );

    let exec = make_executor_with_workflow(workflow);
    assert_eq!(exec.follow_single_edge(a), None);
}

#[test]
fn test_follow_edge_if_true() {
    let mut workflow = Workflow::new("test-if");
    let if_id = workflow.add_node(
        NodeType::If(IfParams {
            condition: dummy_condition(),
        }),
        Position { x: 0.0, y: 0.0 },
    );
    let true_id = workflow.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 100.0, y: -50.0 },
    );
    let false_id = workflow.add_node(
        NodeType::TypeText(TypeTextParams::default()),
        Position { x: 100.0, y: 50.0 },
    );

    workflow.add_edge_with_output(if_id, true_id, EdgeOutput::IfTrue);
    workflow.add_edge_with_output(if_id, false_id, EdgeOutput::IfFalse);

    let exec = make_executor_with_workflow(workflow);

    assert_eq!(exec.follow_edge(if_id, &EdgeOutput::IfTrue), Some(true_id));
    assert_eq!(
        exec.follow_edge(if_id, &EdgeOutput::IfFalse),
        Some(false_id)
    );
    // No LoopBody edge from an If node
    assert_eq!(exec.follow_edge(if_id, &EdgeOutput::LoopBody), None);
}

#[test]
fn test_follow_edge_loop_body_and_done() {
    let mut workflow = Workflow::new("test-loop-edges");
    let loop_id = workflow.add_node(
        NodeType::Loop(LoopParams {
            exit_condition: dummy_condition(),
            max_iterations: 10,
        }),
        Position { x: 0.0, y: 0.0 },
    );
    let body_id = workflow.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 100.0, y: -50.0 },
    );
    let done_id = workflow.add_node(
        NodeType::TypeText(TypeTextParams::default()),
        Position { x: 100.0, y: 50.0 },
    );

    workflow.add_edge_with_output(loop_id, body_id, EdgeOutput::LoopBody);
    workflow.add_edge_with_output(loop_id, done_id, EdgeOutput::LoopDone);

    let exec = make_executor_with_workflow(workflow);

    assert_eq!(
        exec.follow_edge(loop_id, &EdgeOutput::LoopBody),
        Some(body_id)
    );
    assert_eq!(
        exec.follow_edge(loop_id, &EdgeOutput::LoopDone),
        Some(done_id)
    );
    // No IfTrue edge from a Loop node
    assert_eq!(exec.follow_edge(loop_id, &EdgeOutput::IfTrue), None);
}

#[test]
fn test_runtime_context_variables_through_executor() {
    let workflow = Workflow::new("test-ctx");
    let mut exec = make_executor_with_workflow(workflow);

    // Set variables through the executor's context
    exec.context
        .set_variable("find_text.success", Value::Bool(true));
    exec.context
        .set_variable("find_text.text", Value::String("hello world".into()));

    // Verify get_variable
    assert_eq!(
        exec.context.get_variable("find_text.success"),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        exec.context.get_variable("find_text.text"),
        Some(&Value::String("hello world".into()))
    );
    assert_eq!(exec.context.get_variable("nonexistent"), None);

    // Verify condition evaluation through the context
    let cond = Condition {
        left: OutputRef {
            node: "find_text".to_string(),
            field: "success".to_string(),
        },
        operator: Operator::Equals,
        right: ConditionValue::Literal {
            value: LiteralValue::Bool { value: true },
        },
    };
    assert!(exec.context.evaluate_condition(&cond));

    // Test with a Contains condition
    let contains_cond = Condition {
        left: OutputRef {
            node: "find_text".to_string(),
            field: "text".to_string(),
        },
        operator: Operator::Contains,
        right: ConditionValue::Literal {
            value: LiteralValue::String {
                value: "hello".to_string(),
            },
        },
    };
    assert!(exec.context.evaluate_condition(&contains_cond));

    // Test loop counter through context
    let loop_id = uuid::Uuid::new_v4();
    exec.context.loop_counters.insert(loop_id, 3);
    assert_eq!(exec.context.loop_counters[&loop_id], 3);
}

// ---------------------------------------------------------------------------
// find_predecessor tests
// ---------------------------------------------------------------------------

#[test]
fn test_find_predecessor_linear() {
    let mut workflow = Workflow::new("test");
    let click = workflow.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 0.0, y: 0.0 },
    );
    let type_text = workflow.add_node(
        NodeType::TypeText(TypeTextParams::default()),
        Position { x: 100.0, y: 0.0 },
    );
    workflow.add_edge(click, type_text);

    let exec = make_executor_with_workflow(workflow);
    assert_eq!(exec.find_predecessor(type_text), Some(click));
}

#[test]
fn test_find_predecessor_entry_point_has_none() {
    let mut workflow = Workflow::new("test");
    let a = workflow.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 0.0, y: 0.0 },
    );
    let b = workflow.add_node(
        NodeType::TypeText(TypeTextParams::default()),
        Position { x: 100.0, y: 0.0 },
    );
    workflow.add_edge(a, b);

    let exec = make_executor_with_workflow(workflow);
    assert_eq!(exec.find_predecessor(a), None);
}

#[test]
fn test_find_predecessor_ignores_labeled_edges() {
    let mut workflow = Workflow::new("test");
    let if_node = workflow.add_node(
        NodeType::If(IfParams {
            condition: dummy_condition(),
        }),
        Position { x: 0.0, y: 0.0 },
    );
    let type_text = workflow.add_node(
        NodeType::TypeText(TypeTextParams::default()),
        Position { x: 100.0, y: 0.0 },
    );
    workflow.add_edge_with_output(if_node, type_text, EdgeOutput::IfTrue);

    let exec = make_executor_with_workflow(workflow);
    // Labeled edge — find_predecessor returns None
    assert_eq!(exec.find_predecessor(type_text), None);
}

// ---------------------------------------------------------------------------
// NodeType classification tests
// ---------------------------------------------------------------------------

#[test]
fn test_is_text_input() {
    assert!(NodeType::TypeText(TypeTextParams::default()).is_text_input());
    assert!(NodeType::CdpFill(CdpFillParams::default()).is_text_input());
    assert!(NodeType::CdpType(CdpTypeParams::default()).is_text_input());

    assert!(!NodeType::Click(ClickParams::default()).is_text_input());
    assert!(!NodeType::CdpClick(CdpClickParams::default()).is_text_input());
}

#[test]
fn test_is_focus_establishing() {
    assert!(NodeType::Click(ClickParams::default()).is_focus_establishing());
    assert!(NodeType::CdpClick(CdpClickParams::default()).is_focus_establishing());

    assert!(!NodeType::TypeText(TypeTextParams::default()).is_focus_establishing());
    assert!(!NodeType::CdpFill(CdpFillParams::default()).is_focus_establishing());
}
