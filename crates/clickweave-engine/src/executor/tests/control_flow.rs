use super::helpers::*;
use crate::executor::retry_context::RetryContext;
use clickweave_core::output_schema::{ConditionValue, OutputRef};
use clickweave_core::{
    ClickParams, Condition, EdgeOutput, EndLoopParams, IfParams, LiteralValue, LoopParams,
    NodeType, Operator, Position, SwitchCase, SwitchParams, TypeTextParams, Workflow,
};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a condition that compares `<node>.<field>` to a literal bool.
fn bool_condition(node: &str, field: &str, expected: bool) -> Condition {
    Condition {
        left: OutputRef {
            node: node.to_string(),
            field: field.to_string(),
        },
        operator: Operator::Equals,
        right: ConditionValue::Literal {
            value: LiteralValue::Bool { value: expected },
        },
    }
}

/// Build a condition that compares `<node>.<field>` to a literal string.
fn string_condition(node: &str, field: &str, op: Operator, expected: &str) -> Condition {
    Condition {
        left: OutputRef {
            node: node.to_string(),
            field: field.to_string(),
        },
        operator: op,
        right: ConditionValue::Literal {
            value: LiteralValue::String {
                value: expected.to_string(),
            },
        },
    }
}

/// Extract (name, node_type) from a workflow node, cloning so the borrow is released
/// before we call `eval_control_flow` which takes `&mut self`.
fn extract_node_info(wf: &Workflow, node_id: uuid::Uuid) -> (String, NodeType) {
    let node = wf.find_node(node_id).unwrap();
    (node.name.clone(), node.node_type.clone())
}

/// Create a minimal If workflow:
///   If --IfTrue--> true_node
///   If --IfFalse--> false_node
fn make_if_workflow(condition: Condition) -> (Workflow, uuid::Uuid, uuid::Uuid, uuid::Uuid) {
    let mut wf = Workflow::new("test-if");
    let if_id = wf.add_node(
        NodeType::If(IfParams { condition }),
        Position { x: 0.0, y: 0.0 },
    );
    let true_id = wf.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 100.0, y: -50.0 },
    );
    let false_id = wf.add_node(
        NodeType::TypeText(TypeTextParams::default()),
        Position { x: 100.0, y: 50.0 },
    );
    wf.add_edge_with_output(if_id, true_id, EdgeOutput::IfTrue);
    wf.add_edge_with_output(if_id, false_id, EdgeOutput::IfFalse);
    (wf, if_id, true_id, false_id)
}

/// Create a minimal Switch workflow:
///   Switch --SwitchCase("case_a")--> case_a_node
///   Switch --SwitchCase("case_b")--> case_b_node
///   Switch --SwitchDefault--> default_node
fn make_switch_workflow(
    cases: Vec<SwitchCase>,
) -> (Workflow, uuid::Uuid, uuid::Uuid, uuid::Uuid, uuid::Uuid) {
    let mut wf = Workflow::new("test-switch");
    let switch_id = wf.add_node(
        NodeType::Switch(SwitchParams { cases }),
        Position { x: 0.0, y: 0.0 },
    );
    let case_a_id = wf.add_node(
        NodeType::Click(ClickParams::default()),
        Position {
            x: 100.0,
            y: -100.0,
        },
    );
    let case_b_id = wf.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 100.0, y: 0.0 },
    );
    let default_id = wf.add_node(
        NodeType::TypeText(TypeTextParams::default()),
        Position { x: 100.0, y: 100.0 },
    );
    wf.add_edge_with_output(
        switch_id,
        case_a_id,
        EdgeOutput::SwitchCase {
            name: "case_a".to_string(),
        },
    );
    wf.add_edge_with_output(
        switch_id,
        case_b_id,
        EdgeOutput::SwitchCase {
            name: "case_b".to_string(),
        },
    );
    wf.add_edge_with_output(switch_id, default_id, EdgeOutput::SwitchDefault);
    (wf, switch_id, case_a_id, case_b_id, default_id)
}

