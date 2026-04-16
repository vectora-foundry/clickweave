use super::helpers::*;
use crate::executor::ExecutorError;
use clickweave_core::{AppKind, CdpTarget};

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

    // Display must expose uids and target but not the raw snapshot snippets —
    // those are only reachable through the structured variant fields, so that
    // always-on log/UI surfaces stay free of live page DOM text.
    let display = err.to_string();
    assert!(
        display.contains("Save"),
        "display should mention target: {display}"
    );
    assert!(
        display.contains("3 candidates"),
        "display should mention candidate count: {display}"
    );
    for uid in ["a1", "a2", "a3"] {
        assert!(
            display.contains(uid),
            "display should list uid {uid}: {display}"
        );
    }
    assert!(
        !display.contains("button"),
        "display must not leak snippet tokens: {display}"
    );
    assert!(
        !display.contains("[uid="),
        "display must not leak snippet tokens: {display}"
    );

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

const TEST_PID: i32 = 4242;
const OTHER_PID: i32 = 4343;

fn chrome_key(pid: i32) -> (String, i32) {
    ("Chrome".to_string(), pid)
}

#[tokio::test]
async fn restore_or_record_selected_page_records_current_when_no_prior_url() {
    // First connect: no remembered URL. The helper should take whatever page
    // is currently selected and remember it so the next reconnect can
    // restore it.
    let mut exec = make_test_executor();
    let mcp = StubToolProvider::new();
    mcp.push_text_response(
        "Pages (2 total):\n  [0] https://a.example.com/\n  [1]* https://b.example.com/foo\n",
    );

    exec.restore_or_record_selected_page("Chrome", TEST_PID, &mcp)
        .await;

    let calls = mcp.take_calls();
    assert_eq!(calls.len(), 1, "only cdp_list_pages should fire: {calls:?}");
    assert_eq!(calls[0].0, "cdp_list_pages");
    assert_eq!(
        exec.cdp_selected_pages
            .get(&chrome_key(TEST_PID))
            .map(String::as_str),
        Some("https://b.example.com/foo")
    );
}

#[tokio::test]
async fn restore_or_record_selected_page_selects_matching_index_on_reconnect() {
    // Prior run remembered tab B. After reconnect, pages arrive in a
    // different order and a different tab is auto-selected — the helper
    // must call cdp_select_page with B's index.
    let mut exec = make_test_executor();
    exec.cdp_selected_pages.insert(
        chrome_key(TEST_PID),
        "https://b.example.com/foo".to_string(),
    );

    let mcp = StubToolProvider::new();
    // list_pages: [0] a (auto-selected by cdp_connect), [1] b (the user's tab)
    mcp.push_text_response(
        "Pages (2 total):\n  [0]* https://a.example.com/\n  [1] https://b.example.com/foo\n",
    );
    // cdp_select_page success
    mcp.push_text_response("Selected page [1]: https://b.example.com/foo");

    exec.restore_or_record_selected_page("Chrome", TEST_PID, &mcp)
        .await;

    let calls = mcp.take_calls();
    assert_eq!(calls.len(), 2, "list + select: {calls:?}");
    assert_eq!(calls[0].0, "cdp_list_pages");
    assert_eq!(calls[1].0, "cdp_select_page");
    assert_eq!(calls[1].1, Some(serde_json::json!({ "page_idx": 1 })));
    assert_eq!(
        exec.cdp_selected_pages
            .get(&chrome_key(TEST_PID))
            .map(String::as_str),
        Some("https://b.example.com/foo")
    );
}

