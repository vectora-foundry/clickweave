use super::*;
use crate::agent::world_model::{AppKind, CdpPageState, FocusedApp, Fresh, FreshnessSource};
use clickweave_mcp::ToolCallResult;

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
        unimplemented!("coordinate guard predicate does not dispatch tools")
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

const AX_TOOLSET: &[&str] = &["take_ax_snapshot", "ax_click", "ax_set_value", "ax_select"];

fn focused(name: &str, kind: AppKind) -> Fresh<FocusedApp> {
    Fresh {
        value: FocusedApp {
            name: name.to_string(),
            kind,
            pid: 1,
        },
        written_at: 0,
        source: FreshnessSource::DirectObservation,
        ttl_steps: None,
    }
}

fn cdp_page(url: &str) -> Fresh<CdpPageState> {
    Fresh {
        value: CdpPageState {
            url: url.to_string(),
            page_fingerprint: "fp".to_string(),
            element_inventory: Vec::new(),
        },
        written_at: 0,
        source: FreshnessSource::DirectObservation,
        ttl_steps: None,
    }
}

#[test]
fn blocks_click_when_cdp_page_live_and_focus_is_electron() {
    let mut runner = StateRunner::new_for_test("g".to_string());
    runner.world_model.focused_app = Some(focused("Signal", AppKind::ElectronApp));
    runner.world_model.cdp_page = Some(cdp_page("https://signal/"));
    let mcp = ToolsetStub::with(&[]);
    let blocked = runner.coordinate_primitive_blocked("click", &mcp);
    assert!(blocked.is_some(), "click must be blocked under live CDP");
    let msg = blocked.unwrap();
    assert!(msg.contains("cdp_page"));
    assert!(msg.contains("cdp_click"));
    assert!(!msg.contains("cdp_evaluate_script"));
}

#[test]
fn blocks_each_coordinate_primitive_under_cdp() {
    let mut runner = StateRunner::new_for_test("g".to_string());
    runner.world_model.focused_app = Some(focused("Signal", AppKind::ElectronApp));
    runner.world_model.cdp_page = Some(cdp_page("https://signal/"));
    let mcp = ToolsetStub::with(&[]);
    for tool in [
        "click",
        "type_text",
        "press_key",
        "move_mouse",
        "scroll",
        "drag",
    ] {
        assert!(
            runner.coordinate_primitive_blocked(tool, &mcp).is_some(),
            "{tool} must be blocked when CDP is wired",
        );
    }
}

#[test]
fn does_not_block_observation_or_structured_tools_under_cdp() {
    let mut runner = StateRunner::new_for_test("g".to_string());
    runner.world_model.focused_app = Some(focused("Signal", AppKind::ElectronApp));
    runner.world_model.cdp_page = Some(cdp_page("https://signal/"));
    let mcp = ToolsetStub::with(&[]);
    for tool in [
        "find_text",
        "find_image",
        "element_at_point",
        "cdp_click",
        "ax_click",
        "take_screenshot",
    ] {
        assert!(
            runner.coordinate_primitive_blocked(tool, &mcp).is_none(),
            "{tool} must NOT be blocked — only coordinate primitives are",
        );
    }
}

#[test]
fn blocks_click_when_focus_is_native_and_ax_dispatch_wired() {
    let mut runner = StateRunner::new_for_test("g".to_string());
    runner.world_model.focused_app = Some(focused("Calculator", AppKind::Native));
    let mcp = ToolsetStub::with(AX_TOOLSET);
    let blocked = runner.coordinate_primitive_blocked("click", &mcp);
    assert!(blocked.is_some(), "click must be blocked under AX dispatch");
    let msg = blocked.unwrap();
    assert!(msg.contains("Native"));
    assert!(msg.contains("ax_click"));
}

#[test]
fn defers_when_focus_is_native_but_ax_toolset_partial() {
    let mut runner = StateRunner::new_for_test("g".to_string());
    runner.world_model.focused_app = Some(focused("Calculator", AppKind::Native));
    // Missing ax_set_value — partial toolset means agent cannot
    // drive via AX, so coordinate primitives remain a valid path.
    let mcp = ToolsetStub::with(&["take_ax_snapshot", "ax_click"]);
    assert!(runner.coordinate_primitive_blocked("click", &mcp).is_none());
}

#[test]
fn defers_when_no_focused_app() {
    let runner = StateRunner::new_for_test("g".to_string());
    // No focused_app set — caller has not yet observed which surface
    // is wired, so we cannot tell which family the agent should be
    // using and must fall through.
    let mcp = ToolsetStub::with(AX_TOOLSET);
    assert!(runner.coordinate_primitive_blocked("click", &mcp).is_none());
}

#[test]
fn defers_for_electron_focus_without_cdp_page() {
    // Electron is focused but no cdp_page yet (auto-connect hasn't
    // attached). Coordinate primitives are not yet redundant — the
    // agent may need them to bring the window front. Guard defers.
    let mut runner = StateRunner::new_for_test("g".to_string());
    runner.world_model.focused_app = Some(focused("Signal", AppKind::ElectronApp));
    let mcp = ToolsetStub::with(&["cdp_connect"]);
    assert!(runner.coordinate_primitive_blocked("click", &mcp).is_none());
}

#[test]
fn is_coordinate_primitive_includes_actions_excludes_observations() {
    for name in [
        "click",
        "type_text",
        "press_key",
        "move_mouse",
        "scroll",
        "drag",
    ] {
        assert!(is_coordinate_primitive(name), "{name} is a coord primitive");
    }
    for name in [
        "find_text",
        "find_image",
        "element_at_point",
        "take_screenshot",
        "ax_click",
        "cdp_click",
        "launch_app",
    ] {
        assert!(
            !is_coordinate_primitive(name),
            "{name} must NOT be classified as a coordinate primitive",
        );
    }
}