/// Create a minimal Loop workflow:
///   Loop --LoopBody--> body_node
///   Loop --LoopDone--> done_node
fn make_loop_workflow(
    exit_condition: Condition,
    max_iterations: u32,
) -> (Workflow, uuid::Uuid, uuid::Uuid, uuid::Uuid) {
    let mut wf = Workflow::new("test-loop");
    let loop_id = wf.add_node(
        NodeType::Loop(LoopParams {
            exit_condition,
            max_iterations,
        }),
        Position { x: 0.0, y: 0.0 },
    );
    let body_id = wf.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 100.0, y: -50.0 },
    );
    let done_id = wf.add_node(
        NodeType::TypeText(TypeTextParams::default()),
        Position { x: 100.0, y: 50.0 },
    );
    wf.add_edge_with_output(loop_id, body_id, EdgeOutput::LoopBody);
    wf.add_edge_with_output(loop_id, done_id, EdgeOutput::LoopDone);
    (wf, loop_id, body_id, done_id)
}

// ---------------------------------------------------------------------------
// If node tests
// ---------------------------------------------------------------------------

#[test]
fn if_true_branch_followed_when_condition_is_true() {
    let condition = bool_condition("check", "found", true);
    let (wf, if_id, true_id, _false_id) = make_if_workflow(condition);

    let mut exec = make_executor_with_workflow(wf);
    exec.context.set_variable("check.found", Value::Bool(true));

    let (name, node_type) = extract_node_info(&exec.workflow, if_id);
    let next = exec.eval_control_flow(if_id, &name, &node_type, &mut RetryContext::new());

    assert_eq!(next, Some(true_id));
}

#[test]
fn if_false_branch_followed_when_condition_is_false() {
    let condition = bool_condition("check", "found", true);
    let (wf, if_id, _true_id, false_id) = make_if_workflow(condition);

    let mut exec = make_executor_with_workflow(wf);
    exec.context.set_variable("check.found", Value::Bool(false));

    let (name, node_type) = extract_node_info(&exec.workflow, if_id);
    let next = exec.eval_control_flow(if_id, &name, &node_type, &mut RetryContext::new());

    assert_eq!(next, Some(false_id));
}

#[test]
fn if_false_when_variable_missing() {
    // Missing variable resolves to null, which != true.
    let condition = bool_condition("missing", "var", true);
    let (wf, if_id, _true_id, false_id) = make_if_workflow(condition);

    let mut exec = make_executor_with_workflow(wf);
    // Deliberately not setting the variable.

    let (name, node_type) = extract_node_info(&exec.workflow, if_id);
    let next = exec.eval_control_flow(if_id, &name, &node_type, &mut RetryContext::new());

    assert_eq!(next, Some(false_id));
}

// ---------------------------------------------------------------------------
// Switch node tests
// ---------------------------------------------------------------------------

#[test]
fn switch_follows_first_matching_case() {
    let cases = vec![
        SwitchCase {
            name: "case_a".to_string(),
            condition: string_condition("status", "value", Operator::Equals, "error"),
        },
        SwitchCase {
            name: "case_b".to_string(),
            condition: string_condition("status", "value", Operator::Equals, "ok"),
        },
    ];
    let (wf, switch_id, case_a_id, _case_b_id, _default_id) = make_switch_workflow(cases);

    let mut exec = make_executor_with_workflow(wf);
    exec.context
        .set_variable("status.value", Value::String("error".into()));

    let (name, node_type) = extract_node_info(&exec.workflow, switch_id);
    let next = exec.eval_control_flow(switch_id, &name, &node_type, &mut RetryContext::new());

    assert_eq!(next, Some(case_a_id));
}

#[test]
fn switch_follows_second_case_when_first_does_not_match() {
    let cases = vec![
        SwitchCase {
            name: "case_a".to_string(),
            condition: string_condition("status", "value", Operator::Equals, "error"),
        },
        SwitchCase {
            name: "case_b".to_string(),
            condition: string_condition("status", "value", Operator::Equals, "ok"),
        },
    ];
    let (wf, switch_id, _case_a_id, case_b_id, _default_id) = make_switch_workflow(cases);

    let mut exec = make_executor_with_workflow(wf);
    exec.context
        .set_variable("status.value", Value::String("ok".into()));

    let (name, node_type) = extract_node_info(&exec.workflow, switch_id);
    let next = exec.eval_control_flow(switch_id, &name, &node_type, &mut RetryContext::new());

    assert_eq!(next, Some(case_b_id));
}

