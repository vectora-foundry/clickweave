use std::collections::HashSet;

use crate::output_schema::{ConditionValue, OutputRef};
use crate::{NodeType, Workflow};

use super::ValidationError;

/// Validate that variable references in conditions point to actual nodes.
///
/// For each Loop/If/Switch condition, extracts `OutputRef` references from
/// `left` and any `ConditionValue::Ref` on `right`, and checks that a node
/// with that name exists in the workflow.
pub(crate) fn validate_condition_variables(workflow: &Workflow) -> Result<(), ValidationError> {
    // Only include nodes that actually produce runtime variables.
    // Control-flow nodes (If/Switch/Loop/EndLoop) are evaluated by
    // eval_control_flow without variable extraction, so referencing
    // them as variable prefixes would resolve to null at runtime.
    let known_prefixes: HashSet<String> = workflow
        .nodes
        .iter()
        .filter(|n| {
            !matches!(
                n.node_type,
                NodeType::If(_) | NodeType::Switch(_) | NodeType::Loop(_) | NodeType::EndLoop(_)
            )
        })
        .map(|n| n.auto_id.clone())
        .collect();

    for node in &workflow.nodes {
        let conditions: Vec<&crate::Condition> = match &node.node_type {
            NodeType::Loop(p) => vec![&p.exit_condition],
            NodeType::If(p) => vec![&p.condition],
            NodeType::Switch(p) => p.cases.iter().map(|c| &c.condition).collect(),
            _ => continue,
        };

        for condition in conditions {
            // Validate left (always an OutputRef)
            validate_output_ref(&condition.left, &node.name, &known_prefixes)?;

            // Validate right (only if it's a Ref)
            if let ConditionValue::Ref(ref output_ref) = condition.right {
                validate_output_ref(output_ref, &node.name, &known_prefixes)?;
            }
        }
    }

    Ok(())
}

