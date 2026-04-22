//! Tests for the `ax.rs` executor helpers — descriptor/uid resolution over a
//! live snapshot and the `snapshot_expired` retry dance.

use super::helpers::*;
use crate::executor::ExecutorError;
use clickweave_core::AxTarget;
use clickweave_mcp::{ToolCallResult, ToolContent};
use uuid::Uuid;

fn ax_snapshot_result(text: &str) -> ToolCallResult {
    ToolCallResult {
        content: vec![ToolContent::Text {
            text: text.to_string(),
        }],
        is_error: None,
    }
}

fn ax_dispatch_success(dispatched_via: &str) -> ToolCallResult {
    ToolCallResult {
        content: vec![ToolContent::Text {
            text: format!(r#"{{"ok":true,"dispatched_via":"{dispatched_via}"}}"#),
        }],
        is_error: Some(false),
    }
}

fn ax_dispatch_error(code: &str, message: &str) -> ToolCallResult {
    ToolCallResult {
        content: vec![ToolContent::Text {
            text: format!(
                r#"{{"error":{{"code":"{code}","message":"{message}","fallback":null}}}}"#
            ),
        }],
        is_error: Some(true),
    }
}

#[tokio::test]
async fn resolve_ax_target_uid_descriptor_first_match() {
    let exec = make_test_executor();
    let mcp = StubToolProvider::new();
    mcp.push_response(ax_snapshot_result(concat!(
        "uid=a1g3 AXButton \"Submit\"\n",
        "uid=a2g3 AXButton \"Cancel\"\n",
    )));

    let uid = exec
        .resolve_ax_target_uid(
            &AxTarget::Descriptor {
                role: "AXButton".into(),
                name: "Submit".into(),
                parent_name: None,
            },
            &mcp,
        )
        .await
        .expect("descriptor should resolve");
    assert_eq!(uid, "a1g3");

    let calls = mcp.take_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "take_ax_snapshot");
}

#[tokio::test]
async fn resolve_ax_target_uid_descriptor_uses_parent_name_as_tiebreaker() {
    let exec = make_test_executor();
    let mcp = StubToolProvider::new();
    // Two-space-per-depth indentation matches
    // `native-devtools-mcp`'s `format_snapshot` output. Rust line
    // continuations (\<newline>) strip the following whitespace, so each
    // line is built as its own `concat!` entry with explicit leading
    // spaces.
    mcp.push_response(ax_snapshot_result(concat!(
        "uid=a1g1 AXWindow \"Settings\"\n",
        "  uid=a2g1 AXGroup \"Network\"\n",
        "    uid=a3g1 AXButton \"Apply\"\n",
        "  uid=a4g1 AXGroup \"Display\"\n",
        "    uid=a5g1 AXButton \"Apply\"\n",
    )));

    let uid = exec
        .resolve_ax_target_uid(
            &AxTarget::Descriptor {
                role: "AXButton".into(),
                name: "Apply".into(),
                parent_name: Some("Display".into()),
            },
            &mcp,
        )
        .await
        .expect("parent-qualified descriptor should resolve");
    assert_eq!(uid, "a5g1");
}

#[tokio::test]
async fn resolve_ax_target_uid_resolved_uid_found_in_snapshot() {
    let exec = make_test_executor();
    let mcp = StubToolProvider::new();
    mcp.push_response(ax_snapshot_result("uid=a42g7 AXButton \"OK\"\n"));

    let uid = exec
        .resolve_ax_target_uid(&AxTarget::ResolvedUid("a42g7".into()), &mcp)
        .await
        .expect("matching uid should be returned verbatim");
    assert_eq!(uid, "a42g7");
}

#[tokio::test]
async fn resolve_ax_target_uid_resolved_uid_missing_returns_not_found() {
    let exec = make_test_executor();
    let mcp = StubToolProvider::new();
    // Snapshot contains no matching uid — server has rolled generation.
    mcp.push_response(ax_snapshot_result("uid=a1g9 AXButton \"OK\"\n"));

    let err = exec
        .resolve_ax_target_uid(&AxTarget::ResolvedUid("a42g3".into()), &mcp)
        .await
        .expect_err("stale uid must fail cleanly");
    assert!(
        matches!(err, ExecutorError::AxNotFound { ref target } if target == "a42g3"),
        "expected AxNotFound for 'a42g3', got {err:?}"
    );
}

