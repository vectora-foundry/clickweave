//! Ported verbatim from the legacy `resolve_cdp_target_tests`
//! for Task 3a.7.d. The legacy tests targeted
//! `AgentRunner::<B>::resolve_cdp_target`; here they call
//! `StateRunner::resolve_cdp_target` directly (no backend type
//! parameter on the new runner's associated fn).
use super::*;
use crate::executor::Mcp;
use clickweave_mcp::ToolCallResult;

/// MCP stub that panics on any call. Every test in this module
/// exercises paths (structured response, arguments-only) that must
/// not reach MCP — the panic proves those paths don't regress to
/// making extra round-trips.
struct UnusedMcp;

impl Mcp for UnusedMcp {
    async fn call_tool(
        &self,
        _name: &str,
        _arguments: Option<Value>,
    ) -> anyhow::Result<ToolCallResult> {
        panic!("resolve_cdp_target reached MCP on a fast-path case");
    }
    fn has_tool(&self, _name: &str) -> bool {
        false
    }
    fn tools_as_openai(&self) -> Vec<Value> {
        Vec::new()
    }
    async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

async fn resolve(arguments: Value, result_text: &str) -> Option<(String, Option<String>)> {
    StateRunner::resolve_cdp_target(&arguments, result_text, &UnusedMcp).await
}

#[tokio::test]
async fn structured_response_wins_over_pid_argument() {
    let arguments = serde_json::json!({ "pid": 16024 });
    let result_text = serde_json::json!({
        "app_name": "Signal",
        "pid": 16024,
        "bundle_id": "org.whispersystems.signal-desktop",
        "kind": "ElectronApp",
    })
    .to_string();
    let resolved = resolve(arguments, &result_text).await;
    assert_eq!(
        resolved,
        Some(("Signal".to_string(), Some("ElectronApp".to_string())))
    );
}

#[tokio::test]
async fn plain_text_response_falls_back_to_arguments_app_name() {
    let arguments = serde_json::json!({ "app_name": "Signal" });
    let resolved = resolve(arguments, "Window focused successfully").await;
    assert_eq!(resolved, Some(("Signal".to_string(), None)));
}

#[tokio::test]
async fn empty_app_name_in_structured_response_is_ignored() {
    let arguments = serde_json::json!({ "app_name": "Chrome" });
    let result_text = serde_json::json!({ "app_name": "", "pid": 0 }).to_string();
    let resolved = resolve(arguments, &result_text).await;
    assert_eq!(resolved, Some(("Chrome".to_string(), None)));
}

/// MCP stub that returns a fixed multi-text-block `list_apps` response.
/// Pins the contract that the `pid → list_apps` CDP resolution path
/// parses only the first text block: regression guard for a past bug
/// where joining blocks with `\n` broke serde_json parsing whenever a
/// server returned a JSON payload plus trailing prose.
struct MultiBlockListAppsMcp;

impl Mcp for MultiBlockListAppsMcp {
    async fn call_tool(
        &self,
        name: &str,
        _arguments: Option<Value>,
    ) -> anyhow::Result<ToolCallResult> {
        assert_eq!(name, "list_apps");
        Ok(ToolCallResult {
            content: vec![
                clickweave_mcp::ToolContent::Text {
                    text: r#"[{"name":"Signal","pid":16024}]"#.to_string(),
                },
                clickweave_mcp::ToolContent::Text {
                    text: "(rendered from cached process table)".to_string(),
                },
            ],
            is_error: None,
        })
    }
    fn has_tool(&self, name: &str) -> bool {
        name == "list_apps"
    }
    fn tools_as_openai(&self) -> Vec<Value> {
        Vec::new()
    }
    async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn pid_resolves_to_app_name_even_with_trailing_prose_block() {
    let arguments = serde_json::json!({ "pid": 16024 });
    let resolved = StateRunner::resolve_cdp_target(
        &arguments,
        "Window focused successfully",
        &MultiBlockListAppsMcp,
    )
    .await;
    assert_eq!(resolved, Some(("Signal".to_string(), None)));
}
