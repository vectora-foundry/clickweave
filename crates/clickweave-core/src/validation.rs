use std::collections::{HashMap, HashSet};

use thiserror::Error;
use uuid::Uuid;

use crate::{EdgeOutput, NodeType, ValueRef, Workflow, sanitize_node_name};

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("Workflow has no nodes")]
    NoNodes,

    #[error("No entry point found (all nodes have incoming edges)")]
    NoEntryPoint,

    #[error(
        "Multiple entry points found ({0} nodes with no incoming edges). Only one is supported."
    )]
    MultipleEntryPoints(usize),

    #[error("Node {0} has multiple outgoing edges (only single path allowed)")]
    MultipleOutgoingEdges(String),

    #[error("Cycle detected in workflow")]
    CycleDetected,

    #[error("If node '{0}' must have both true and false edges")]
    MissingIfBranch(String),

    #[error("Switch node '{0}' missing edge for case '{1}'")]
    MissingSwitchCase(String, String),

    #[error("Loop node '{0}' must have both body and done edges")]
    MissingLoopEdge(String),

    #[error("EndLoop node '{0}' references non-Loop node")]
    InvalidEndLoopTarget(String),

    #[error("Loop node '{0}' has no paired EndLoop")]
    UnpairedLoop(String),

    #[error("EndLoop node '{0}' edge does not point to its paired Loop")]
    EndLoopEdgeMismatch(String),

    #[error("Loop node '{0}' has multiple EndLoop nodes (expected exactly one)")]
    MultipleEndLoops(String),

    #[error("If node '{0}' has extra outgoing edges beyond IfTrue and IfFalse")]
    ExtraIfEdges(String),

    #[error("Loop node '{0}' has extra outgoing edges beyond LoopBody and LoopDone")]
    ExtraLoopEdges(String),

    #[error("Switch node '{0}' has duplicate edge for case '{1}'")]
    DuplicateSwitchCase(String, String),

    #[error("Switch node '{0}' has unknown output edge '{1}'")]
    UnknownSwitchOutput(String, String),

    #[error("Switch node '{0}' has duplicate case name '{1}'")]
    DuplicateSwitchCaseName(String, String),

    #[error(
        "Condition in node '{node_name}' references variable '{variable}' but no node with sanitized name '{prefix}' exists in the workflow"
    )]
    InvalidVariableReference {
        node_name: String,
        variable: String,
        prefix: String,
    },

    #[error("Condition in node '{0}' has an empty variable reference")]
    EmptyVariableReference(String),
}

pub fn validate_workflow(workflow: &Workflow) -> Result<(), ValidationError> {
    if workflow.nodes.is_empty() {
        return Err(ValidationError::NoNodes);
    }

    // Check for entry points: nodes with no incoming edges.
    // EndLoop back-edges to Loop nodes don't count as incoming edges.
    let targets_excluding_endloop_back: HashSet<Uuid> = workflow
        .edges
        .iter()
        .filter(|e| {
            // Exclude edges that originate from an EndLoop node and point to its paired Loop
            if let Some(node) = workflow.find_node(e.from)
                && let NodeType::EndLoop(params) = &node.node_type
                && e.to == params.loop_id
            {
                return false;
            }
            true
        })
        .map(|e| e.to)
        .collect();

    let entry_count = workflow
        .nodes
        .iter()
        .filter(|n| !targets_excluding_endloop_back.contains(&n.id))
        .count();
    if entry_count == 0 {
        return Err(ValidationError::NoEntryPoint);
    }
    if entry_count > 1 {
        return Err(ValidationError::MultipleEntryPoints(entry_count));
    }

    // Validate outgoing edges per node based on node type
    validate_outgoing_edges(workflow)?;

    // Validate loop pairing
    validate_loop_pairing(workflow)?;

    // Cycle detection: ignore edges originating from EndLoop nodes, then
    // do a standard DFS-based cycle check on the remaining graph.
    validate_no_illegal_cycles(workflow)?;

    validate_condition_variables(workflow)?;

    Ok(())
}

