//! B4: SupervisionPaused skill-scope golden test.
//! B5: ApprovalRequired ad-hoc-scope test — SKIPPED (no ApprovalRequired event
//!     type in Phase 1 codebase; `ExecutorEvent` has only `SupervisionPaused` /
//!     `SupervisionPassed`; this will be covered when Phase 1.E wires the
//!     ad-hoc approval event).
//!
//! B4 tests that `SafetyScope::Skill` carries the correct `skill_id`,
//! `section_id`, and `step_id` fields, and that `ExecutorEvent::SupervisionPaused`
//! wraps a `SafetyScope::Skill` value without loss.
//!
//! The runner does not yet emit `SupervisionPaused` automatically from
//! `run_skill_steps` (Phase 1.E wires that). This test validates the type
//! construction and pattern-matching shape that Phase 1.E will fill in,
//! plus a property-level assertion on `SafetyScope::AdHoc` for completeness.

use clickweave_core::SafetyScope;
use clickweave_engine::executor::ExecutorEvent;
use uuid::Uuid;

/// B4: A `SupervisionPaused` event with a `SafetyScope::Skill` scope correctly
/// carries `skill_id`, `section_id`, and `step_id` through the event envelope.
#[test]
fn supervision_paused_carries_skill_scope_fields() {
    let scope = SafetyScope::Skill {
        skill_id: "skl_test_001".to_string(),
        section_id: "sec_launch".to_string(),
        step_id: "s_003".to_string(),
    };

    let event = ExecutorEvent::SupervisionPaused {
        scope: scope.clone(),
        finding: "step uses launch_app which requires user approval".to_string(),
        screenshot: None,
    };

    match event {
        ExecutorEvent::SupervisionPaused {
            scope:
                SafetyScope::Skill {
                    skill_id,
                    section_id,
                    step_id,
                },
            finding,
            ..
        } => {
            assert_eq!(skill_id, "skl_test_001");
            assert_eq!(section_id, "sec_launch");
            assert_eq!(step_id, "s_003");
            assert!(finding.contains("launch_app"));
        }
        other => panic!("expected SupervisionPaused with Skill scope, got {other:?}"),
    }
}

/// Validate `SafetyScope::AdHoc` carries a stable `run_id` UUID.
/// B5 (the full ad-hoc agent-loop gating test) is deferred — no
/// `ApprovalRequired` event type exists in Phase 1.
#[test]
fn safety_scope_adhoc_carries_run_id() {
    let run_id = Uuid::new_v4();
    let scope = SafetyScope::AdHoc { run_id };

    match scope {
        SafetyScope::AdHoc { run_id: extracted } => {
            assert_eq!(extracted, run_id, "run_id round-trips through AdHoc scope");
        }
        other => panic!("expected AdHoc scope, got {other:?}"),
    }
}

/// Verify `SafetyScope::Skill` and `::AdHoc` are the only two variants and
/// they are mutually exclusive — no accidental match arms elided.
#[test]
fn safety_scope_variants_are_mutually_exclusive() {
    let skill_scope = SafetyScope::Skill {
        skill_id: "s".to_string(),
        section_id: "sec".to_string(),
        step_id: "step".to_string(),
    };
    let adhoc_scope = SafetyScope::AdHoc {
        run_id: Uuid::new_v4(),
    };

    let is_skill = matches!(skill_scope, SafetyScope::Skill { .. });
    let is_adhoc_for_skill = matches!(skill_scope, SafetyScope::AdHoc { .. });
    assert!(is_skill);
    assert!(!is_adhoc_for_skill);

    let is_adhoc = matches!(adhoc_scope, SafetyScope::AdHoc { .. });
    let is_skill_for_adhoc = matches!(adhoc_scope, SafetyScope::Skill { .. });
    assert!(is_adhoc);
    assert!(!is_skill_for_adhoc);
}
