use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::{NodeType, Workflow};

use super::ValidationError;

/// Validate loop pairing:
/// - Every EndLoop.loop_id must reference a valid Loop node
/// - Every Loop node must have exactly one EndLoop referencing it
/// - EndLoop's outgoing edge must point to its loop_id
pub(crate) fn validate_loop_pairing(workflow: &Workflow) -> Result<(), ValidationError> {
    // Collect all Loop node IDs
    let loop_node_ids: HashSet<Uuid> = workflow
        .nodes
        .iter()
        .filter(|n| matches!(&n.node_type, NodeType::Loop(_)))
        .map(|n| n.id)
        .collect();

    // Track which Loop nodes are referenced by EndLoop nodes
    let mut loop_references: HashMap<Uuid, usize> = HashMap::new();

    for node in &workflow.nodes {
        if let NodeType::EndLoop(params) = &node.node_type {
            // EndLoop.loop_id must reference a valid Loop node
            if !loop_node_ids.contains(&params.loop_id) {
                return Err(ValidationError::InvalidEndLoopTarget(node.name.clone()));
            }

            *loop_references.entry(params.loop_id).or_insert(0) += 1;

            // EndLoop's outgoing edge must point to its loop_id
            let outgoing: Vec<_> = workflow
                .edges
                .iter()
                .filter(|e| e.from == node.id)
                .collect();
            if outgoing.len() != 1 || outgoing[0].to != params.loop_id {
                return Err(ValidationError::EndLoopEdgeMismatch(node.name.clone()));
            }
        }
    }

    // Every Loop node must have exactly one EndLoop referencing it
    for node in &workflow.nodes {
        if matches!(&node.node_type, NodeType::Loop(_)) {
            let count = loop_references.get(&node.id).copied().unwrap_or(0);
            if count == 0 {
                return Err(ValidationError::UnpairedLoop(node.name.clone()));
            }
            if count > 1 {
                return Err(ValidationError::MultipleEndLoops(node.name.clone()));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{dummy_condition, pos};
    use crate::{ClickParams, EdgeOutput, EndLoopParams, LoopParams, NodeType, Workflow};

    use super::super::ValidationError;
    use super::super::validate_workflow;

    #[test]
    fn test_validate_valid_loop() {
        // Loop -> (LoopBody) -> BodyNode -> EndLoop -> (back to Loop)
        // Loop -> (LoopDone) -> DoneNode
        let mut wf = Workflow::default();
        let loop_node = wf.add_node(
            NodeType::Loop(LoopParams {
                exit_condition: dummy_condition(),
                max_iterations: 10,
            }),
            pos(0.0, 0.0),
        );
        let body = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let end_loop = wf.add_node(
            NodeType::EndLoop(EndLoopParams { loop_id: loop_node }),
            pos(200.0, 0.0),
        );
        let done = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 100.0));

        wf.add_edge_with_output(loop_node, body, EdgeOutput::LoopBody);
        wf.add_edge_with_output(loop_node, done, EdgeOutput::LoopDone);
        wf.add_edge(body, end_loop);
        wf.add_edge(end_loop, loop_node); // back-edge

        assert!(validate_workflow(&wf).is_ok());
    }

    #[test]
    fn test_validate_terminal_loop() {
        // Loop with LoopBody only (no LoopDone) -- workflow ends after loop
        let mut wf = Workflow::default();
        let loop_node = wf.add_node(
            NodeType::Loop(LoopParams {
                exit_condition: dummy_condition(),
                max_iterations: 10,
            }),
            pos(0.0, 0.0),
        );
        let body = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let end_loop = wf.add_node(
            NodeType::EndLoop(EndLoopParams { loop_id: loop_node }),
            pos(200.0, 0.0),
        );

        wf.add_edge_with_output(loop_node, body, EdgeOutput::LoopBody);
        wf.add_edge(body, end_loop);
        wf.add_edge(end_loop, loop_node); // back-edge

        assert!(validate_workflow(&wf).is_ok());
    }

    #[test]
    fn test_validate_loop_without_end_loop() {
        // Loop node but no EndLoop referencing it -> UnpairedLoop
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

        wf.add_edge_with_output(loop_node, body, EdgeOutput::LoopBody);
        wf.add_edge_with_output(loop_node, done, EdgeOutput::LoopDone);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::UnpairedLoop(_)));
    }

    #[test]
    fn test_validate_end_loop_bad_target() {
        // EndLoop with loop_id pointing to a non-Loop node -> InvalidEndLoopTarget
        let mut wf = Workflow::default();
        let regular = wf.add_node(NodeType::Click(ClickParams::default()), pos(0.0, 0.0));
        let end_loop = wf.add_node(
            NodeType::EndLoop(EndLoopParams { loop_id: regular }),
            pos(100.0, 0.0),
        );
        // regular -> end_loop -> regular (back-edge excluded), single entry point
        wf.add_edge(regular, end_loop);
        wf.add_edge(end_loop, regular);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::InvalidEndLoopTarget(_)));
    }

    #[test]
    fn test_validate_end_loop_edge_mismatch() {
        // EndLoop's outgoing edge points somewhere other than its loop_id
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
        let end_loop = wf.add_node(
            NodeType::EndLoop(EndLoopParams { loop_id: loop_node }),
            pos(200.0, 0.0),
        );

        wf.add_edge_with_output(loop_node, body, EdgeOutput::LoopBody);
        wf.add_edge_with_output(loop_node, done, EdgeOutput::LoopDone);
        wf.add_edge(body, end_loop);
        // EndLoop edge points to done instead of loop_node
        wf.add_edge(end_loop, done);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::EndLoopEdgeMismatch(_)));
    }

    #[test]
    fn test_validate_multiple_end_loops_for_same_loop() {
        // Two EndLoop nodes both referencing the same Loop -> MultipleEndLoops
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
        let end_loop_1 = wf.add_node(
            NodeType::EndLoop(EndLoopParams { loop_id: loop_node }),
            pos(200.0, 0.0),
        );
        let end_loop_2 = wf.add_node(
            NodeType::EndLoop(EndLoopParams { loop_id: loop_node }),
            pos(300.0, 0.0),
        );

        wf.add_edge_with_output(loop_node, body, EdgeOutput::LoopBody);
        wf.add_edge_with_output(loop_node, done, EdgeOutput::LoopDone);
        wf.add_edge(body, end_loop_1);
        wf.add_edge(end_loop_1, loop_node);
        // Connect done -> end_loop_2 so it has an incoming edge (single entry point)
        wf.add_edge(done, end_loop_2);
        wf.add_edge(end_loop_2, loop_node);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::MultipleEndLoops(_)));
    }

    #[test]
    fn test_validate_valid_nested_loops() {
        // Outer Loop -> (LoopBody) -> Inner Loop -> (LoopBody) -> Node -> Inner EndLoop -> Inner Loop
        //                                        Inner Loop -> (LoopDone) -> Outer EndLoop -> Outer Loop
        // Outer Loop -> (LoopDone) -> FinalNode
        let mut wf = Workflow::default();

        let outer_loop = wf.add_node(
            NodeType::Loop(LoopParams {
                exit_condition: dummy_condition(),
                max_iterations: 10,
            }),
            pos(0.0, 0.0),
        );
        let inner_loop = wf.add_node(
            NodeType::Loop(LoopParams {
                exit_condition: dummy_condition(),
                max_iterations: 5,
            }),
            pos(100.0, 0.0),
        );
        let inner_body = wf.add_node(NodeType::Click(ClickParams::default()), pos(200.0, 0.0));
        let inner_end = wf.add_node(
            NodeType::EndLoop(EndLoopParams {
                loop_id: inner_loop,
            }),
            pos(300.0, 0.0),
        );
        let outer_end = wf.add_node(
            NodeType::EndLoop(EndLoopParams {
                loop_id: outer_loop,
            }),
            pos(200.0, 100.0),
        );
        let final_node = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 200.0));

        // Outer loop edges
        wf.add_edge_with_output(outer_loop, inner_loop, EdgeOutput::LoopBody);
        wf.add_edge_with_output(outer_loop, final_node, EdgeOutput::LoopDone);

        // Inner loop edges
        wf.add_edge_with_output(inner_loop, inner_body, EdgeOutput::LoopBody);
        wf.add_edge_with_output(inner_loop, outer_end, EdgeOutput::LoopDone);

        // Inner body -> inner end -> inner loop (back-edge)
        wf.add_edge(inner_body, inner_end);
        wf.add_edge(inner_end, inner_loop);

        // Outer end -> outer loop (back-edge)
        wf.add_edge(outer_end, outer_loop);

        assert!(validate_workflow(&wf).is_ok());
    }
}
