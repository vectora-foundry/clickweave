use super::WorkflowExecutor;
use clickweave_llm::ChatBackend;
use std::collections::HashSet;
use uuid::Uuid;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Find entry points: nodes with no incoming edges.
    pub(crate) fn entry_points(&self) -> Vec<Uuid> {
        let targets: HashSet<Uuid> = self.workflow.edges.iter().map(|e| e.to).collect();

        self.workflow
            .nodes
            .iter()
            .filter(|n| !targets.contains(&n.id))
            .map(|n| n.id)
            .collect()
    }

    /// Follow the single outgoing edge from a node.
    pub(crate) fn follow_single_edge(&self, from: Uuid) -> Option<Uuid> {
        self.workflow
            .edges
            .iter()
            .find(|e| e.from == from)
            .map(|e| e.to)
    }

    /// Find the predecessor of a node by looking for an incoming edge.
    pub(crate) fn find_predecessor(&self, node_id: Uuid) -> Option<Uuid> {
        self.workflow
            .edges
            .iter()
            .find(|e| e.to == node_id)
            .map(|e| e.from)
    }
}
