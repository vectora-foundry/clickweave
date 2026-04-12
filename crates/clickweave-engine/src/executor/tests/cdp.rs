use super::helpers::*;
use crate::executor::retry_context::RetryContext;
use crate::executor::{ExecutorError, ExecutorEvent};
use clickweave_core::CdpTarget;
use uuid::Uuid;

#[tokio::test]
async fn resolve_cdp_element_uid_rejects_empty_target() {
    let exec = make_test_executor();
    let mcp = StubToolProvider::new();

    let err = exec
        .resolve_cdp_element_uid("", &mcp)
        .await
        .expect_err("empty target must fail fast");
    assert!(matches!(err, ExecutorError::Cdp(_)));
    assert!(err.to_string().contains("empty"));

    // No MCP calls should have happened — the guard fires before any network I/O.
    assert!(mcp.take_calls().is_empty());
}

#[tokio::test]
async fn resolve_cdp_element_uid_rejects_whitespace_only_target() {
    let exec = make_test_executor();
    let mcp = StubToolProvider::new();

    let err = exec
        .resolve_cdp_element_uid("   \t\n", &mcp)
        .await
        .expect_err("whitespace-only target must fail fast");
    assert!(matches!(err, ExecutorError::Cdp(_)));
    assert!(mcp.take_calls().is_empty());
}

#[tokio::test]
async fn resolve_cdp_target_uid_passes_through_resolved_uid() {
    let exec = make_test_executor();
    let mcp = StubToolProvider::new();

    let uid = exec
        .resolve_cdp_target_uid(&CdpTarget::ResolvedUid("a7".to_string()), &mcp)
        .await
        .expect("ResolvedUid should pass through");
    assert_eq!(uid, "a7");
    assert!(mcp.take_calls().is_empty());
}

#[tokio::test]
async fn resolve_cdp_target_uid_passes_through_uid_shaped_label() {
    // Legacy workflows stored raw UIDs in the `uid` field which now deserialize
    // as ExactLabel("a5"). We must not try to snapshot-search for "a5".
    let exec = make_test_executor();
    let mcp = StubToolProvider::new();

    let uid = exec
        .resolve_cdp_target_uid(&CdpTarget::ExactLabel("a5".to_string()), &mcp)
        .await
        .expect("UID-shaped label should pass through");
    assert_eq!(uid, "a5");
    assert!(mcp.take_calls().is_empty());
}

#[tokio::test]
async fn resolve_cdp_target_uid_resolves_intent_via_snapshot() {
    let exec = make_test_executor();
    let mcp = StubToolProvider::new();
    // cdp_list_pages (health check) then cdp_take_snapshot returning a line
    // that matches "message input" and carries a UID.
    mcp.push_text_response("1: https://example.com [selected]");
    mcp.push_text_response("[uid=\"a9\"] textbox \"message input\"");

    let uid = exec
        .resolve_cdp_target_uid(&CdpTarget::Intent("message input".to_string()), &mcp)
        .await
        .expect("intent should resolve via snapshot");
    assert_eq!(uid, "a9");
    let calls = mcp.take_calls();
    assert_eq!(calls[0].0, "cdp_list_pages");
    assert_eq!(calls[1].0, "cdp_take_snapshot");
}

#[tokio::test]
async fn execute_cdp_action_emits_warning_on_native_fallback() {
    // When the CDP resolver returns a `__native_at__:X:Y` sentinel UID, the
    // executor silently swapped the CDP tool for a native click/move_mouse.
    // That made CDP nodes secretly run as native actions with no user-visible
    // signal. Verify the fallback now emits an `ExecutorEvent::Warning` and
    // forwards the native tool call.
    let (exec, mut rx) = make_test_executor_with_events();
    let mcp = StubToolProvider::new();

    // resolve_cdp_element_uid: cdp_list_pages health check, then snapshot
    // whose matching line carries the native-fallback sentinel as its UID.
    mcp.push_text_response("1: https://example.com [selected]");
    mcp.push_text_response("[uid=\"__native_at__:100:200\"] button \"Submit\"");
    // The subsequent native click call.
    mcp.push_text_response("clicked");

    let ctx = RetryContext::new();
    let out = exec
        .execute_cdp_action("click", Uuid::new_v4(), "Submit", &mcp, None, &ctx)
        .await
        .expect("native fallback should succeed");
    assert_eq!(out, "clicked");

    // The native click call should have gone through instead of cdp_click.
    let calls = mcp.take_calls();
    assert_eq!(calls[0].0, "cdp_list_pages");
    assert_eq!(calls[1].0, "cdp_take_snapshot");
    assert_eq!(calls[2].0, "click");
    assert_eq!(
        calls[2].1,
        Some(serde_json::json!({ "x": 100, "y": 200 })),
        "native click must receive the coordinates from the sentinel UID"
    );

    // A warning event should have been emitted so the UI can surface that
    // a CDP node silently ran as a native action.
    let mut saw_warning = false;
    while let Ok(event) = rx.try_recv() {
        if let ExecutorEvent::Warning(msg) = event {
            assert!(
                msg.contains("native click") && msg.contains("Submit"),
                "warning message should name the target and the native tool: {msg}"
            );
            saw_warning = true;
        }
    }
    assert!(
        saw_warning,
        "native fallback must emit ExecutorEvent::Warning"
    );
}

#[tokio::test]
async fn execute_cdp_action_uses_move_mouse_for_hover_fallback() {
    // The hover action must fall back to `move_mouse`, not `click`.
    let (exec, _rx) = make_test_executor_with_events();
    let mcp = StubToolProvider::new();
    mcp.push_text_response("1: https://example.com [selected]");
    mcp.push_text_response("[uid=\"__native_at__:42:99\"] link \"Tooltip Target\"");
    mcp.push_text_response("moved");

    let ctx = RetryContext::new();
    exec.execute_cdp_action("hover", Uuid::new_v4(), "Tooltip Target", &mcp, None, &ctx)
        .await
        .expect("hover fallback should succeed");

    let calls = mcp.take_calls();
    assert_eq!(calls[2].0, "move_mouse");
    assert_eq!(calls[2].1, Some(serde_json::json!({ "x": 42, "y": 99 })));
}
