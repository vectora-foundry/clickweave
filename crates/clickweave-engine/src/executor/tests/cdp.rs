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
