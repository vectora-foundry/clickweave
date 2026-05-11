//! B6: Phase 1 destructive-tool gating golden test.
//!
//! Validates D32's fallback: `should_gate_step` correctly gates destructive
//! tools (from the static allow-list or with `destructive_hint = true`)
//! while passing through non-destructive tools.
//!
//! The actual suspension/pause loop (emitting `SupervisionPaused` and
//! blocking on user response) is Phase 1.E work. This test exercises the
//! `should_gate_step` decision function that Phase 1.E will call — the
//! gate predicate that separates "must pause" from "run through".
//!
//! NOTE: `run_skill_steps` currently passes `_requires_approval` as an
//! ignored parameter (Phase 1.E placeholder). The gating predicate
//! `should_gate_step` is tested here at the integration level to confirm
//! that a recorded skill's destructive step would gate while its
//! non-destructive step would not.

use clickweave_engine::agent::permissions::ToolAnnotations;
use clickweave_engine::executor::skill_runner::should_gate_step;

// ── Static-list gating (phase1_static_approvals feature) ────────────────────

/// A `launch_app` tool (in CONFIRMABLE_TOOLS) with no explicit override
/// and no annotations: must gate.
#[cfg(feature = "phase1_static_approvals")]
#[test]
fn launch_app_gates_on_empty_replay_with_no_annotations() {
    let annotations = ToolAnnotations::default();
    assert!(
        should_gate_step("launch_app", None, &annotations),
        "launch_app must gate under D32 fallback (static list)"
    );
}

/// `quit_app` is also in CONFIRMABLE_TOOLS.
#[cfg(feature = "phase1_static_approvals")]
#[test]
fn quit_app_gates_on_empty_replay_with_no_annotations() {
    let annotations = ToolAnnotations::default();
    assert!(
        should_gate_step("quit_app", None, &annotations),
        "quit_app must gate under D32 fallback (static list)"
    );
}

/// A non-destructive tool with no annotations and not in CONFIRMABLE_TOOLS
/// does NOT gate.
#[cfg(feature = "phase1_static_approvals")]
#[test]
fn take_screenshot_does_not_gate_with_no_annotations() {
    let annotations = ToolAnnotations::default();
    assert!(
        !should_gate_step("take_screenshot", None, &annotations),
        "take_screenshot must not gate — not in static list and no destructive hint"
    );
}

/// A tool with `destructive_hint = Some(true)` gates regardless of
/// static list membership.
#[test]
fn custom_destructive_tool_gates_via_annotation() {
    let annotations = ToolAnnotations {
        destructive_hint: Some(true),
        ..ToolAnnotations::default()
    };
    assert!(
        should_gate_step("custom_file_delete", None, &annotations),
        "any tool with destructive_hint=true must gate"
    );
}

/// Explicit `requires_approval = Some(false)` bypasses even a tool on
/// the static list (explicit always wins over heuristic).
#[cfg(feature = "phase1_static_approvals")]
#[test]
fn explicit_false_bypasses_static_list_tool() {
    let annotations = ToolAnnotations::default();
    assert!(
        !should_gate_step("launch_app", Some(false), &annotations),
        "explicit false must bypass static-list gating"
    );
}

/// Explicit `requires_approval = Some(true)` gates even a non-destructive
/// tool not in any list.
#[test]
fn explicit_true_gates_non_destructive_tool() {
    let annotations = ToolAnnotations {
        destructive_hint: Some(false),
        ..ToolAnnotations::default()
    };
    assert!(
        should_gate_step("take_screenshot", Some(true), &annotations),
        "explicit true must gate regardless of annotations or list membership"
    );
}

// ── End-to-end discrimination: destructive vs non-destructive in one skill ───

/// Simulate the discrimination a Phase 1.E runner would apply to a two-step
/// skill: one destructive (launch_app) and one non-destructive (take_screenshot).
/// The destructive step would pause; the non-destructive step would run through.
#[cfg(feature = "phase1_static_approvals")]
#[test]
fn skill_with_one_destructive_and_one_safe_step_gates_only_destructive() {
    let no_annotations = ToolAnnotations::default();

    let steps = vec![
        ("s_001", "launch_app"),      // destructive → should gate
        ("s_002", "take_screenshot"), // safe → should not gate
    ];

    let gating: Vec<(&str, bool)> = steps
        .iter()
        .map(|(id, tool)| (*id, should_gate_step(tool, None, &no_annotations)))
        .collect();

    assert_eq!(gating[0], ("s_001", true), "launch_app must gate");
    assert_eq!(gating[1], ("s_002", false), "take_screenshot must not gate");
}