#[test]
fn switch_follows_default_when_no_case_matches() {
    let cases = vec![
        SwitchCase {
            name: "case_a".to_string(),
            condition: string_condition("status", "value", Operator::Equals, "error"),
        },
        SwitchCase {
            name: "case_b".to_string(),
            condition: string_condition("status", "value", Operator::Equals, "ok"),
        },
    ];
    let (wf, switch_id, _case_a_id, _case_b_id, default_id) = make_switch_workflow(cases);

    let mut exec = make_executor_with_workflow(wf);
    exec.context
        .set_variable("status.value", Value::String("unknown".into()));

    let (name, node_type) = extract_node_info(&exec.workflow, switch_id);
    let next = exec.eval_control_flow(switch_id, &name, &node_type, &mut RetryContext::new());

    assert_eq!(next, Some(default_id));
}

#[test]
fn switch_returns_none_when_no_match_and_no_default_edge() {
    // Build a switch workflow without a SwitchDefault edge.
    let cases = vec![SwitchCase {
        name: "case_a".to_string(),
        condition: string_condition("status", "value", Operator::Equals, "error"),
    }];
    let mut wf = Workflow::new("test-switch-no-default");
    let switch_id = wf.add_node(
        NodeType::Switch(SwitchParams { cases }),
        Position { x: 0.0, y: 0.0 },
    );
    let case_a_id = wf.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 100.0, y: 0.0 },
    );
    wf.add_edge_with_output(
        switch_id,
        case_a_id,
        EdgeOutput::SwitchCase {
            name: "case_a".to_string(),
        },
    );
    // No SwitchDefault edge added.

    let mut exec = make_executor_with_workflow(wf);
    exec.context
        .set_variable("status.value", Value::String("ok".into()));

    let (name, node_type) = extract_node_info(&exec.workflow, switch_id);
    let next = exec.eval_control_flow(switch_id, &name, &node_type, &mut RetryContext::new());

    assert_eq!(next, None);
}

// ---------------------------------------------------------------------------
// Loop node tests
// ---------------------------------------------------------------------------

#[test]
fn loop_first_iteration_always_enters_body() {
    // Do-while semantics: iteration 0 always enters the body,
    // even if exit_condition would be true.
    let exit_condition = bool_condition("check", "done", true);
    let (wf, loop_id, body_id, _done_id) = make_loop_workflow(exit_condition, 10);

    let mut exec = make_executor_with_workflow(wf);
    // Set the variable so the exit condition IS true -- but iteration 0
    // should skip the exit check and enter the body anyway.
    exec.context.set_variable("check.done", Value::Bool(true));

    let (name, node_type) = extract_node_info(&exec.workflow, loop_id);
    let next = exec.eval_control_flow(loop_id, &name, &node_type, &mut RetryContext::new());

    assert_eq!(next, Some(body_id));
    // Counter should have been incremented from 0 to 1.
    assert_eq!(exec.context.loop_counters[&loop_id], 1);
}

#[test]
fn loop_exits_on_max_iterations() {
    let exit_condition = bool_condition("check", "done", true);
    let (wf, loop_id, _body_id, done_id) = make_loop_workflow(exit_condition, 3);

    let mut exec = make_executor_with_workflow(wf);
    // Exit condition is false, but we've already done max_iterations.
    exec.context.set_variable("check.done", Value::Bool(false));
    exec.context.loop_counters.insert(loop_id, 3);

    let (name, node_type) = extract_node_info(&exec.workflow, loop_id);
    let mut ctx = RetryContext::new();
    let next = exec.eval_control_flow(loop_id, &name, &node_type, &mut ctx);

    assert_eq!(next, Some(done_id));
    // Counter should be cleaned up after exit.
    assert!(!exec.context.loop_counters.contains_key(&loop_id));
    // pending_loop_exit should be set.
    let pending = ctx.pending_loop_exit.as_ref().unwrap();
    assert_eq!(pending.node_id, loop_id);
    assert_eq!(
        pending.reason,
        crate::executor::LoopExitReason::MaxIterations
    );
    assert_eq!(pending.iterations, 3);
}

