use std::collections::HashSet;

use crate::{NodeType, ValueRef, Workflow, sanitize_node_name};

use super::ValidationError;

/// Validate that variable references in conditions point to actual nodes.
///
/// For each Loop/If/Switch condition, extracts `Variable` references, splits
/// on the first `.` to get the node-name prefix, and checks that a node with
/// that sanitized name exists in the workflow.
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
        .map(|n| sanitize_node_name(&n.name))
        .collect();

    for node in &workflow.nodes {
        let value_refs: Vec<&ValueRef> = match &node.node_type {
            NodeType::Loop(p) => vec![&p.exit_condition.left, &p.exit_condition.right],
            NodeType::If(p) => vec![&p.condition.left, &p.condition.right],
            NodeType::Switch(p) => p
                .cases
                .iter()
                .flat_map(|c| [&c.condition.left, &c.condition.right])
                .collect(),
            _ => continue,
        };

        for value_ref in value_refs {
            if let ValueRef::Variable { name } = value_ref {
                if name.is_empty() {
                    return Err(ValidationError::EmptyVariableReference(node.name.clone()));
                }
                let prefix = name.split('.').next().unwrap_or(name);
                if !known_prefixes.contains(prefix) {
                    return Err(ValidationError::InvalidVariableReference {
                        node_name: node.name.clone(),
                        variable: name.clone(),
                        prefix: prefix.to_string(),
                    });
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::pos;
    use crate::{
        ClickParams, Condition, EdgeOutput, EndLoopParams, FindTextParams, IfParams, LiteralValue,
        LoopParams, NodeType, Operator, SwitchCase, SwitchParams, ValueRef, Workflow,
    };

    use super::super::ValidationError;
    use super::super::validate_workflow;

    #[test]
    fn test_validate_loop_valid_variable_reference() {
        // Loop exit condition references "find_text.found" -- a node named "Find Text" exists
        let mut wf = Workflow::default();
        let find = wf.add_node(NodeType::FindText(FindTextParams::default()), pos(0.0, 0.0));
        let loop_node = wf.add_node(
            NodeType::Loop(LoopParams {
                exit_condition: Condition {
                    left: ValueRef::Variable {
                        name: "find_text.found".to_string(),
                    },
                    operator: Operator::Equals,
                    right: ValueRef::Literal {
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
                    left: ValueRef::Variable {
                        name: "find_textt.found".to_string(),
                    },
                    operator: Operator::Equals,
                    right: ValueRef::Literal {
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
                    left: ValueRef::Variable {
                        name: "nonexistent_node.result".to_string(),
                    },
                    operator: Operator::Equals,
                    right: ValueRef::Literal {
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
    fn test_validate_literal_only_condition_passes() {
        // Conditions with only literals (no variables) should pass
        let mut wf = Workflow::default();
        let if_node = wf.add_node(
            NodeType::If(IfParams {
                condition: Condition {
                    left: ValueRef::Literal {
                        value: LiteralValue::Number { value: 1.0 },
                    },
                    operator: Operator::Equals,
                    right: ValueRef::Literal {
                        value: LiteralValue::Number { value: 1.0 },
                    },
                },
            }),
            pos(0.0, 0.0),
        );
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let b = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));

        wf.add_edge_with_output(if_node, a, EdgeOutput::IfTrue);
        wf.add_edge_with_output(if_node, b, EdgeOutput::IfFalse);

        assert!(validate_workflow(&wf).is_ok());
    }

    #[test]
    fn test_validate_variable_without_dot_is_invalid() {
        // A variable name with no dot (no field) -- prefix is the whole name,
        // which must still match a sanitized node name
        let mut wf = Workflow::default();
        let if_node = wf.add_node(
            NodeType::If(IfParams {
                condition: Condition {
                    left: ValueRef::Variable {
                        name: "no_such_node".to_string(),
                    },
                    operator: Operator::IsNotEmpty,
                    right: ValueRef::Literal {
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
                        left: ValueRef::Variable {
                            name: "typo_node.found".to_string(),
                        },
                        operator: Operator::Equals,
                        right: ValueRef::Literal {
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
        // A Loop condition that references its own Loop node name should fail --
        // control-flow nodes don't produce runtime variables.
        let mut wf = Workflow::default();
        let find = wf.add_node(NodeType::FindText(FindTextParams::default()), pos(0.0, 0.0));
        let loop_node = wf.add_node(
            NodeType::Loop(LoopParams {
                exit_condition: Condition {
                    left: ValueRef::Variable {
                        name: "loop.success".to_string(),
                    },
                    operator: Operator::Equals,
                    right: ValueRef::Literal {
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
                    left: ValueRef::Variable {
                        name: String::new(),
                    },
                    operator: Operator::Equals,
                    right: ValueRef::Literal {
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