/// Validate that each node has the correct outgoing edges for its type.
fn validate_outgoing_edges(workflow: &Workflow) -> Result<(), ValidationError> {
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

/// Validate loop pairing:
/// - Every EndLoop.loop_id must reference a valid Loop node
/// - Every Loop node must have exactly one EndLoop referencing it
/// - EndLoop's outgoing edge must point to its loop_id
fn validate_loop_pairing(workflow: &Workflow) -> Result<(), ValidationError> {
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

/// Cycle detection that allows EndLoop→Loop back-edges.
///
/// We ignore all edges originating from EndLoop nodes when building the
/// adjacency graph, then run standard DFS cycle detection. EndLoop edges
/// are validated separately by `validate_loop_pairing`.
fn validate_no_illegal_cycles(workflow: &Workflow) -> Result<(), ValidationError> {
    // Build set of EndLoop node IDs
    let endloop_ids: HashSet<Uuid> = workflow
        .nodes
        .iter()
        .filter(|n| matches!(&n.node_type, NodeType::EndLoop(_)))
        .map(|n| n.id)
        .collect();

    // Build adjacency list, excluding edges from EndLoop nodes
    let mut adjacency: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for node in &workflow.nodes {
        adjacency.entry(node.id).or_default();
    }
    for edge in &workflow.edges {
        if !endloop_ids.contains(&edge.from) {
            adjacency.entry(edge.from).or_default().push(edge.to);
        }
    }

    // DFS cycle detection using white/gray/black coloring
    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    let mut color: HashMap<Uuid, Color> = workflow
        .nodes
        .iter()
        .map(|n| (n.id, Color::White))
        .collect();

    fn dfs(
        node: Uuid,
        adjacency: &HashMap<Uuid, Vec<Uuid>>,
        color: &mut HashMap<Uuid, Color>,
    ) -> bool {
        color.insert(node, Color::Gray);
        if let Some(neighbors) = adjacency.get(&node) {
            for &neighbor in neighbors {
                match color.get(&neighbor) {
                    Some(Color::Gray) => return true, // back edge = cycle
                    Some(Color::White) => {
                        if dfs(neighbor, adjacency, color) {
                            return true;
                        }
                    }
                    _ => {}
                }
            }
        }
        color.insert(node, Color::Black);
        false
    }

    for node in &workflow.nodes {
        if color.get(&node.id) == Some(&Color::White) && dfs(node.id, &adjacency, &mut color) {
            return Err(ValidationError::CycleDetected);
        }
    }

    Ok(())
}

/// Validate that variable references in conditions point to actual nodes.
///
/// For each Loop/If/Switch condition, extracts `Variable` references, splits
/// on the first `.` to get the node-name prefix, and checks that a node with
/// that sanitized name exists in the workflow.
fn validate_condition_variables(workflow: &Workflow) -> Result<(), ValidationError> {
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
    use super::*;
    use crate::{
        ClickParams, Condition, EndLoopParams, FindTextParams, IfParams, LiteralValue, LoopParams,
        NodeType, Operator, Position, SwitchCase, SwitchParams, TypeTextParams, ValueRef,
    };

    fn dummy_condition() -> Condition {
        Condition {
            left: ValueRef::Literal {
                value: LiteralValue::Bool { value: true },
            },
            operator: Operator::Equals,
            right: ValueRef::Literal {
                value: LiteralValue::Bool { value: true },
            },
        }
    }

    fn pos(x: f32, y: f32) -> Position {
        Position { x, y }
    }

    // --- Regression tests (existing behavior) ---

    #[test]
    fn test_validate_empty_workflow() {
        let wf = Workflow::default();
        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::NoNodes));
    }

    #[test]
    fn test_validate_no_entry_point() {
        let mut wf = Workflow::default();
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(0.0, 0.0));
        let b = wf.add_node(
            NodeType::TypeText(TypeTextParams::default()),
            pos(100.0, 0.0),
        );
        // Create edges so every node has an incoming edge (and it's a cycle)
        wf.add_edge(a, b);
        wf.add_edge(b, a);

        let err = validate_workflow(&wf).unwrap_err();
        // Could be NoEntryPoint or CycleDetected depending on check order
        assert!(
            matches!(err, ValidationError::NoEntryPoint)
                || matches!(err, ValidationError::CycleDetected)
        );
    }

    #[test]
    fn test_validate_multiple_entry_points() {
        let mut wf = Workflow::default();
        // Two disconnected nodes = two entry points
        wf.add_node(NodeType::Click(ClickParams::default()), pos(0.0, 0.0));
        wf.add_node(
            NodeType::TypeText(TypeTextParams::default()),
            pos(100.0, 0.0),
        );

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::MultipleEntryPoints(2)));
    }

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

    #[test]
    fn test_validate_valid_linear_workflow() {
        let mut wf = Workflow::default();
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(0.0, 0.0));
        let b = wf.add_node(
            NodeType::TypeText(TypeTextParams::default()),
            pos(100.0, 0.0),
        );
        wf.add_edge(a, b);

        assert!(validate_workflow(&wf).is_ok());
    }

    #[test]
    fn test_validate_single_node() {
        let mut wf = Workflow::default();
        wf.add_node(NodeType::Click(ClickParams::default()), pos(0.0, 0.0));

        assert!(validate_workflow(&wf).is_ok());
    }

    // --- If node tests ---

    #[test]
    fn test_validate_valid_if_workflow() {
        // If → (IfTrue) → A, If → (IfFalse) → B, both A and B → C (converge)
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
        // If with only IfTrue edge → MissingIfBranch
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

    // --- Loop tests ---

    #[test]
    fn test_validate_valid_loop() {
        // Loop → (LoopBody) → BodyNode → EndLoop → (back to Loop)
        // Loop → (LoopDone) → DoneNode
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
        // Loop with LoopBody only (no LoopDone) — workflow ends after loop
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
        // Loop node but no EndLoop referencing it → UnpairedLoop
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
        // EndLoop with loop_id pointing to a non-Loop node → InvalidEndLoopTarget
        let mut wf = Workflow::default();
        let regular = wf.add_node(NodeType::Click(ClickParams::default()), pos(0.0, 0.0));
        let end_loop = wf.add_node(
            NodeType::EndLoop(EndLoopParams { loop_id: regular }),
            pos(100.0, 0.0),
        );
        // regular → end_loop → regular (back-edge excluded), single entry point
        wf.add_edge(regular, end_loop);
        wf.add_edge(end_loop, regular);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::InvalidEndLoopTarget(_)));
    }

    #[test]
    fn test_validate_non_endloop_cycle_detected() {
        // A → B → A cycle (neither is EndLoop) → CycleDetected
        let mut wf = Workflow::default();
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(0.0, 0.0));
        let b = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let c = wf.add_node(NodeType::Click(ClickParams::default()), pos(200.0, 0.0));
        // c is the entry point, c → a → b → a (cycle)
        wf.add_edge(c, a);
        wf.add_edge(a, b);
        wf.add_edge(b, a);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::CycleDetected));
    }

    #[test]
    fn test_validate_valid_nested_loops() {
        // Outer Loop → (LoopBody) → Inner Loop → (LoopBody) → Node → Inner EndLoop → Inner Loop
        //                                        Inner Loop → (LoopDone) → Outer EndLoop → Outer Loop
        // Outer Loop → (LoopDone) → FinalNode
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

        // Inner body → inner end → inner loop (back-edge)
        wf.add_edge(inner_body, inner_end);
        wf.add_edge(inner_end, inner_loop);

        // Outer end → outer loop (back-edge)
        wf.add_edge(outer_end, outer_loop);

        assert!(validate_workflow(&wf).is_ok());
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
        // Two EndLoop nodes both referencing the same Loop → MultipleEndLoops
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
        // Connect done → end_loop_2 so it has an incoming edge (single entry point)
        wf.add_edge(done, end_loop_2);
        wf.add_edge(end_loop_2, loop_node);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::MultipleEndLoops(_)));
    }

    // --- Strict cardinality tests ---

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

    // --- Variable reference validation tests ---

    #[test]
    fn test_validate_loop_valid_variable_reference() {
        // Loop exit condition references "find_text.found" — a node named "Find Text" exists
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
        // Loop exit condition references "find_textt.found" — typo, no matching node
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
        // A variable name with no dot (no field) — prefix is the whole name,
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
        // A Loop condition that references its own Loop node name should fail —
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