fn validate_output_ref(
    output_ref: &OutputRef,
    node_name: &str,
    known_prefixes: &HashSet<String>,
) -> Result<(), ValidationError> {
    if output_ref.node.is_empty() {
        return Err(ValidationError::EmptyVariableReference(
            node_name.to_string(),
        ));
    }
    if !known_prefixes.contains(&output_ref.node) {
        let variable = format!("{}.{}", output_ref.node, output_ref.field);
        return Err(ValidationError::InvalidVariableReference {
            node_name: node_name.to_string(),
            variable,
            prefix: output_ref.node.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::pos;
    use crate::output_schema::{ConditionValue, OutputRef};
    use crate::{
        ClickParams, Condition, EdgeOutput, EndLoopParams, FindTextParams, IfParams, LiteralValue,
        LoopParams, NodeType, Operator, SwitchCase, SwitchParams, Workflow,
    };

    use super::super::ValidationError;
    use super::super::validate_workflow;

    #[test]
    fn test_validate_loop_valid_variable_reference() {
        // Loop exit condition references "find_text_1.found" -- a FindText node exists with that auto_id
        let mut wf = Workflow::default();
        let find = wf.add_node(NodeType::FindText(FindTextParams::default()), pos(0.0, 0.0));
        let loop_node = wf.add_node(
            NodeType::Loop(LoopParams {
                exit_condition: Condition {
                    left: OutputRef {
                        node: "find_text_1".to_string(),
                        field: "found".to_string(),
                    },
                    operator: Operator::Equals,
                    right: ConditionValue::Literal {
                        value: LiteralValue::Bool { value: true },
                    },
                },
                max_iterations: 10,
            }),
            pos(100.0, 0.0),
        );
        let end_loop = wf.add_node(
            NodeType::EndLoop(EndLoopParams { loop_id: loop_node }),
            pos(200.0, 0.0),
        );
        let done = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));

        wf.add_edge(find, loop_node);
        wf.add_edge_with_output(loop_node, end_loop, EdgeOutput::LoopBody);
        wf.add_edge_with_output(loop_node, done, EdgeOutput::LoopDone);
        wf.add_edge(end_loop, loop_node);

        assert!(validate_workflow(&wf).is_ok());
    }

    #[test]
    fn test_validate_loop_invalid_variable_reference() {
        // Loop exit condition references "find_textt.found" -- typo, no matching node
        let mut wf = Workflow::default();
        let find = wf.add_node(NodeType::FindText(FindTextParams::default()), pos(0.0, 0.0));
        let loop_node = wf.add_node(
            NodeType::Loop(LoopParams {
                exit_condition: Condition {
                    left: OutputRef {
                        node: "find_textt".to_string(),
                        field: "found".to_string(),
                    },
                    operator: Operator::Equals,
                    right: ConditionValue::Literal {
                        value: LiteralValue::Bool { value: true },
                    },
                },
                max_iterations: 10,
            }),
            pos(100.0, 0.0),
        );
        let end_loop = wf.add_node(
            NodeType::EndLoop(EndLoopParams { loop_id: loop_node }),
            pos(200.0, 0.0),
        );
        let done = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));

        wf.add_edge(find, loop_node);
        wf.add_edge_with_output(loop_node, end_loop, EdgeOutput::LoopBody);
        wf.add_edge_with_output(loop_node, done, EdgeOutput::LoopDone);
        wf.add_edge(end_loop, loop_node);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::InvalidVariableReference { .. }
        ));
    }

    #[test]
    fn test_validate_if_invalid_variable_reference() {
        // If condition references a variable with no matching node
        let mut wf = Workflow::default();
        let if_node = wf.add_node(
            NodeType::If(IfParams {
                condition: Condition {
                    left: OutputRef {
                        node: "nonexistent_node".to_string(),
                        field: "result".to_string(),
                    },
                    operator: Operator::Equals,
                    right: ConditionValue::Literal {
                        value: LiteralValue::Bool { value: true },
                    },
                },
            }),
            pos(0.0, 0.0),
        );
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let b = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));

        wf.add_edge_with_output(if_node, a, EdgeOutput::IfTrue);
        wf.add_edge_with_output(if_node, b, EdgeOutput::IfFalse);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::InvalidVariableReference { .. }
        ));
    }

    #[test]
    fn test_validate_literal_right_condition_passes() {
        // Conditions with a valid left ref and a literal right should pass
        let mut wf = Workflow::default();
        let click = wf.add_node(NodeType::Click(ClickParams::default()), pos(0.0, 100.0));
        let if_node = wf.add_node(
            NodeType::If(IfParams {
                condition: Condition {
                    left: OutputRef {
                        node: "click_1".to_string(),
                        field: "result".to_string(),
                    },
                    operator: Operator::Equals,
                    right: ConditionValue::Literal {
                        value: LiteralValue::Number { value: 1.0 },
                    },
                },
            }),
            pos(0.0, 0.0),
        );
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let b = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));

        wf.add_edge(click, if_node);
        wf.add_edge_with_output(if_node, a, EdgeOutput::IfTrue);
        wf.add_edge_with_output(if_node, b, EdgeOutput::IfFalse);

        assert!(validate_workflow(&wf).is_ok());
    }

    #[test]
    fn test_validate_node_ref_without_match_is_invalid() {
        // An OutputRef node that doesn't match any node should fail
        let mut wf = Workflow::default();
        let if_node = wf.add_node(
            NodeType::If(IfParams {
                condition: Condition {
                    left: OutputRef {
                        node: "no_such_node".to_string(),
                        field: "result".to_string(),
                    },
                    operator: Operator::IsNotEmpty,
                    right: ConditionValue::Literal {
                        value: LiteralValue::Bool { value: true },
                    },
                },
            }),
            pos(0.0, 0.0),
        );
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let b = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));

        wf.add_edge_with_output(if_node, a, EdgeOutput::IfTrue);
        wf.add_edge_with_output(if_node, b, EdgeOutput::IfFalse);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::InvalidVariableReference { .. }
        ));
    }

    #[test]
    fn test_validate_switch_invalid_variable_reference() {
        // Switch case condition references a variable with no matching node
        let mut wf = Workflow::default();
        let find = wf.add_node(NodeType::FindText(FindTextParams::default()), pos(0.0, 0.0));
        let switch_node = wf.add_node(
            NodeType::Switch(SwitchParams {
                cases: vec![SwitchCase {
                    name: "found".to_string(),
                    condition: Condition {
                        left: OutputRef {
                            node: "typo_node".to_string(),
                            field: "found".to_string(),
                        },
                        operator: Operator::Equals,
                        right: ConditionValue::Literal {
                            value: LiteralValue::Bool { value: true },
                        },
                    },
                }],
            }),
            pos(100.0, 0.0),
        );
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(200.0, 0.0));

        wf.add_edge(find, switch_node);
        wf.add_edge_with_output(
            switch_node,
            a,
            EdgeOutput::SwitchCase {
                name: "found".to_string(),
            },
        );

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::InvalidVariableReference { .. }
        ));
    }

    #[test]
    fn test_validate_control_flow_node_name_not_valid_prefix() {
        // A Loop condition that references its own Loop auto_id should fail --
        // control-flow nodes don't produce runtime variables.
        let mut wf = Workflow::default();
        let find = wf.add_node(NodeType::FindText(FindTextParams::default()), pos(0.0, 0.0));
        let loop_node = wf.add_node(
            NodeType::Loop(LoopParams {
                exit_condition: Condition {
                    left: OutputRef {
                        node: "loop_1".to_string(),
                        field: "success".to_string(),
                    },
                    operator: Operator::Equals,
                    right: ConditionValue::Literal {
                        value: LiteralValue::Bool { value: true },
                    },
                },
                max_iterations: 10,
            }),
            pos(100.0, 0.0),
        );
        let end_loop = wf.add_node(
            NodeType::EndLoop(EndLoopParams { loop_id: loop_node }),
            pos(200.0, 0.0),
        );
        let done = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));

        wf.add_edge(find, loop_node);
        wf.add_edge_with_output(loop_node, end_loop, EdgeOutput::LoopBody);
        wf.add_edge_with_output(loop_node, done, EdgeOutput::LoopDone);
        wf.add_edge(end_loop, loop_node);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::InvalidVariableReference { .. }
        ));
    }

    #[test]
    fn test_validate_empty_variable_reference_rejected() {
        // Empty variable name should be rejected, not silently skipped
        let mut wf = Workflow::default();
        let if_node = wf.add_node(
            NodeType::If(IfParams {
                condition: Condition {
                    left: OutputRef {
                        node: String::new(),
                        field: String::new(),
                    },
                    operator: Operator::Equals,
                    right: ConditionValue::Literal {
                        value: LiteralValue::Bool { value: true },
                    },
                },
            }),
            pos(0.0, 0.0),
        );
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let b = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));

        wf.add_edge_with_output(if_node, a, EdgeOutput::IfTrue);
        wf.add_edge_with_output(if_node, b, EdgeOutput::IfFalse);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::EmptyVariableReference(_)));
    }
}
