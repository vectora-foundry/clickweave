use crate::{Edge, Node, Workflow};
use std::collections::HashSet;
use uuid::Uuid;

/// Merge a patch into a workflow, producing the patched result.
///
/// Operations applied in order: remove nodes, apply updates, add nodes,
/// remove edges, add new edges. Edges pointing to/from removed nodes are
/// also removed automatically.
pub fn merge_patch_into_workflow(
    workflow: &Workflow,
    added_nodes: &[Node],
    removed_node_ids: &[Uuid],
    updated_nodes: &[Node],
    added_edges: &[Edge],
    removed_edges: &[Edge],
) -> Workflow {
    let removed_ids: HashSet<_> = removed_node_ids.iter().collect();

    let nodes: Vec<Node> = workflow
        .nodes
        .iter()
        .filter(|n| !removed_ids.contains(&n.id))
        .map(|n| {
            updated_nodes
                .iter()
                .find(|u| u.id == n.id)
                .cloned()
                .unwrap_or_else(|| n.clone())
        })
        .chain(added_nodes.iter().cloned())
        .collect();

    let edges: Vec<Edge> = workflow
        .edges
        .iter()
        .filter(|e| {
            !removed_edges
                .iter()
                .any(|r| e.from == r.from && e.to == r.to && e.output == r.output)
        })
        // Also remove edges pointing to/from removed nodes
        .filter(|e| !removed_ids.contains(&e.from) && !removed_ids.contains(&e.to))
        .cloned()
        .chain(added_edges.iter().cloned())
        .collect();

    let mut merged = Workflow {
        id: workflow.id,
        name: workflow.name.clone(),
        nodes,
        edges,
        groups: workflow.groups.clone(),
        next_id_counters: workflow.next_id_counters.clone(),
        auto_approve_resolutions: workflow.auto_approve_resolutions,
    };
    merged.fixup_auto_ids();
    merged
}

/// Splice inserted nodes before an anchor node.
///
/// Given a list of `(new_node_id, insert_before_id)` pairs in insertion order,
/// finds the predecessor of each anchor and rewires:
///   predecessor -> new_node -> anchor
///
/// For chained insertions (A insert_before X, B insert_before X),
/// they are chained: predecessor -> A -> B -> X.
///
/// Returns the additional edges to add and edges to remove.
pub fn splice_insert_before(
    workflow: &Workflow,
    insertions: &[(Uuid, Uuid)], // (new_node_id, insert_before_id)
) -> (Vec<Edge>, Vec<Edge>) {
    let mut add_edges = Vec::new();
    let mut remove_edges = Vec::new();

    if insertions.is_empty() {
        return (add_edges, remove_edges);
    }

    // Group insertions by anchor (preserving order)
    let anchor_id = insertions[0].1;
    let new_node_ids: Vec<Uuid> = insertions.iter().map(|(id, _)| *id).collect();

    // Find predecessor of anchor (unlabeled edge)
    let pred_edge = workflow
        .edges
        .iter()
        .find(|e| e.to == anchor_id && e.output.is_none());

    if let Some(pred) = pred_edge {
        // Remove predecessor -> anchor
        remove_edges.push(pred.clone());
        // predecessor -> first inserted
        add_edges.push(Edge {
            from: pred.from,
            to: new_node_ids[0],
            output: None,
        });
    }

    // Chain inserted nodes together
    for window in new_node_ids.windows(2) {
        add_edges.push(Edge {
            from: window[0],
            to: window[1],
            output: None,
        });
    }

    // Last inserted -> anchor
    add_edges.push(Edge {
        from: *new_node_ids.last().unwrap(),
        to: anchor_id,
        output: None,
    });

    (add_edges, remove_edges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_params::ClickParams;
    use crate::{NodeType, Position};

    fn make_node(name: &str) -> Node {
        Node {
            id: Uuid::new_v4(),
            name: name.to_string(),
            node_type: NodeType::Click(ClickParams {
                target: Some(crate::node_params::ClickTarget::Text {
                    text: name.to_string(),
                }),
                ..Default::default()
            }),
            position: Position { x: 0.0, y: 0.0 },
            auto_id: String::new(),
            enabled: true,
            timeout_ms: None,
            settle_ms: None,
            retries: 0,
            supervision_retries: 2,
            trace_level: Default::default(),
            role: Default::default(),
            expected_outcome: None,
        }
    }

    #[test]
    fn merge_updates_existing_node() {
        let node = make_node("Click A");
        let wf = Workflow {
            nodes: vec![node.clone()],
            ..Default::default()
        };
        let mut updated = node.clone();
        updated.name = "Click B".to_string();

        let result = merge_patch_into_workflow(&wf, &[], &[], &[updated], &[], &[]);
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "Click B");
    }

    #[test]
    fn merge_removes_node_and_orphaned_edges() {
        let a = make_node("A");
        let b = make_node("B");
        let edge = Edge {
            from: a.id,
            to: b.id,
            output: None,
        };
        let wf = Workflow {
            nodes: vec![a.clone(), b.clone()],
            edges: vec![edge],
            ..Default::default()
        };

        let result = merge_patch_into_workflow(&wf, &[], &[b.id], &[], &[], &[]);
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.edges.len(), 0); // edge to removed node also gone
    }

    #[test]
    fn splice_insert_before_single_node() {
        let a = make_node("A");
        let b = make_node("B");
        let new = make_node("New");
        let wf = Workflow {
            nodes: vec![a.clone(), b.clone()],
            edges: vec![Edge {
                from: a.id,
                to: b.id,
                output: None,
            }],
            ..Default::default()
        };

        let (add, remove) = splice_insert_before(&wf, &[(new.id, b.id)]);
        assert_eq!(remove.len(), 1); // A->B removed
        assert_eq!(remove[0].from, a.id);
        assert_eq!(add.len(), 2); // A->New, New->B
        assert_eq!(add[0].from, a.id);
        assert_eq!(add[0].to, new.id);
        assert_eq!(add[1].from, new.id);
        assert_eq!(add[1].to, b.id);
    }

    #[test]
    fn splice_insert_before_chained() {
        let a = make_node("A");
        let b = make_node("B");
        let n1 = make_node("N1");
        let n2 = make_node("N2");
        let wf = Workflow {
            nodes: vec![a.clone(), b.clone()],
            edges: vec![Edge {
                from: a.id,
                to: b.id,
                output: None,
            }],
            ..Default::default()
        };

        let (add, remove) = splice_insert_before(&wf, &[(n1.id, b.id), (n2.id, b.id)]);
        assert_eq!(remove.len(), 1);
        assert_eq!(add.len(), 3); // A->N1, N1->N2, N2->B
        assert_eq!(add[0].to, n1.id);
        assert_eq!(add[1].from, n1.id);
        assert_eq!(add[1].to, n2.id);
        assert_eq!(add[2].from, n2.id);
        assert_eq!(add[2].to, b.id);
    }
}
