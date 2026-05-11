//! Ported verbatim from the focus_window skip guard section of the
//! legacy runner's observation-union tests for Task 3a.7.d. Exercises
//! `StateRunner::should_skip_focus_window` and its two sister
//! predicates (`is_synthetic_focus_skip`, `mcp_has_toolset`) against
//! the same matrix of kind / toolset / CDP-liveness / policy cases
//! the legacy `AgentRunner` suite pinned.
use super::*;
use clickweave_mcp::ToolCallResult;

/// Minimal `Mcp` stub used to exercise the focus_window skip guard.
/// Only `has_tool` is consulted by
/// [`StateRunner::should_skip_focus_window`] — `call_tool` /
/// `tools_as_openai` / `refresh_server_tool_list` are never reached
/// in these unit tests but must exist to satisfy the trait bound.
struct ToolsetStub {
    tools: Vec<String>,
}

impl ToolsetStub {
    fn with(tools: &[&str]) -> Self {
        Self {
            tools: tools.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl crate::executor::Mcp for ToolsetStub {
    async fn call_tool(
        &self,
        _name: &str,
        _arguments: Option<Value>,
    ) -> anyhow::Result<ToolCallResult> {
        unimplemented!("focus_window skip guard does not dispatch tools")
    }

    fn has_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t == name)
    }

    fn tools_as_openai(&self) -> Vec<Value> {
        Vec::new()
    }

    async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Fresh runner pre-seeded with one app/kind hint for guard tests.
fn runner_with_kind(app_name: &str, kind: &str) -> StateRunner {
    let mut runner = StateRunner::new_for_test("test-goal".to_string());
    runner.record_app_kind(app_name, kind);
    runner
}

const FULL_AX_TOOLSET: &[&str] = &["take_ax_snapshot", "ax_click", "ax_set_value", "ax_select"];

#[test]
fn mcp_has_toolset_requires_every_member() {
    // Missing even one member blocks the guard. The guard only fires
    // when the full macOS AX dispatch toolset is present; on Windows
    // and on older MCP servers the set is incomplete and
    // focus_window still matters.
    let mcp_full = ToolsetStub::with(FULL_AX_TOOLSET);
    assert!(mcp_has_toolset(&mcp_full, FULL_AX_TOOLSET));

    for (i, missing) in FULL_AX_TOOLSET.iter().enumerate() {
        let partial: Vec<&str> = FULL_AX_TOOLSET
            .iter()
            .enumerate()
            .filter_map(|(j, t)| (j != i).then_some(*t))
            .collect();
        let mcp = ToolsetStub::with(&partial);
        assert!(
            !mcp_has_toolset(&mcp, FULL_AX_TOOLSET),
            "toolset without {} must not count as full AX toolset",
            missing,
        );
    }
}

#[test]
fn should_skip_focus_window_fires_for_known_native_with_full_ax_toolset() {
    // Baseline happy path: MCP exposes the full AX toolset AND we've
    // already seen that the target is Native — suppress focus_window
    // to keep the user's foreground undisturbed.
    let runner = runner_with_kind("Calculator", "Native");
    let mcp = ToolsetStub::with(FULL_AX_TOOLSET);
    let args = serde_json::json!({"app_name": "Calculator"});
    assert_eq!(
        runner.should_skip_focus_window(&args, &mcp),
        Some(FocusSkipReason::AxAvailable),
    );
}

#[test]
fn should_skip_focus_window_defers_for_electron_or_chrome_without_live_cdp() {
    // Broader contract (see `should_skip_focus_window`): Electron /
    // Chrome apps DO qualify for the skip, but only after CDP is
    // live for that exact app. When no CDP session is bound yet,
    // the first `focus_window` call often precedes `cdp_connect`
    // and may be needed to bring the window front so the debug
    // port is discoverable. Without CDP live, the guard must defer
    // regardless of which dispatch toolset the MCP server exposes.
    //
    // NOTE: this test previously asserted that Electron / Chrome
    // apps were NEVER skipped. That narrower contract was relaxed
    // when CDP dispatch became the dominant path for these apps.
    // The test now covers the pre-CDP-connect half of the broader
    // contract; the post-CDP-connect half is covered by
    // `should_skip_focus_window_fires_for_electron_with_live_cdp`.
    // AX + CDP toolsets both present — the only thing missing is
    // the live CDP session, which is the point.
    let mcp = ToolsetStub::with(&[
        "take_ax_snapshot",
        "ax_click",
        "ax_set_value",
        "ax_select",
        "cdp_find_elements",
        "cdp_click",
    ]);
    for kind in ["ElectronApp", "ChromeBrowser"] {
        let runner = runner_with_kind("VSCode", kind);
        let args = serde_json::json!({"app_name": "VSCode"});
        assert!(
            runner.should_skip_focus_window(&args, &mcp).is_none(),
            "focus_window must NOT be skipped for kind={} without a live CDP session",
            kind,
        );
    }
}

/// Seed a runner with a kind hint AND an active CDP session bound
/// to the same app — the on-the-wire state the agent reaches after
/// `launch_app` + successful `cdp_connect`. Delegates to
/// [`StateRunner::seed_cdp_live_for_test`] so the "post-`on_cdp_connected`
/// state shape" has a single source of truth.
fn runner_with_kind_and_cdp(app_name: &str, kind: &str) -> StateRunner {
    let mut runner = StateRunner::new_for_test("test-goal".to_string());
    runner.seed_cdp_live_for_test(app_name, kind);
    runner
}

const FULL_CDP_TOOLSET: &[&str] = &["cdp_find_elements", "cdp_click"];

#[test]
fn should_skip_focus_window_fires_for_electron_with_live_cdp() {
    // CDP dispatch operates on backgrounded windows without stealing
    // focus, so once a session is live for the exact app, the real
    // `focus_window` is redundant and the guard must fire.
    let runner = runner_with_kind_and_cdp("Signal", "ElectronApp");
    let mcp = ToolsetStub::with(FULL_CDP_TOOLSET);
    let args = serde_json::json!({"app_name": "Signal"});
    assert_eq!(
        runner.should_skip_focus_window(&args, &mcp),
        Some(FocusSkipReason::CdpLive),
    );
}

#[test]
fn should_skip_focus_window_fires_for_chrome_browser_with_live_cdp() {
    // Same contract as the Electron path — ChromeBrowser targets
    // go through CDP and must be suppressed when a session is live.
    let runner = runner_with_kind_and_cdp("Google Chrome", "ChromeBrowser");
    let mcp = ToolsetStub::with(FULL_CDP_TOOLSET);
    let args = serde_json::json!({"app_name": "Google Chrome"});
    assert_eq!(
        runner.should_skip_focus_window(&args, &mcp),
        Some(FocusSkipReason::CdpLive),
    );
}

#[test]
fn should_skip_focus_window_defers_for_electron_when_cdp_not_connected() {
    // Kind hint + full CDP toolset but NO live session — the first
    // focus_window often precedes cdp_connect and may itself be
    // what brings the window front so the debug port is findable.
    // The guard must defer here.
    let runner = runner_with_kind("Signal", "ElectronApp");
    let mcp = ToolsetStub::with(FULL_CDP_TOOLSET);
    let args = serde_json::json!({"app_name": "Signal"});
    assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
}

#[test]
fn should_skip_focus_window_defers_for_electron_when_cdp_tools_missing() {
    // CDP is live but the MCP server does not advertise the CDP
    // dispatch toolset (older server, stripped build). Without
    // cdp_find_elements / cdp_click the agent cannot drive the
    // target via CDP, so coordinate-based tools — which DO need
    // focus — are the likely fallback. The guard must defer.
    let runner = runner_with_kind_and_cdp("Signal", "ElectronApp");
    // Only cdp_find_elements, missing cdp_click.
    let mcp = ToolsetStub::with(&["cdp_find_elements"]);
    let args = serde_json::json!({"app_name": "Signal"});
    assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
}

#[test]
fn should_skip_focus_window_defers_when_cdp_bound_to_other_app() {
    // A live CDP session bound to a different app must not authorize
    // a skip for this one — the name scope of `is_connected_to` is
    // load-bearing.
    let mut runner = StateRunner::new_for_test("test-goal".to_string());
    runner.record_app_kind("Signal", "ElectronApp");
    runner.cdp_state.set_connected("Slack", 0);
    let mcp = ToolsetStub::with(FULL_CDP_TOOLSET);
    let args = serde_json::json!({"app_name": "Signal"});
    assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
}

#[test]
fn should_skip_focus_window_defers_when_kind_unknown() {
    // First-ever focus: no prior probe / structured response, so we
    // can't classify the app. The task is explicit about erring on
    // the side of executing focus_window normally in this case —
    // breaking Electron / Windows workflows is strictly worse than
    // a single preserved focus-steal on the first call.
    let runner = StateRunner::new_for_test("test-goal".to_string());
    let mcp = ToolsetStub::with(FULL_AX_TOOLSET);
    let args = serde_json::json!({"app_name": "MysteryApp"});
    assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
}

#[test]
fn should_skip_focus_window_defers_when_ax_toolset_incomplete() {
    // Windows / older MCP servers surface only a partial toolset.
    // Without ax_click / ax_set_value / ax_select, the agent cannot
    // drive the target via AX and `focus_window` is still required.
    let runner = runner_with_kind("Calculator", "Native");
    // Only take_ax_snapshot — no dispatch primitives.
    let mcp = ToolsetStub::with(&["take_ax_snapshot"]);
    let args = serde_json::json!({"app_name": "Calculator"});
    assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
}

#[test]
fn should_skip_focus_window_requires_app_name_in_args() {
    // window_id / pid-only focus_window variants are ambiguous; we
    // can't map them to a recorded kind, so the guard must not
    // fire. resolve_cdp_target's list_apps / list_windows path
    // still runs the real tool, which is the correct behavior.
    let runner = runner_with_kind("Calculator", "Native");
    let mcp = ToolsetStub::with(FULL_AX_TOOLSET);
    let args = serde_json::json!({"window_id": 42});
    assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
}

#[test]
fn is_synthetic_focus_skip_matches_only_the_sentinels() {
    // Post-step bookkeeping gates CDP auto-connect and workflow-node
    // creation on this predicate — it must be tight enough that a
    // real focus_window success never masquerades as a skip, yet
    // match every FocusSkipReason variant so none of the runner's
    // suppressions leak into the workflow graph.
    for reason in FocusSkipReason::ALL {
        assert!(
            StateRunner::is_synthetic_focus_skip("focus_window", reason.llm_message()),
            "focus_window + {:?} message must register as synthetic skip",
            reason,
        );
        assert!(
            !StateRunner::is_synthetic_focus_skip("launch_app", reason.llm_message()),
            "non-focus_window tool with {:?} message must not register",
            reason,
        );
    }
    // Different result text — a real MCP success must not be
    // treated as skipped.
    assert!(!StateRunner::is_synthetic_focus_skip(
        "focus_window",
        "Window focused successfully",
    ));
}

#[test]
fn should_skip_focus_window_respects_allow_focus_window_policy() {
    // Policy takes precedence over every kind / toolset branch: when
    // `allow_focus_window == false`, the predicate must return the
    // policy sentinel even for cases that would otherwise defer
    // (unknown kind, missing toolset, missing app_name, CDP-not-live).
    // The returned skip text is the LLM-facing nudge toward AX / CDP
    // dispatch primitives.
    let mut runner = StateRunner::new(
        "test-goal".to_string(),
        AgentConfig {
            allow_focus_window: false,
            ..Default::default()
        },
    );
    let mcp_empty = ToolsetStub::with(&[]);

    // 1. Unknown app kind, empty toolset — would normally defer.
    let args_named = serde_json::json!({"app_name": "MysteryApp"});
    assert_eq!(
        runner.should_skip_focus_window(&args_named, &mcp_empty),
        Some(FocusSkipReason::PolicyDisabled),
    );

    // 2. Missing app_name (window_id / pid-only form) — the kind /
    // toolset branches always defer here, but policy overrides.
    let args_windowed = serde_json::json!({"window_id": 42});
    assert_eq!(
        runner.should_skip_focus_window(&args_windowed, &mcp_empty),
        Some(FocusSkipReason::PolicyDisabled),
    );

    // 3. Electron kind hint but no live CDP session — normally
    // defers because the first focus_window often precedes
    // cdp_connect. Policy overrides.
    runner.record_app_kind("Signal", "ElectronApp");
    let args_electron = serde_json::json!({"app_name": "Signal"});
    assert_eq!(
        runner.should_skip_focus_window(&args_electron, &mcp_empty),
        Some(FocusSkipReason::PolicyDisabled),
    );

    // 4. `new_for_test` opts allow_focus_window back in so the
    //    unit tests in this module exercise the kind/toolset
    //    branches without per-test opt-in; an unseeded fixture
    //    runner must defer on unknown kind.
    let test_default_runner = StateRunner::new_for_test("test-goal".to_string());
    assert!(
        test_default_runner
            .should_skip_focus_window(&args_named, &mcp_empty)
            .is_none(),
    );
}

#[test]
fn default_config_disables_focus_window_via_policy() {
    // Pins the production-default contract: `AgentConfig::default()`
    // must suppress every focus_window unconditionally. `new_for_test`
    // overrides this for the rest of the suite (see above).
    let runner = StateRunner::new("test-goal".to_string(), AgentConfig::default());
    let mcp = ToolsetStub::with(&[]);
    let args = serde_json::json!({"app_name": "AnyApp"});
    assert_eq!(
        runner.should_skip_focus_window(&args, &mcp),
        Some(FocusSkipReason::PolicyDisabled),
        "AgentConfig::default() must suppress focus_window unconditionally",
    );
}

#[test]
fn record_app_kind_overwrites_previous_value_for_same_app() {
    // Apps can transition between kinds across runs (e.g. a Chrome
    // profile that used to be launched plain and is now launched
    // with --remote-debugging-port). The latest hint must win so
    // the guard reflects the current lifecycle, not history.
    let mut runner = StateRunner::new_for_test("test-goal".to_string());
    runner.record_app_kind("Calculator", "Native");
    runner.record_app_kind("Calculator", "ElectronApp");
    let mcp = ToolsetStub::with(FULL_AX_TOOLSET);
    let args = serde_json::json!({"app_name": "Calculator"});
    // Electron now — guard must NOT fire.
    assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
}

#[test]
fn should_skip_focus_window_fires_cdp_attachable_for_electron_pre_connect() {
    // Pre-CDP-connect contract: kind is Electron / Chrome and the
    // server advertises `cdp_connect`. The post-tool hook will
    // auto-connect on its own — the real focus_window is
    // unnecessary and would only steal foreground in the meantime.
    for kind in ["ElectronApp", "ChromeBrowser"] {
        let runner = runner_with_kind("VSCode", kind);
        let mcp = ToolsetStub::with(&["cdp_connect"]);
        let args = serde_json::json!({"app_name": "VSCode"});
        assert_eq!(
            runner.should_skip_focus_window(&args, &mcp),
            Some(FocusSkipReason::CdpAttachable),
            "kind={kind} with cdp_connect advertised must trigger CdpAttachable",
        );
    }
}

#[test]
fn should_skip_focus_window_defers_for_electron_when_cdp_connect_missing() {
    // CDP-attachable arm requires the server to actually advertise
    // `cdp_connect`. Without it the post-tool hook cannot fire, so
    // the first focus_window may itself be needed to bring the
    // window front and the classifier must defer.
    let runner = runner_with_kind("VSCode", "ElectronApp");
    // FULL_CDP_TOOLSET does NOT include cdp_connect by design —
    // it is the dispatch toolset, not the lifecycle one.
    let mcp = ToolsetStub::with(FULL_CDP_TOOLSET);
    let args = serde_json::json!({"app_name": "VSCode"});
    assert!(runner.should_skip_focus_window(&args, &mcp).is_none());
}

#[test]
fn cdp_live_takes_precedence_over_cdp_attachable_for_same_app() {
    // When the session is live AND the server advertises
    // `cdp_connect`, the more specific `CdpLive` arm must fire —
    // the agent has the dispatch toolset, not just the connect
    // primitive. Order matters in the match: CdpLive first.
    let runner = runner_with_kind_and_cdp("Signal", "ElectronApp");
    // Both CDP dispatch AND cdp_connect advertised.
    let mcp = ToolsetStub::with(&["cdp_find_elements", "cdp_click", "cdp_connect"]);
    let args = serde_json::json!({"app_name": "Signal"});
    assert_eq!(
        runner.should_skip_focus_window(&args, &mcp),
        Some(FocusSkipReason::CdpLive),
    );
}
