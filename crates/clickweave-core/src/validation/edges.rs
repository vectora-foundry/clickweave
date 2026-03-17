use std::collections::HashSet;

use crate::{EdgeOutput, NodeType, Workflow};

use super::ValidationError;

/// Validate that each node has the correct outgoing edges for its type.
pub(crate) fn validate_outgoing_edges(workflow: &Workflow) -> Result<(), ValidationError> {
    for node in &workflow.nodes {
        let outgoing: Vec<_> = workflow
            .edges
            .iter()
            .filter(|e| e.from == node.id)
            .collect();

        match &node.node_type {
            NodeType::If(_) => {
                let has_true = outgoing
                    .iter()
                    .any(|e| e.output.as_ref() == Some(&EdgeOutput::IfTrue));
                let has_false = outgoing
                    .iter()
                    .any(|e| e.output.as_ref() == Some(&EdgeOutput::IfFalse));
                if !has_true || !has_false {
                    return Err(ValidationError::MissingIfBranch(node.name.clone()));
                }
                if outgoing.len() != 2 {
                    return Err(ValidationError::ExtraIfEdges(node.name.clone()));
                }
            }
            NodeType::Switch(params) => {
                let mut declared_names: HashSet<&str> = HashSet::new();
                for case in &params.cases {
                    if !declared_names.insert(case.name.as_str()) {
                        return Err(ValidationError::DuplicateSwitchCaseName(
                            node.name.clone(),
                            case.name.clone(),
                        ));
                    }
                }

                for case in &params.cases {
                    let count = outgoing
                        .iter()
                        .filter(|e| {
                            e.output.as_ref()
                                == Some(&EdgeOutput::SwitchCase {
                                    name: case.name.clone(),
                                })
                        })
                        .count();
                    if count == 0 {
                        return Err(ValidationError::MissingSwitchCase(
                            node.name.clone(),
                            case.name.clone(),
                        ));
                    }
                    if count > 1 {
                        return Err(ValidationError::DuplicateSwitchCase(
                            node.name.clone(),
                            case.name.clone(),
                        ));
                    }
                }

                // Reject unknown outputs (not a declared case and not SwitchDefault)
                for edge in &outgoing {
                    match &edge.output {
                        Some(EdgeOutput::SwitchCase { name })
                            if !declared_names.contains(name.as_str()) =>
                        {
                            return Err(ValidationError::UnknownSwitchOutput(
                                node.name.clone(),
                                name.clone(),
                            ));
                        }
                        Some(EdgeOutput::SwitchDefault) | Some(EdgeOutput::SwitchCase { .. }) => {}
                        other => {
                            let label = match other {
                                Some(o) => format!("{:?}", o),
                                None => "unlabeled".to_string(),
                            };
                            return Err(ValidationError::UnknownSwitchOutput(
                                node.name.clone(),
                                label,
                            ));
                        }
                    }
                }
            }
            NodeType::Loop(_) => {
                let has_body = outgoing
                    .iter()
                    .any(|e| e.output.as_ref() == Some(&EdgeOutput::LoopBody));
                if !has_body {
                    return Err(ValidationError::MissingLoopEdge(node.name.clone()));
                }
                // LoopDone is optional — terminal loops may have only a LoopBody edge.
                let has_done = outgoing
                    .iter()
                    .any(|e| e.output.as_ref() == Some(&EdgeOutput::LoopDone));
                if outgoing.len() != if has_done { 2 } else { 1 } {
                    return Err(ValidationError::ExtraLoopEdges(node.name.clone()));
                }
            }
            NodeType::EndLoop(_) => {
                // EndLoop must have exactly 1 regular edge (validated in loop pairing)
                // but we don't enforce the "max 1" rule here since loop pairing covers it.
            }
            _ => {
                // Regular nodes: 0 or 1 outgoing edges
                if outgoing.len() > 1 {
                    return Err(ValidationError::MultipleOutgoingEdges(node.name.clone()));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{dummy_condition, pos};
    use crate::{
        ClickParams, EdgeOutput, EndLoopParams, IfParams, LoopParams, NodeType, SwitchCase,
        SwitchParams, TypeTextParams, Workflow,
    };

    use super::super::ValidationError;
    use super::super::validate_workflow;

    #[test]
    fn test_validate_multiple_outgoing_edges() {
        let mut wf = Workflow::default();
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(0.0, 0.0));
        let b = wf.add_node(
            NodeType::TypeText(TypeTextParams::default()),
            pos(100.0, 0.0),
        );
        let c = wf.add_node(
            NodeType::TypeText(TypeTextParams::default()),
            pos(200.0, 0.0),
        );
        wf.add_edge(a, b);
        wf.add_edge(a, c); // a has 2 outgoing

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::MultipleOutgoingEdges(_)));
    }

    // --- If node tests ---

    #[test]
    fn test_validate_valid_if_workflow() {
        // If -> (IfTrue) -> A, If -> (IfFalse) -> B, both A and B -> C (converge)
        let mut wf = Workflow::default();
        let if_node = wf.add_node(
            NodeType::If(IfParams {
                condition: dummy_condition(),
            }),
            pos(0.0, 0.0),
        );
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let b = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));
        let c = wf.add_node(NodeType::Click(ClickParams::default()), pos(200.0, 50.0));

        wf.add_edge_with_output(if_node, a, EdgeOutput::IfTrue);
        wf.add_edge_with_output(if_node, b, EdgeOutput::IfFalse);
        wf.add_edge(a, c);
        wf.add_edge(b, c);

        assert!(validate_workflow(&wf).is_ok());
    }

    #[test]
    fn test_validate_if_missing_branch() {
        // If with only IfTrue edge -> MissingIfBranch
        let mut wf = Workflow::default();
        let if_node = wf.add_node(
            NodeType::If(IfParams {
                condition: dummy_condition(),
            }),
            pos(0.0, 0.0),
        );
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));

        wf.add_edge_with_output(if_node, a, EdgeOutput::IfTrue);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::MissingIfBranch(_)));
    }

    #[test]
    fn test_validate_if_extra_edges() {
        let mut wf = Workflow::default();
        let if_node = wf.add_node(
            NodeType::If(IfParams {
                condition: dummy_condition(),
            }),
            pos(0.0, 0.0),
        );
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let b = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));
        let c = wf.add_node(NodeType::Click(ClickParams::default()), pos(200.0, 0.0));

        wf.add_edge_with_output(if_node, a, EdgeOutput::IfTrue);
        wf.add_edge_with_output(if_node, b, EdgeOutput::IfFalse);
        // Extra unlabeled edge
        wf.add_edge(if_node, c);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::ExtraIfEdges(_)));
    }

    #[test]
    fn test_validate_loop_extra_edges() {
        let mut wf = Workflow::default();
        let loop_node = wf.add_node(
            NodeType::Loop(LoopParams {
                exit_condition: dummy_condition(),
                max_iterations: 10,
            }),
            pos(0.0, 0.0),
        );
        let body = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let done = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));
        let extra = wf.add_node(NodeType::Click(ClickParams::default()), pos(200.0, 0.0));
        let end_loop = wf.add_node(
            NodeType::EndLoop(EndLoopParams { loop_id: loop_node }),
            pos(200.0, 100.0),
        );

        wf.add_edge_with_output(loop_node, body, EdgeOutput::LoopBody);
        wf.add_edge_with_output(loop_node, done, EdgeOutput::LoopDone);
        wf.add_edge(loop_node, extra); // extra unlabeled edge
        wf.add_edge(body, end_loop);
        wf.add_edge(end_loop, loop_node);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::ExtraLoopEdges(_)));
    }

    // --- Switch tests ---

    #[test]
    fn test_validate_valid_switch_workflow() {
        let mut wf = Workflow::default();
        let switch_node = wf.add_node(
            NodeType::Switch(SwitchParams {
                cases: vec![
                    SwitchCase {
                        name: "case_a".to_string(),
                        condition: dummy_condition(),
                    },
                    SwitchCase {
                        name: "case_b".to_string(),
                        condition: dummy_condition(),
                    },
                ],
            }),
            pos(0.0, 0.0),
        );
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let b = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));

        wf.add_edge_with_output(
            switch_node,
            a,
            EdgeOutput::SwitchCase {
                name: "case_a".to_string(),
            },
        );
        wf.add_edge_with_output(
            switch_node,
            b,
            EdgeOutput::SwitchCase {
                name: "case_b".to_string(),
            },
        );

        assert!(validate_workflow(&wf).is_ok());
    }

    #[test]
    fn test_validate_switch_missing_case() {
        let mut wf = Workflow::default();
        let switch_node = wf.add_node(
            NodeType::Switch(SwitchParams {
                cases: vec![
                    SwitchCase {
                        name: "case_a".to_string(),
                        condition: dummy_condition(),
                    },
                    SwitchCase {
                        name: "case_b".to_string(),
                        condition: dummy_condition(),
                    },
                ],
            }),
            pos(0.0, 0.0),
        );
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));

        // Only provide edge for case_a, missing case_b
        wf.add_edge_with_output(
            switch_node,
            a,
            EdgeOutput::SwitchCase {
                name: "case_a".to_string(),
            },
        );

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::MissingSwitchCase(_, ref case) if case == "case_b"));
    }

    #[test]
    fn test_validate_switch_duplicate_case() {
        let mut wf = Workflow::default();
        let switch_node = wf.add_node(
            NodeType::Switch(SwitchParams {
                cases: vec![SwitchCase {
                    name: "case_a".to_string(),
                    condition: dummy_condition(),
                }],
            }),
            pos(0.0, 0.0),
        );
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let b = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));

        // Two edges for the same case
        wf.add_edge_with_output(
            switch_node,
            a,
            EdgeOutput::SwitchCase {
                name: "case_a".to_string(),
            },
        );
        wf.add_edge_with_output(
            switch_node,
            b,
            EdgeOutput::SwitchCase {
                name: "case_a".to_string(),
            },
        );

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::DuplicateSwitchCase(_, _)));
    }

    #[test]
    fn test_validate_switch_unknown_output() {
        let mut wf = Workflow::default();
        let switch_node = wf.add_node(
            NodeType::Switch(SwitchParams {
                cases: vec![SwitchCase {
                    name: "case_a".to_string(),
                    condition: dummy_condition(),
                }],
            }),
            pos(0.0, 0.0),
        );
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let b = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));

        wf.add_edge_with_output(
            switch_node,
            a,
            EdgeOutput::SwitchCase {
                name: "case_a".to_string(),
            },
        );
        // Edge with undeclared case name
        wf.add_edge_with_output(
            switch_node,
            b,
            EdgeOutput::SwitchCase {
                name: "case_unknown".to_string(),
            },
        );

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::UnknownSwitchOutput(_, _)));
    }

    #[test]
    fn test_validate_switch_duplicate_case_name() {
        let mut wf = Workflow::default();
        let switch_node = wf.add_node(
            NodeType::Switch(SwitchParams {
                cases: vec![
                    SwitchCase {
                        name: "same".to_string(),
                        condition: dummy_condition(),
                    },
                    SwitchCase {
                        name: "same".to_string(),
                        condition: dummy_condition(),
                    },
                ],
            }),
            pos(0.0, 0.0),
        );
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let b = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));

        wf.add_edge_with_output(
            switch_node,
            a,
            EdgeOutput::SwitchCase {
                name: "same".to_string(),
            },
        );
        wf.add_edge_with_output(
            switch_node,
            b,
            EdgeOutput::SwitchCase {
                name: "same".to_string(),
            },
        );

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::DuplicateSwitchCaseName(_, _)
        ));
    }
}
