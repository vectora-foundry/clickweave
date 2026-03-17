use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::{NodeType, Workflow};

use super::ValidationError;

/// Cycle detection that allows EndLoop->Loop back-edges.
///
/// We ignore all edges originating from EndLoop nodes when building the
/// adjacency graph, then run standard DFS cycle detection. EndLoop edges
/// are validated separately by `validate_loop_pairing`.
pub(crate) fn validate_no_illegal_cycles(workflow: &Workflow) -> Result<(), ValidationError> {
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

#[cfg(test)]
mod tests {
    use super::super::test_helpers::pos;
    use crate::{ClickParams, NodeType, Workflow};

    use super::super::ValidationError;
    use super::super::validate_workflow;

    #[test]
    fn test_validate_non_endloop_cycle_detected() {
        // A -> B -> A cycle (neither is EndLoop) -> CycleDetected
        let mut wf = Workflow::default();
        let a = wf.add_node(NodeType::Click(ClickParams::default()), pos(0.0, 0.0));
        let b = wf.add_node(NodeType::Click(ClickParams::default()), pos(100.0, 0.0));
        let c = wf.add_node(NodeType::Click(ClickParams::default()), pos(200.0, 0.0));
        // c is the entry point, c -> a -> b -> a (cycle)
        wf.add_edge(c, a);
        wf.add_edge(a, b);
        wf.add_edge(b, a);

        let err = validate_workflow(&wf).unwrap_err();
        assert!(matches!(err, ValidationError::CycleDetected));
    }
}
