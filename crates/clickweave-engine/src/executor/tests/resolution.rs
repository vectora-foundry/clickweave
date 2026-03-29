use super::helpers::*;
use clickweave_core::{
    CdpClickParams, CdpPressKeyParams, CdpTarget, Node, NodeType, Position, RuntimeResolution,
    Workflow, WorkflowPatchCompact,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Runtime resolution patch application tests
// ---------------------------------------------------------------------------

/// Build a small workflow: A (CdpPressKey "Enter") → B (CdpType "test")
fn make_press_then_type_workflow() -> (Workflow, Uuid, Uuid) {
    let mut wf = Workflow::new("test-resolution");
    let a = wf.add_node(
        NodeType::CdpPressKey(CdpPressKeyParams {
            key: "Enter".to_string(),
            ..Default::default()
        }),
        Position { x: 0.0, y: 0.0 },
    );
    let b = wf.add_node(
        NodeType::CdpType(clickweave_core::CdpTypeParams {
            text: "test".to_string(),
            ..Default::default()
        }),
        Position { x: 100.0, y: 0.0 },
    );
    wf.add_edge(a, b);
    (wf, a, b)
}

#[test]
fn apply_resolution_patch_changes_node_type() {
    let (wf, press_key_id, _type_id) = make_press_then_type_workflow();
    let mut exec = make_executor_with_workflow(wf);

    // Simulate a resolution patch that changes CdpPressKey → CdpClick
    let updated_node = Node {
        id: press_key_id,
        name: "Click Note to Self".to_string(),
        node_type: NodeType::CdpClick(CdpClickParams {
            target: CdpTarget::ExactLabel("Note to Self".to_string()),
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
    };

    let patch = WorkflowPatchCompact {
        added_nodes: Vec::new(),
        removed_node_ids: Vec::new(),
        updated_nodes: vec![updated_node],
        added_edges: Vec::new(),
        removed_edges: Vec::new(),
    };

    exec.apply_resolution_patch(&patch);

    // Verify the node type was changed
    let node = exec
        .workflow
        .nodes
        .iter()
        .find(|n| n.id == press_key_id)
        .unwrap();
    assert_eq!(node.name, "Click Note to Self");
    assert!(
        matches!(&node.node_type, NodeType::CdpClick(p) if p.target.as_str() == "Note to Self"),
        "Expected CdpClick but got {:?}",
        node.node_type
    );
}

#[test]
fn apply_resolution_patch_preserves_edges() {
    let (wf, a_id, b_id) = make_press_then_type_workflow();
    let mut exec = make_executor_with_workflow(wf);

    // Update node A without touching edges
    let updated = Node {
        id: a_id,
        name: "Click target".to_string(),
        node_type: NodeType::CdpClick(CdpClickParams {
            target: CdpTarget::ExactLabel("target".to_string()),
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
    };

    exec.apply_resolution_patch(&WorkflowPatchCompact {
        added_nodes: Vec::new(),
        removed_node_ids: Vec::new(),
        updated_nodes: vec![updated],
        added_edges: Vec::new(),
        removed_edges: Vec::new(),
    });

    // Edge A→B should still exist
    assert_eq!(exec.workflow.edges.len(), 1);
    assert_eq!(exec.workflow.edges[0].from, a_id);
    assert_eq!(exec.workflow.edges[0].to, b_id);
}

#[tokio::test]
async fn request_resolution_returns_none_without_channel() {
    let (wf, a_id, _) = make_press_then_type_workflow();
    let exec = make_executor_with_workflow(wf);

    // No resolution_tx set — should return None
    let result = exec
        .request_resolution(
            a_id,
            "Press Enter",
            "press_key",
            "Enter",
            "no elements",
            None,
        )
        .await;
    assert!(result.is_none());
}

#[tokio::test]
async fn request_resolution_skips_rejected_target() {
    let (wf, a_id, _) = make_press_then_type_workflow();
    let mut exec = make_executor_with_workflow(wf);

    // Set up a resolution channel (but we won't read from it)
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    exec.resolution_tx = Some(tx);

    // Mark this (node, target) as rejected
    exec.rejected_resolutions
        .insert((a_id, "Enter".to_string()));

    // Should return None because the pair was previously rejected
    let result = exec
        .request_resolution(
            a_id,
            "Press Enter",
            "press_key",
            "Enter",
            "no elements",
            None,
        )
        .await;
    assert!(result.is_none());
}

#[tokio::test]
async fn request_resolution_sends_query_and_receives_response() {
    let (wf, a_id, _) = make_press_then_type_workflow();
    let mut exec = make_executor_with_workflow(wf);

    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    exec.resolution_tx = Some(tx);

    // Spawn a task that receives the query and responds
    let handle = tokio::spawn(async move {
        let query = rx.recv().await.expect("should receive a query");
        assert_eq!(query.node_id, a_id);
        assert_eq!(query.node_name, "Press Enter");
        assert_eq!(query.target, "Enter");

        // Respond with Updated — change PressKey to Click
        let _ = query
            .response_tx
            .send(RuntimeResolution::Updated(WorkflowPatchCompact {
                added_nodes: Vec::new(),
                removed_node_ids: Vec::new(),
                updated_nodes: vec![Node {
                    id: a_id,
                    name: "Click Note to Self".to_string(),
                    node_type: NodeType::CdpClick(CdpClickParams {
                        target: CdpTarget::ExactLabel("Note to Self".to_string()),
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
                }],
                added_edges: Vec::new(),
                removed_edges: Vec::new(),
            }));
    });

    let result = exec
        .request_resolution(
            a_id,
            "Press Enter",
            "press_key",
            "Enter",
            "no elements",
            None,
        )
        .await;

    handle.await.unwrap();

    // Should have received the Updated resolution
    assert!(
        matches!(&result, Some(RuntimeResolution::Updated(patch)) if patch.updated_nodes.len() == 1),
        "Expected Updated resolution, got {:?}",
        result
    );
}

/// Integration test: apply an Updated resolution and verify the executor's
/// workflow is mutated so a subsequent re-entry would use the new node type.
#[tokio::test]
async fn resolution_updated_changes_executor_workflow() {
    let (wf, a_id, b_id) = make_press_then_type_workflow();
    let mut exec = make_executor_with_workflow(wf);

    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    exec.resolution_tx = Some(tx);

    // Simulate the resolution callback + patch application flow
    let handle = tokio::spawn(async move {
        let query = rx.recv().await.unwrap();
        let _ = query
            .response_tx
            .send(RuntimeResolution::Updated(WorkflowPatchCompact {
                added_nodes: Vec::new(),
                removed_node_ids: Vec::new(),
                updated_nodes: vec![Node {
                    id: a_id,
                    name: "Click Note to Self".to_string(),
                    node_type: NodeType::CdpClick(CdpClickParams {
                        target: CdpTarget::ExactLabel("Note to Self".to_string()),
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
                }],
                added_edges: Vec::new(),
                removed_edges: Vec::new(),
            }));
    });

    let resolution = exec
        .request_resolution(a_id, "Press Enter", "press_key", "Enter", "inventory", None)
        .await
        .expect("should get resolution");

    handle.await.unwrap();

    // Apply the patch (this is what run_loop does after receiving Updated)
    if let RuntimeResolution::Updated(patch) = resolution {
        exec.apply_resolution_patch(&patch);
    }

    // Verify: node A is now CdpClick, not CdpPressKey
    let node_a = exec.workflow.nodes.iter().find(|n| n.id == a_id).unwrap();
    assert!(
        matches!(&node_a.node_type, NodeType::CdpClick(p) if p.target.as_str() == "Note to Self"),
        "Node A should be CdpClick after resolution, got {:?}",
        node_a.node_type
    );
    assert_eq!(node_a.name, "Click Note to Self");

    // Verify: edge A→B still exists
    let edge = exec.workflow.edges.iter().find(|e| e.from == a_id);
    assert!(edge.is_some(), "Edge A→B should survive the patch");
    assert_eq!(edge.unwrap().to, b_id);

    // Verify: node B is untouched
    let node_b = exec.workflow.nodes.iter().find(|n| n.id == b_id).unwrap();
    assert!(matches!(&node_b.node_type, NodeType::CdpType(_)));
}
