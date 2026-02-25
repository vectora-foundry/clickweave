use super::WorkflowExecutor;
use clickweave_core::{EdgeOutput, NodeType};
use clickweave_llm::ChatBackend;
use std::collections::HashSet;
use uuid::Uuid;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Find entry points: nodes with no incoming edges.
    /// EndLoop back-edges (edges where the source is an EndLoop node)
    /// are NOT counted as incoming edges — this prevents loops from breaking
    /// entry point detection.
    pub(crate) fn entry_points(&self) -> Vec<Uuid> {
        let endloop_nodes: HashSet<Uuid> = self
            .workflow
            .nodes
            .iter()
            .filter(|n| matches!(n.node_type, NodeType::EndLoop(_)))
            .map(|n| n.id)
            .collect();

        let targets: HashSet<Uuid> = self
            .workflow
            .edges
            .iter()
            .filter(|e| !endloop_nodes.contains(&e.from))
            .map(|e| e.to)
            .collect();

        self.workflow
            .nodes
            .iter()
            .filter(|n| !targets.contains(&n.id))
            .map(|n| n.id)
            .collect()
    }

    /// Follow the single outgoing edge from a regular node (output is None).
    pub(crate) fn follow_single_edge(&self, from: Uuid) -> Option<Uuid> {
        self.workflow
            .edges
            .iter()
            .find(|e| e.from == from && e.output.is_none())
            .map(|e| e.to)
    }

    /// Follow a specific labeled edge from a control flow node.
    pub(crate) fn follow_edge(&self, from: Uuid, output: &EdgeOutput) -> Option<Uuid> {
        self.workflow
            .edges
            .iter()
            .find(|e| e.from == from && e.output.as_ref() == Some(output))
            .map(|e| e.to)
    }

    /// Follow the "default" edge when a control flow node is disabled.
    /// Falls through to the non-executing branch: IfFalse, LoopDone, or
    /// the first available outgoing edge for Switch.
    pub(crate) fn follow_disabled_edge(&self, node_id: Uuid, node_type: &NodeType) -> Option<Uuid> {
        match node_type {
            NodeType::If(_) => self.follow_edge(node_id, &EdgeOutput::IfFalse),
            NodeType::Loop(_) => self.follow_edge(node_id, &EdgeOutput::LoopDone),
            NodeType::Switch(_) => self
                .follow_edge(node_id, &EdgeOutput::SwitchDefault)
                .or_else(|| {
                    // No default edge — pick the first case edge as fallthrough
                    self.workflow
                        .edges
                        .iter()
                        .find(|e| e.from == node_id && e.output.is_some())
                        .map(|e| e.to)
                }),
            // EndLoop and regular nodes: follow_single_edge is fine
            _ => self.follow_single_edge(node_id),
        }
    }
}