#[test]
fn loop_exits_on_condition_met() {
    let exit_condition = bool_condition("check", "done", true);
    let (wf, loop_id, _body_id, done_id) = make_loop_workflow(exit_condition, 10);

    let mut exec = make_executor_with_workflow(wf);
    // Exit condition is true, and we're past iteration 0.
    exec.context.set_variable("check.done", Value::Bool(true));
    exec.context.loop_counters.insert(loop_id, 2);

    let (name, node_type) = extract_node_info(&exec.workflow, loop_id);
    let mut ctx = RetryContext::new();
    let next = exec.eval_control_flow(loop_id, &name, &node_type, &mut ctx);

    assert_eq!(next, Some(done_id));
    assert!(!exec.context.loop_counters.contains_key(&loop_id));
    let pending = ctx.pending_loop_exit.as_ref().unwrap();
    assert_eq!(
        pending.reason,
        crate::executor::LoopExitReason::ConditionMet
    );
    assert_eq!(pending.iterations, 2);
}

#[test]
fn loop_continues_when_under_max_and_condition_false() {
    let exit_condition = bool_condition("check", "done", true);
    let (wf, loop_id, body_id, _done_id) = make_loop_workflow(exit_condition, 10);

    let mut exec = make_executor_with_workflow(wf);
    exec.context.set_variable("check.done", Value::Bool(false));
    exec.context.loop_counters.insert(loop_id, 2);

    let (name, node_type) = extract_node_info(&exec.workflow, loop_id);
    let mut ctx = RetryContext::new();
    let next = exec.eval_control_flow(loop_id, &name, &node_type, &mut ctx);

    assert_eq!(next, Some(body_id));
    // Counter should have been incremented from 2 to 3.
    assert_eq!(exec.context.loop_counters[&loop_id], 3);
    // No pending exit should be set.
    assert!(ctx.pending_loop_exit.is_none());
}

#[test]
fn loop_max_iterations_takes_priority_over_condition() {
    // When counter == max AND condition is also true, max_iterations branch
    // fires first (checked before condition in the code).
    let exit_condition = bool_condition("check", "done", true);
    let (wf, loop_id, _body_id, done_id) = make_loop_workflow(exit_condition, 5);

    let mut exec = make_executor_with_workflow(wf);
    exec.context.set_variable("check.done", Value::Bool(true));
    exec.context.loop_counters.insert(loop_id, 5);

    let (name, node_type) = extract_node_info(&exec.workflow, loop_id);
    let mut ctx = RetryContext::new();
    let next = exec.eval_control_flow(loop_id, &name, &node_type, &mut ctx);

    assert_eq!(next, Some(done_id));
    let pending = ctx.pending_loop_exit.as_ref().unwrap();
    assert_eq!(
        pending.reason,
        crate::executor::LoopExitReason::MaxIterations
    );
}

// ---------------------------------------------------------------------------
// EndLoop node tests
// ---------------------------------------------------------------------------

#[test]
fn endloop_jumps_back_to_paired_loop_node() {
    let exit_condition = bool_condition("check", "done", true);
    let mut wf = Workflow::new("test-endloop");

    let loop_id = wf.add_node(
        NodeType::Loop(LoopParams {
            exit_condition,
            max_iterations: 10,
        }),
        Position { x: 0.0, y: 0.0 },
    );
    let body_id = wf.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 100.0, y: 0.0 },
    );
    let endloop_id = wf.add_node(
        NodeType::EndLoop(EndLoopParams { loop_id }),
        Position { x: 200.0, y: 0.0 },
    );
    let done_id = wf.add_node(
        NodeType::TypeText(TypeTextParams::default()),
        Position { x: 0.0, y: 100.0 },
    );

    wf.add_edge_with_output(loop_id, body_id, EdgeOutput::LoopBody);
    wf.add_edge(body_id, endloop_id);
    wf.add_edge(endloop_id, loop_id);
    wf.add_edge_with_output(loop_id, done_id, EdgeOutput::LoopDone);

    let mut exec = make_executor_with_workflow(wf);

    let (name, node_type) = extract_node_info(&exec.workflow, endloop_id);
    let next = exec.eval_control_flow(endloop_id, &name, &node_type, &mut RetryContext::new());

    assert_eq!(next, Some(loop_id));
}
