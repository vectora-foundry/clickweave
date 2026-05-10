use super::*;
use crate::agent::trace_graph::TraceNodeKind;
use clickweave_core::{AxClickParams, AxSelectParams, AxSetValueParams, AxTarget, McpToolCallParams};

fn runner_with_snapshot(body: &str) -> StateRunner {
    use crate::agent::world_model::{AxSnapshotData, Fresh, FreshnessSource};
    let mut r = StateRunner::new_for_test("g".to_string());
    r.world_model.last_native_ax_snapshot = Some(Fresh {
        value: AxSnapshotData {
            snapshot_id: "a1g1".to_string(),
            element_count: 3,
            captured_at_step: 0,
            ax_tree_text: body.to_string(),
        },
        written_at: 0,
        source: FreshnessSource::DirectObservation,
        ttl_steps: None,
    });
    r
}

#[test]
fn enrich_ax_click_resolved_uid_to_descriptor() {
    let r = runner_with_snapshot("uid=a5g2 AXButton \"Continue\"\n");
    let mut nt = TraceNodeKind::AxClick(AxClickParams {
        target: AxTarget::ResolvedUid("a5g2".into()),
        ..Default::default()
    });
    r.enrich_ax_descriptor(&mut nt);
    match nt {
        TraceNodeKind::AxClick(p) => assert_eq!(
            p.target,
            AxTarget::Descriptor {
                role: "AXButton".into(),
                name: "Continue".into(),
                parent_name: None,
            }
        ),
        _ => panic!("expected AxClick"),
    }
}

#[test]
fn upgrade_preserves_parent_name_for_outline_rows() {
    // NSOutlineView rows often share (role, name) across sections, so
    // the parent anchor is what makes the descriptor unambiguous.
    let snapshot = concat!(
        "uid=a1g1 AXOutline \"Sidebar\"\n",
        "  uid=a2g1 AXGroup \"Network\"\n",
        "    uid=a3g1 AXRow \"Wi-Fi\"\n",
    );
    let r = runner_with_snapshot(snapshot);
    let mut nt = TraceNodeKind::AxSelect(AxSelectParams {
        target: AxTarget::ResolvedUid("a3g1".into()),
        ..Default::default()
    });
    r.enrich_ax_descriptor(&mut nt);
    match nt {
        TraceNodeKind::AxSelect(p) => assert_eq!(
            p.target,
            AxTarget::Descriptor {
                role: "AXRow".into(),
                name: "Wi-Fi".into(),
                parent_name: Some("Network".into()),
            }
        ),
        _ => panic!("expected AxSelect"),
    }
}

#[test]
fn enrich_preserves_value_on_ax_set_value() {
    let r = runner_with_snapshot("uid=a10g1 AXTextField \"Email\"\n");
    let mut nt = TraceNodeKind::AxSetValue(AxSetValueParams {
        target: AxTarget::ResolvedUid("a10g1".into()),
        value: "preserved".into(),
        ..Default::default()
    });
    r.enrich_ax_descriptor(&mut nt);
    match nt {
        TraceNodeKind::AxSetValue(p) => {
            assert_eq!(p.value, "preserved");
            assert_eq!(
                p.target,
                AxTarget::Descriptor {
                    role: "AXTextField".into(),
                    name: "Email".into(),
                    parent_name: None,
                }
            );
        }
        _ => panic!("expected AxSetValue"),
    }
}

#[test]
fn enrich_is_noop_when_uid_not_in_snapshot() {
    let r = runner_with_snapshot("uid=a1g1 AXButton \"Other\"\n");
    let mut nt = TraceNodeKind::AxClick(AxClickParams {
        target: AxTarget::ResolvedUid("a99g9".into()),
        ..Default::default()
    });
    r.enrich_ax_descriptor(&mut nt);
    match nt {
        TraceNodeKind::AxClick(p) => assert_eq!(p.target, AxTarget::ResolvedUid("a99g9".into())),
        _ => panic!("expected AxClick"),
    }
}

#[test]
fn enrich_is_noop_for_non_ax_nodes() {
    let r = runner_with_snapshot("uid=a1g1 AXButton \"X\"\n");
    let mut nt = TraceNodeKind::McpToolCall(McpToolCallParams {
        tool_name: "click".into(),
        arguments: serde_json::json!({}),
    });
    r.enrich_ax_descriptor(&mut nt);
    assert!(matches!(nt, TraceNodeKind::McpToolCall(_)));
}

#[test]
fn enrich_is_noop_when_no_snapshot_captured() {
    let r = StateRunner::new_for_test("g".to_string());
    let mut nt = TraceNodeKind::AxClick(AxClickParams {
        target: AxTarget::ResolvedUid("a5g2".into()),
        ..Default::default()
    });
    r.enrich_ax_descriptor(&mut nt);
    match nt {
        TraceNodeKind::AxClick(p) => assert_eq!(p.target, AxTarget::ResolvedUid("a5g2".into())),
        _ => panic!("expected AxClick"),
    }
}
