//! `update_continuity_after_tool_success` mirrors AX and OCR
//! results into the source-agnostic `world_model.elements` field
//! so the renderer can print them uniformly.

use super::*;
use crate::agent::world_model::ObservedElement;

fn runner() -> StateRunner {
    StateRunner::new_for_test("test goal".to_string())
}

#[test]
fn take_ax_snapshot_populates_elements_with_ax_variants() {
    let mut r = runner();
    let body = "uid=a1g3 button \"Login\"\n  uid=a2g3 textbox \"Email\"\n";
    r.update_continuity_after_tool_success("take_ax_snapshot", body);
    let els = r.world_model.elements.as_ref().expect("elements populated");
    assert!(!els.value.is_empty(), "expected parsed AX elements");
    assert!(
        els.value
            .iter()
            .all(|e| matches!(e, ObservedElement::Ax(_))),
        "all elements must be Ax-variant"
    );
}

#[test]
fn take_ax_snapshot_with_empty_body_does_not_overwrite_elements() {
    let mut r = runner();
    // Pre-populate a CDP elements surface; an empty AX snapshot
    // should not clobber it (no `Ax` elements parsed).
    let cdp_match = clickweave_core::cdp::CdpFindElementMatch {
        uid: "d1".into(),
        role: "button".into(),
        label: "OK".into(),
        tag: "button".into(),
        disabled: false,
        parent_role: None,
        parent_name: None,
        ..Default::default()
    };
    r.world_model.elements = Some(crate::agent::world_model::Fresh {
        value: vec![ObservedElement::Cdp(cdp_match.clone())],
        written_at: 0,
        source: crate::agent::world_model::FreshnessSource::DirectObservation,
        ttl_steps: Some(2),
    });
    r.update_continuity_after_tool_success("take_ax_snapshot", "");
    let els = r.world_model.elements.as_ref().unwrap();
    assert!(matches!(els.value.first(), Some(ObservedElement::Cdp(_))));
}