#[tokio::test]
async fn resolve_ax_target_uid_resolved_uid_empty_fails_validation() {
    let exec = make_test_executor();
    let mcp = StubToolProvider::new();
    // An empty snapshot would otherwise match zero entries — the validation
    // guard must fire before we even look.
    mcp.push_response(ax_snapshot_result(""));

    let err = exec
        .resolve_ax_target_uid(&AxTarget::ResolvedUid("".into()), &mcp)
        .await
        .expect_err("empty uid must be rejected");
    assert!(matches!(err, ExecutorError::Validation(_)));
}

#[tokio::test]
async fn resolve_and_ax_click_retries_on_snapshot_expired() {
    // Two snapshots (one per attempt) + two dispatch calls (first fails with
    // snapshot_expired, second succeeds).
    let mut exec = make_test_executor();
    let mcp = StubToolProvider::new();
    mcp.push_response(ax_snapshot_result("uid=a5g1 AXButton \"Go\"\n"));
    mcp.push_response(ax_dispatch_error(
        "snapshot_expired",
        "uid from prior generation",
    ));
    mcp.push_response(ax_snapshot_result("uid=a5g2 AXButton \"Go\"\n"));
    mcp.push_response(ax_dispatch_success("AXPress"));

    let mut retry_ctx = crate::executor::retry_context::RetryContext::new();
    let result = exec
        .resolve_and_ax_click(
            Uuid::new_v4(),
            &AxTarget::Descriptor {
                role: "AXButton".into(),
                name: "Go".into(),
                parent_name: None,
            },
            &mcp,
            None,
            &mut retry_ctx,
        )
        .await
        .expect("retry should succeed");
    // set_tool_result_and_parse parses JSON payloads, so the returned Value
    // is the decoded `{ ok, dispatched_via }` envelope.
    assert_eq!(result["ok"], true);
    assert_eq!(result["dispatched_via"], "AXPress");

    // Verify the call sequence: snap, ax_click (fail), snap, ax_click (ok).
    let calls = mcp.take_calls();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0].0, "take_ax_snapshot");
    assert_eq!(calls[1].0, "ax_click");
    assert_eq!(calls[2].0, "take_ax_snapshot");
    assert_eq!(calls[3].0, "ax_click");
    // Second call used the fresh-generation uid.
    assert_eq!(calls[3].1.as_ref().unwrap()["uid"], "a5g2");
}

#[tokio::test]
async fn resolve_and_ax_click_fails_on_terminal_error_code() {
    let mut exec = make_test_executor();
    let mcp = StubToolProvider::new();
    mcp.push_response(ax_snapshot_result("uid=a1g1 AXButton \"OK\"\n"));
    mcp.push_response(ax_dispatch_error(
        "not_dispatchable",
        "element does not support AXPress",
    ));

    let mut retry_ctx = crate::executor::retry_context::RetryContext::new();
    let err = exec
        .resolve_and_ax_click(
            Uuid::new_v4(),
            &AxTarget::Descriptor {
                role: "AXButton".into(),
                name: "OK".into(),
                parent_name: None,
            },
            &mcp,
            None,
            &mut retry_ctx,
        )
        .await
        .expect_err("not_dispatchable should surface immediately");
    match err {
        ExecutorError::AxDispatch {
            tool,
            code,
            fallback,
            ..
        } => {
            assert_eq!(tool, "ax_click");
            assert_eq!(code, "not_dispatchable");
            assert!(fallback.is_none());
        }
        other => panic!("expected AxDispatch, got {other:?}"),
    }
}

