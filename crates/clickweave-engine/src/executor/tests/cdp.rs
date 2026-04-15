use super::helpers::*;
use crate::executor::ExecutorError;
use clickweave_core::CdpTarget;

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
async fn resolve_cdp_element_uid_succeeds_with_single_match() {
    let exec = make_test_executor();
    let mcp = StubToolProvider::new();
    // cdp_list_pages (health check) then cdp_take_snapshot with exactly one match.
    mcp.push_text_response("1: https://example.com [selected]");
    mcp.push_text_response("[uid=\"a5\"] button \"Submit\"");

    let uid = exec
        .resolve_cdp_element_uid("Submit", &mcp)
        .await
        .expect("single match should resolve");
    assert_eq!(uid, "a5");
}

#[tokio::test]
async fn resolve_cdp_element_uid_reports_zero_matches() {
    let exec = make_test_executor();
    let mcp = StubToolProvider::new();
    // Health check + three snapshot attempts that never mention the target.
    mcp.push_text_response("1: https://example.com [selected]");
    for _ in 0..3 {
        mcp.push_text_response("[uid=\"a1\"] button \"Cancel\"");
    }

    let err = exec
        .resolve_cdp_element_uid("Submit", &mcp)
        .await
        .expect_err("missing element must fail");
    assert!(matches!(err, ExecutorError::Cdp(_)));
    assert!(err.to_string().contains("No matching element"));
}

#[tokio::test]
async fn resolve_cdp_element_uid_surfaces_ambiguous_candidates() {
    use crate::executor::CdpCandidate;

    let exec = make_test_executor();
    let mcp = StubToolProvider::new();
    // Health check + a snapshot with three buttons labelled "Save".
    mcp.push_text_response("1: https://example.com [selected]");
    mcp.push_text_response(concat!(
        "[uid=\"a1\"] button \"Save\"\n",
        "[uid=\"a2\"] button \"Save\"\n",
        "[uid=\"a3\"] button \"Save\"\n",
    ));

    let err = exec
        .resolve_cdp_element_uid("Save", &mcp)
        .await
        .expect_err("ambiguous match must fail loudly");

    let ExecutorError::CdpAmbiguousTarget { target, candidates } = err else {
        panic!("expected CdpAmbiguousTarget, got: {err:?}");
    };
    assert_eq!(target, "Save");
    assert_eq!(
        candidates,
        vec![
            CdpCandidate {
                uid: "a1".to_string(),
                snippet: "[uid=\"a1\"] button \"Save\"".to_string(),
            },
            CdpCandidate {
                uid: "a2".to_string(),
                snippet: "[uid=\"a2\"] button \"Save\"".to_string(),
            },
            CdpCandidate {
                uid: "a3".to_string(),
                snippet: "[uid=\"a3\"] button \"Save\"".to_string(),
            },
        ]
    );
}

#[tokio::test]
async fn execute_cdp_action_returns_resolver_error_when_target_missing() {
    // The deterministic executor no longer carries a silent native-click
    // fallback for elements absent from the CDP accessibility tree — the
    // multi-tier resolver that produced the `__native_at__:X:Y` sentinel
    // was removed, so `execute_cdp_action` must surface the resolver's
    // "no matching element" error as a normal `NodeFailed` instead of
    // quietly moving the physical mouse.
    use crate::executor::retry_context::RetryContext;
    use uuid::Uuid;

    let exec = make_test_executor();
    let mcp = StubToolProvider::new();
    // Health check + three snapshot attempts that never mention the target.
    mcp.push_text_response("1: https://example.com [selected]");
    for _ in 0..3 {
        mcp.push_text_response("[uid=\"a1\"] button \"Cancel\"");
    }

    let ctx = RetryContext::new();
    let err = exec
        .execute_cdp_action("click", Uuid::new_v4(), "Submit", &mcp, None, &ctx)
        .await
        .expect_err("missing element must propagate as an error");
    assert!(matches!(err, ExecutorError::Cdp(_)));
    assert!(err.to_string().contains("No matching element"));

    // No native click/move_mouse tool call should appear in the call log.
    let calls = mcp.take_calls();
    assert!(
        calls
            .iter()
            .all(|(name, _)| name != "click" && name != "move_mouse"),
        "execute_cdp_action must not silently fall back to native click: {calls:?}"
    );
}