#[tokio::test]
async fn restore_or_record_selected_page_isolates_same_name_instances_by_pid() {
    // Two Chrome instances with different PIDs must have independent
    // remembered tabs. A reconnect under one PID must not consume the
    // other PID's remembered URL or overwrite it.
    let mut exec = make_test_executor();
    exec.cdp_selected_pages.insert(
        chrome_key(OTHER_PID),
        "https://other-instance.example.com/".to_string(),
    );

    let mcp = StubToolProvider::new();
    mcp.push_text_response("Pages (1 total):\n  [0]* https://default-instance.example.com/\n");

    // Reconnect under TEST_PID (first connect for this instance).
    exec.restore_or_record_selected_page("Chrome", TEST_PID, &mcp)
        .await;

    let calls = mcp.take_calls();
    assert_eq!(
        calls.len(),
        1,
        "must not issue cdp_select_page using the other instance's URL: {calls:?}"
    );
    assert_eq!(calls[0].0, "cdp_list_pages");

    // Each instance owns its own entry.
    assert_eq!(
        exec.cdp_selected_pages
            .get(&chrome_key(TEST_PID))
            .map(String::as_str),
        Some("https://default-instance.example.com/")
    );
    assert_eq!(
        exec.cdp_selected_pages
            .get(&chrome_key(OTHER_PID))
            .map(String::as_str),
        Some("https://other-instance.example.com/")
    );
}

#[tokio::test]
async fn restore_or_record_selected_page_skips_select_when_already_on_target() {
    // Prior run remembered tab A, and the auto-select landed on A. Avoid
    // the redundant cdp_select_page call.
    let mut exec = make_test_executor();
    exec.cdp_selected_pages
        .insert(chrome_key(TEST_PID), "https://a.example.com/".to_string());

    let mcp = StubToolProvider::new();
    mcp.push_text_response(
        "Pages (2 total):\n  [0]* https://a.example.com/\n  [1] https://b.example.com/foo\n",
    );

    exec.restore_or_record_selected_page("Chrome", TEST_PID, &mcp)
        .await;

    let calls = mcp.take_calls();
    assert_eq!(
        calls.len(),
        1,
        "no redundant cdp_select_page expected: {calls:?}"
    );
    assert_eq!(calls[0].0, "cdp_list_pages");
}

#[tokio::test]
async fn restore_or_record_selected_page_falls_back_when_remembered_tab_closed() {
    // Prior run remembered a tab that no longer exists — the helper must
    // skip cdp_select_page entirely (no fabricated index) and fall back to
    // the auto-selected page.
    let mut exec = make_test_executor();
    exec.cdp_selected_pages.insert(
        chrome_key(TEST_PID),
        "https://gone.example.com/".to_string(),
    );

    let mcp = StubToolProvider::new();
    mcp.push_text_response(
        "Pages (2 total):\n  [0]* https://a.example.com/\n  [1] https://b.example.com/\n",
    );

    exec.restore_or_record_selected_page("Chrome", TEST_PID, &mcp)
        .await;

    let calls = mcp.take_calls();
    assert_eq!(
        calls.len(),
        1,
        "cdp_select_page must not be called with an arbitrary index: {calls:?}"
    );
    // Remembered URL should be updated to the auto-selected one so subsequent
    // reconnects aim at the actually-available tab.
    assert_eq!(
        exec.cdp_selected_pages
            .get(&chrome_key(TEST_PID))
            .map(String::as_str),
        Some("https://a.example.com/")
    );
}

#[tokio::test]
async fn restore_or_record_selected_page_tolerates_list_pages_error() {
    // A failed cdp_list_pages must not panic or update state — the caller
    // has already established the CDP connection; a bad list is strictly a
    // "keep the auto-selected tab" fallthrough.
    let mut exec = make_test_executor();
    exec.cdp_selected_pages
        .insert(chrome_key(TEST_PID), "https://a.example.com/".to_string());

    let mcp = StubToolProvider::new();
    mcp.push_response(clickweave_mcp::ToolCallResult {
        content: vec![clickweave_mcp::ToolContent::Text {
            text: "boom".to_string(),
        }],
        is_error: Some(true),
    });

    exec.restore_or_record_selected_page("Chrome", TEST_PID, &mcp)
        .await;

    let calls = mcp.take_calls();
    assert_eq!(calls.len(), 1);
    // Remembered URL is left untouched.
    assert_eq!(
        exec.cdp_selected_pages
            .get(&chrome_key(TEST_PID))
            .map(String::as_str),
        Some("https://a.example.com/")
    );
}

