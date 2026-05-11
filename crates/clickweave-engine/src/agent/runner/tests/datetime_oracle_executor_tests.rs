use super::*;
use crate::executor::Mcp;
use clickweave_mcp::ToolCallResult;
use serde_json::{Value, json};

struct PanicMcp;

impl Mcp for PanicMcp {
    async fn call_tool(
        &self,
        _name: &str,
        _arguments: Option<Value>,
    ) -> anyhow::Result<ToolCallResult> {
        panic!("date/time oracle must be answered by the harness before MCP dispatch");
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

#[tokio::test]
async fn get_current_datetime_is_intercepted_before_mcp() {
    let executor = McpToolExecutor { mcp: &PanicMcp };

    let body = executor
        .call_tool(crate::agent::time_oracle::TOOL_NAME, &json!({}))
        .await
        .expect("oracle response");
    let value: Value = serde_json::from_str(&body).expect("oracle JSON");

    assert_eq!(value["kind"], "current_datetime");
    assert_eq!(value["source"], "system_clock");
    assert!(
        value["utc_datetime"]
            .as_str()
            .is_some_and(|s| s.ends_with('Z'))
    );
    assert!(value["unix_millis"].as_i64().is_some());
    assert!(value["timezone"]["offset"].as_str().is_some());
}