#[tokio::test]
async fn resolve_and_ax_set_value_sends_value_arg() {
    let mut exec = make_test_executor();
    let mcp = StubToolProvider::new();
    mcp.push_response(ax_snapshot_result("uid=a2g1 AXTextField \"Email\"\n"));
    mcp.push_response(ax_dispatch_success("AXSetAttributeValue"));

    let mut retry_ctx = crate::executor::retry_context::RetryContext::new();
    exec.resolve_and_ax_set_value(
        Uuid::new_v4(),
        &AxTarget::Descriptor {
            role: "AXTextField".into(),
            name: "Email".into(),
            parent_name: None,
        },
        "user@example.com",
        &mcp,
        None,
        &mut retry_ctx,
    )
    .await
    .expect("ax_set_value should succeed");

    let calls = mcp.take_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].0, "ax_set_value");
    let args = calls[1].1.as_ref().expect("args sent");
    assert_eq!(args["uid"], "a2g1");
    assert_eq!(args["value"], "user@example.com");
}

#[tokio::test]
async fn resolve_and_ax_select_omits_value_arg() {
    let mut exec = make_test_executor();
    let mcp = StubToolProvider::new();
    mcp.push_response(ax_snapshot_result(concat!(
        "uid=a1g1 AXOutline\n",
        "  uid=a2g1 AXRow \"Wi-Fi\"\n",
    )));
    mcp.push_response(ax_dispatch_success("AXSelectedRows"));

    let mut retry_ctx = crate::executor::retry_context::RetryContext::new();
    exec.resolve_and_ax_select(
        Uuid::new_v4(),
        &AxTarget::Descriptor {
            role: "AXRow".into(),
            name: "Wi-Fi".into(),
            parent_name: None,
        },
        &mcp,
        None,
        &mut retry_ctx,
    )
    .await
    .expect("ax_select should succeed");

    let calls = mcp.take_calls();
    assert_eq!(calls[1].0, "ax_select");
    let args = calls[1].1.as_ref().expect("args sent");
    assert_eq!(args["uid"], "a2g1");
    assert!(args.get("value").is_none());
}

#[tokio::test]
async fn resolve_ax_descriptor_matches_server_lowercase_role_form() {
    // Walkthrough capture stores descriptors with the raw macOS AX role
    // (`AXButton`), but `take_ax_snapshot` on the live server emits CDP-style
    // lowercase roles (`button`). The resolver must normalize and match
    // either form.
    let exec = make_test_executor();
    let mcp = StubToolProvider::new();
    mcp.push_response(ax_snapshot_result("uid=a1g3 button \"Save\"\n"));

    let uid = exec
        .resolve_ax_target_uid(
            &AxTarget::Descriptor {
                role: "AXButton".into(),
                name: "Save".into(),
                parent_name: None,
            },
            &mcp,
        )
        .await
        .expect("descriptor should match across role-form mismatch");
    assert_eq!(uid, "a1g3");
}

#[tokio::test]
async fn resolve_ax_descriptor_matches_when_both_sides_use_lowercase_role() {
    // Agent-loop enrichment stores descriptors using the snapshot's role
    // string verbatim (already lowercase). Same-form match must still work.
    let exec = make_test_executor();
    let mcp = StubToolProvider::new();
    mcp.push_response(ax_snapshot_result("uid=a1g3 button \"Save\"\n"));

    let uid = exec
        .resolve_ax_target_uid(
            &AxTarget::Descriptor {
                role: "button".into(),
                name: "Save".into(),
                parent_name: None,
            },
            &mcp,
        )
        .await
        .expect("descriptor should match same-form role");
    assert_eq!(uid, "a1g3");
}

#[tokio::test]
async fn resolve_ax_descriptor_does_not_match_value_attribute_as_name() {
    // An unlabeled textbox serializes as
    //   `uid=a1g1 textbox value="hello" focused`
    // — no quoted name. Resolving a descriptor with `name == "hello"` must
    // NOT succeed against that line (otherwise the descriptor becomes
    // value-dependent and breaks replay when the user edits the field).
    let exec = make_test_executor();
    let mcp = StubToolProvider::new();
    mcp.push_response(ax_snapshot_result(
        "uid=a1g1 textbox value=\"hello\" focused\n",
    ));

    let err = exec
        .resolve_ax_target_uid(
            &AxTarget::Descriptor {
                role: "textbox".into(),
                name: "hello".into(),
                parent_name: None,
            },
            &mcp,
        )
        .await
        .expect_err("value attribute must not be lifted as the descriptor name");
    assert!(matches!(err, ExecutorError::AxNotFound { .. }));
}
