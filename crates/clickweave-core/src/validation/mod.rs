mod cycles;
mod edges;
mod loops;
#[cfg(test)]
mod test_helpers;
mod variables;

use std::collections::HashSet;

use thiserror::Error;
use uuid::Uuid;

use crate::{NodeType, Workflow};

use cycles::validate_no_illegal_cycles;
use edges::validate_outgoing_edges;
use loops::validate_loop_pairing;
use variables::validate_condition_variables;

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

#[cfg(test)]
mod tests {
    use super::test_helpers::pos;
    use super::*;
    use crate::{ClickParams, NodeType, TypeTextParams, Workflow};

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
}