#[tokio::test]
async fn snapshot_selected_page_url_remembers_current_selection() {
    let mut exec = make_test_executor();
    let mcp = StubToolProvider::new();
    mcp.push_text_response(
        "Pages (2 total):\n  [0] https://a.example.com/\n  [1]* https://b.example.com/foo\n",
    );

    exec.snapshot_selected_page_url("Chrome", TEST_PID, &mcp)
        .await;

    assert_eq!(
        exec.cdp_selected_pages
            .get(&chrome_key(TEST_PID))
            .map(String::as_str),
        Some("https://b.example.com/foo")
    );
}

#[tokio::test]
async fn snapshot_selected_page_url_is_silent_on_error() {
    let mut exec = make_test_executor();
    let mcp = StubToolProvider::new();
    mcp.push_response(clickweave_mcp::ToolCallResult {
        content: vec![clickweave_mcp::ToolContent::Text {
            text: "boom".to_string(),
        }],
        is_error: Some(true),
    });

    exec.snapshot_selected_page_url("Chrome", TEST_PID, &mcp)
        .await;

    assert!(!exec.cdp_selected_pages.contains_key(&chrome_key(TEST_PID)));
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

// Electron CDP reconnect must NOT receive a Chrome-profile hint — otherwise
// `ensure_cdp_connected` skips the "reuse existing debug port" path and quits
// the Electron app that was already debug-attached during the walkthrough.
#[test]
fn resolve_chrome_profile_path_for_app_returns_none_for_electron() {
    let exec = make_test_executor_with_default_profile();

    let result = exec
        .resolve_chrome_profile_path_for_app(AppKind::ElectronApp, "Slack", None)
        .expect("electron resolution should not error");
    assert_eq!(
        result, None,
        "Electron apps must never get a Chrome-profile path, even when one is available",
    );
}

#[test]
fn resolve_chrome_profile_path_for_app_returns_none_for_native() {
    let exec = make_test_executor_with_default_profile();

    let result = exec
        .resolve_chrome_profile_path_for_app(AppKind::Native, "Calculator", None)
        .expect("native resolution should not error");
    assert_eq!(result, None);
}

#[test]
fn resolve_chrome_profile_path_for_app_returns_path_for_google_chrome() {
    let exec = make_test_executor_with_default_profile();

    let result = exec
        .resolve_chrome_profile_path_for_app(AppKind::ChromeBrowser, "Google Chrome", None)
        .expect("chrome resolution should not error");
    assert!(
        result.is_some(),
        "Google Chrome should fall back to the first available profile path"
    );
}

// Brave/Edge/Arc/Chromium are classified as ChromeBrowser for CDP purposes,
// but the Chrome profile tooling can only spawn Google Chrome. Passing a
// profile hint would disable debug-port reuse and force a spurious relaunch
// with the wrong binary.
#[test]
fn resolve_chrome_profile_path_for_app_returns_none_for_non_google_chrome_family() {
    let exec = make_test_executor_with_default_profile();

    for name in ["Brave Browser", "Microsoft Edge", "Arc", "Chromium"] {
        let result = exec
            .resolve_chrome_profile_path_for_app(AppKind::ChromeBrowser, name, None)
            .expect("chrome-family resolution should not error");
        assert_eq!(
            result, None,
            "{name} must not receive a Google-Chrome profile path",
        );
    }
}

#[test]
fn resolve_chrome_profile_path_for_app_propagates_unknown_profile_error_for_chrome() {
    let exec = make_test_executor_with_default_profile();

    let err = exec
        .resolve_chrome_profile_path_for_app(
            AppKind::ChromeBrowser,
            "Google Chrome",
            Some("no-such-profile"),
        )
        .expect_err("unknown Chrome profile must surface as an error");
    assert!(matches!(err, ExecutorError::ToolCall { .. }));
}

// Even when a bogus profile name is supplied for Electron (e.g. a stale field
// in a legacy workflow), the helper must ignore it — Electron has no profiles.
#[test]
fn resolve_chrome_profile_path_for_app_ignores_profile_name_for_electron() {
    let exec = make_test_executor_with_default_profile();

    let result = exec
        .resolve_chrome_profile_path_for_app(AppKind::ElectronApp, "Slack", Some("no-such-profile"))
        .expect("electron must not error on a stale Chrome-profile field");
    assert_eq!(result, None);
}
