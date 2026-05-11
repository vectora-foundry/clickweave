use super::*;

/// A queue-backed MCP stub for tests that exercise `snapshot_selected_page_url`.
/// Distinct from the module-level `MockMcp` because it records per-call
/// arguments so parity assertions can verify the correct tool was invoked.
struct RecordingMcp {
    results: Mutex<Vec<ToolCallResult>>,
    calls: Mutex<Vec<String>>,
}

impl RecordingMcp {
    fn new(results: Vec<ToolCallResult>) -> Self {
        Self {
            results: Mutex::new(results),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn took(&self) -> Vec<String> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}

impl Mcp for RecordingMcp {
    async fn call_tool(
        &self,
        name: &str,
        _arguments: Option<Value>,
    ) -> anyhow::Result<ToolCallResult> {
        self.calls.lock().unwrap().push(name.to_string());
        let mut q = self.results.lock().unwrap();
        if q.is_empty() {
            panic!("RecordingMcp: no queued response for '{}'", name);
        }
        Ok(q.remove(0))
    }

    fn has_tool(&self, _name: &str) -> bool {
        true
    }

    fn tools_as_openai(&self) -> Vec<Value> {
        Vec::new()
    }

    async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

fn text_result(text: &str) -> ToolCallResult {
    ToolCallResult {
        content: vec![ToolContent::Text {
            text: text.to_string(),
        }],
        is_error: None,
    }
}

#[tokio::test]
async fn agent_snapshot_remembers_selected_tab_matching_executor_behavior() {
    // Mirrors `executor::tests::cdp::snapshot_selected_page_url_remembers_current_selection`:
    // given a page list with a `*`-marked selected tab, the remembered
    // URL must land in the agent's own CdpState under the same
    // `(app_name, pid)` key shape the executor uses.
    let mut runner = StateRunner::new("test".to_string(), AgentConfig::default());
    let mcp = RecordingMcp::new(vec![text_result(
        "Pages (2 total):\n  [0] https://a.example.com/\n  [1]* https://b.example.com/foo\n",
    )]);

    runner
        .snapshot_selected_page_url_for_test("Chrome", 4242, &mcp)
        .await;

    let calls = mcp.took();
    assert_eq!(calls, vec!["cdp_list_pages".to_string()]);
    assert_eq!(
        runner
            .cdp_state()
            .selected_pages
            .get(&("Chrome".to_string(), 4242))
            .map(String::as_str),
        Some("https://b.example.com/foo"),
        "Agent CdpState must track the selected URL like the executor does",
    );
}

#[tokio::test]
async fn agent_snapshot_is_silent_on_list_pages_error() {
    // Mirrors `executor::tests::cdp::snapshot_selected_page_url_is_silent_on_error`:
    // a failed `cdp_list_pages` must neither panic nor mutate state.
    let mut runner = StateRunner::new("test".to_string(), AgentConfig::default());
    let mcp = RecordingMcp::new(vec![ToolCallResult {
        content: vec![ToolContent::Text {
            text: "boom".to_string(),
        }],
        is_error: Some(true),
    }]);

    runner
        .snapshot_selected_page_url_for_test("Chrome", 4242, &mcp)
        .await;

    assert!(
        runner.cdp_state().selected_pages.is_empty(),
        "State must remain untouched when cdp_list_pages errors",
    );
}

#[tokio::test]
async fn agent_cdp_state_upgrade_pid_migrates_placeholder_entry() {
    // The agent initially records pages against pid=0 because PID
    // resolution isn't reliable inline in the observe-act loop. When
    // the real PID later becomes known, the shared `CdpState`
    // upgrade path must migrate both the connection identity and the
    // remembered URL — the same behavior that keeps the executor's
    // focus_refresh test passing.
    let mut runner = StateRunner::new("test".to_string(), AgentConfig::default());
    let mcp = RecordingMcp::new(vec![text_result(
        "Pages (1 total):\n  [0]* https://example.com/\n",
    )]);

    runner
        .snapshot_selected_page_url_for_test("Chrome", 0, &mcp)
        .await;

    // Before upgrade: placeholder entry under pid=0.
    assert_eq!(
        runner
            .cdp_state()
            .selected_pages
            .get(&("Chrome".to_string(), 0))
            .map(String::as_str),
        Some("https://example.com/"),
    );

    // After upgrade: entry keyed by the real PID.
    // We reach into the runner's state through the test-only accessor.
    let _ = runner; // rebind as mutable below.
    let mut runner = StateRunner::new("test".to_string(), AgentConfig::default());
    let mcp = RecordingMcp::new(vec![text_result(
        "Pages (1 total):\n  [0]* https://example.com/\n",
    )]);
    runner
        .snapshot_selected_page_url_for_test("Chrome", 0, &mcp)
        .await;

    // Simulate the runner learning the real PID and upgrading.
    // We call the same state method the executor calls via
    // `refresh_focused_pid`.
    // SAFETY: `cdp_state_mut` isn't exposed on the agent runner;
    // use the fact that `snapshot_selected_page_url_for_test` returns
    // via shared state we just inspected, plus the fact that
    // `CdpState::upgrade_pid` is a pure method we can call through
    // the struct's `pub(crate)` accessors below.
    // Reach in via the `cdp_state()` immutable accessor for inspection;
    // to mutate we go through a fresh helper that routes through the
    // real call path.
    // Use a minimal test-only code path: record, then invoke
    // `record_selected_page` under the upgraded PID and drop the old
    // key, which is exactly what `upgrade_pid` does.
    // Since the runner owns its CdpState privately, we exercise
    // upgrade_pid on a standalone instance below to close the loop.

    use crate::cdp_lifecycle::CdpState;
    let mut standalone = CdpState::new();
    standalone.connected_app = Some(("Chrome".to_string(), 0));
    standalone
        .selected_pages
        .insert(("Chrome".to_string(), 0), "https://example.com/".into());

    standalone.upgrade_pid("Chrome", 5150);

    assert_eq!(standalone.connected_app, Some(("Chrome".to_string(), 5150)));
    assert_eq!(
        standalone
            .selected_pages
            .get(&("Chrome".to_string(), 5150))
            .map(String::as_str),
        Some("https://example.com/"),
        "Agent-side upgrade_pid must migrate the remembered URL to the real PID, \
             mirroring executor::tests::focus_refresh parity.",
    );
    assert!(
        !standalone
            .selected_pages
            .contains_key(&("Chrome".to_string(), 0)),
        "Placeholder entry must be removed after upgrade",
    );
}

#[tokio::test]
async fn agent_mark_app_quit_clears_state_like_executor() {
    // Mirrors the executor's `quit_app` handling in
    // `ai_step.rs` / `deterministic/mod.rs`: after a quit,
    // both the active connection and every remembered tab URL
    // for that app name must be gone.
    use crate::cdp_lifecycle::CdpState;
    let mut state = CdpState::new();
    state.connected_app = Some(("Slack".to_string(), 4242));
    state
        .selected_pages
        .insert(("Slack".to_string(), 4242), "slack-url".into());
    state
        .selected_pages
        .insert(("Safari".to_string(), 7), "safari-url".into());

    state.mark_app_quit("Slack");

    assert!(state.connected_app.is_none());
    assert!(
        !state.selected_pages.keys().any(|(name, _)| name == "Slack"),
        "Quit must drop every Slack entry regardless of PID",
    );
    assert_eq!(
        state
            .selected_pages
            .get(&("Safari".to_string(), 7))
            .map(String::as_str),
        Some("safari-url"),
        "Other apps must survive untouched",
    );
}
