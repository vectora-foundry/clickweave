use super::Mcp;
use super::{ExecutorResult, WorkflowExecutor};
use clickweave_llm::ChatBackend;
use serde_json::Value;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Execute a FindApp node: call list_apps, filter by search, return first match.
    pub(crate) async fn execute_find_app(
        &self,
        search: &str,
        mcp: &(impl Mcp + ?Sized),
    ) -> ExecutorResult<Value> {
        let result = mcp
            .call_tool("list_apps", Some(serde_json::json!({})))
            .await
            .map_err(|e| super::ExecutorError::ToolCall {
                tool: "list_apps".into(),
                message: e.to_string(),
            })?;
        Self::check_tool_error(&result, "list_apps")?;
        let text = crate::cdp_lifecycle::extract_text(&result);

        // Parse the list_apps output — it returns a list of app objects
        let apps: Value = serde_json::from_str(&text).unwrap_or(Value::Array(vec![]));
        let search_lower = search.to_lowercase();

        if let Some(apps_arr) = apps.as_array() {
            for app in apps_arr {
                let name = app.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if name.to_lowercase().contains(&search_lower) {
                    return Ok(serde_json::json!({
                        "found": true,
                        "name": name,
                        "pid": app.get("pid").and_then(|v| v.as_i64()).unwrap_or(0),
                    }));
                }
            }
        }

        Ok(serde_json::json!({"found": false, "name": "", "pid": 0}))
    }
}
