use super::WorkflowExecutor;
use clickweave_core::runtime::RuntimeContext;
use clickweave_core::{NodeRun, NodeType, sanitize_node_name};
use clickweave_llm::ChatBackend;
use serde_json::Value;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Store node outputs in RuntimeContext for condition evaluation.
    pub(crate) fn extract_and_store_variables(
        &mut self,
        node_name: &str,
        node_result: &Value,
        node_type: &NodeType,
        node_run: Option<&NodeRun>,
    ) {
        let sanitized = sanitize_node_name(node_name);
        self.context.set_variable(
            format!("{}.success", sanitized),
            serde_json::Value::Bool(true),
        );
        self.record_event(
            node_run,
            "variable_set",
            serde_json::json!({
                "variable": format!("{}.success", sanitized),
                "value": true,
            }),
        );
        let extracted =
            extract_result_variables(&mut self.context, &sanitized, node_result, node_type);
        for (var_name, var_value) in &extracted {
            self.record_event(
                node_run,
                "variable_set",
                serde_json::json!({
                    "variable": var_name,
                    "value": var_value,
                }),
            );
        }
    }
}

/// Extract type-specific variables from a tool result into the RuntimeContext.
///
/// Returns the list of `(variable_name, value)` pairs that were set, for tracing.
///
/// Contract:
/// - `.result` is always set as raw `Value` (JSON value for structured results,
///   string for text, empty string for null/AiStep).
/// - Objects: each top-level field -> `<prefix>.<key>`, plus `.result` = raw Value.
/// - Arrays: `.found` (bool), `.count`, first-element fields, plus typed alias
///   (e.g. `.windows` for `ListWindows`), plus `.result` = raw Value.
/// - Strings: `.result` only.
/// - Null: `.result = ""`.
pub(crate) fn extract_result_variables(
    ctx: &mut RuntimeContext,
    prefix: &str,
    result: &Value,
    node_type: &NodeType,
) -> Vec<(String, Value)> {
    let mut vars: Vec<(String, Value)> = Vec::new();

    let mut set = |name: String, value: Value| {
        ctx.set_variable(name.clone(), value.clone());
        vars.push((name, value));
    };

    match result {
        Value::Object(map) => {
            for (key, value) in map {
                set(format!("{}.{}", prefix, key), value.clone());
            }
            set(format!("{}.result", prefix), result.clone());
        }
        Value::Array(arr) => {
            let found = !arr.is_empty();
            set(format!("{}.found", prefix), Value::Bool(found));
            set(
                format!("{}.count", prefix),
                Value::Number(serde_json::Number::from(arr.len())),
            );
            if let Some(Value::Object(first)) = arr.first() {
                for (key, value) in first {
                    set(format!("{}.{}", prefix, key), value.clone());
                }
            }
            // Typed alias for the full array based on node type
            if let Some(alias) = array_alias_for_node_type(node_type) {
                set(format!("{}.{}", prefix, alias), result.clone());
            }
            set(format!("{}.result", prefix), result.clone());
        }
        Value::String(s) => {
            set(format!("{}.result", prefix), Value::String(s.clone()));
        }
        Value::Null => {
            set(format!("{}.result", prefix), Value::String(String::new()));
        }
        other => {
            set(format!("{}.result", prefix), other.clone());
        }
    }

    vars
}

/// Returns a typed alias name for array results based on node type.
///
/// For example, `ListWindows` results get stored as `<prefix>.windows`.
fn array_alias_for_node_type(node_type: &NodeType) -> Option<&'static str> {
    match node_type {
        NodeType::ListWindows(_) => Some("windows"),
        NodeType::FindText(_) | NodeType::FindImage(_) => Some("matches"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_variables_from_object() {
        let mut ctx = RuntimeContext::new();
        let result = serde_json::json!({"text": "Login", "x": 100.5, "y": 200.0});
        let node_type = NodeType::Click(clickweave_core::ClickParams::default());
        let vars = extract_result_variables(&mut ctx, "click", &result, &node_type);

        assert_eq!(
            ctx.get_variable("click.text"),
            Some(&Value::String("Login".into()))
        );
        assert!(ctx.get_variable("click.x").is_some());
        assert!(ctx.get_variable("click.y").is_some());
        // .result is the raw JSON Value
        assert_eq!(ctx.get_variable("click.result"), Some(&result));
        assert!(!vars.is_empty());
    }

    #[test]
    fn extract_variables_from_array_find_text() {
        let mut ctx = RuntimeContext::new();
        let result = serde_json::json!([
            {"text": "Login", "x": 100, "y": 200},
            {"text": "Logout", "x": 300, "y": 400}
        ]);
        let node_type = NodeType::FindText(clickweave_core::FindTextParams::default());
        let vars = extract_result_variables(&mut ctx, "find_text", &result, &node_type);

        assert_eq!(
            ctx.get_variable("find_text.found"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            ctx.get_variable("find_text.text"),
            Some(&Value::String("Login".into()))
        );
        assert_eq!(
            ctx.get_variable("find_text.count"),
            Some(&Value::Number(serde_json::Number::from(2)))
        );
        // .result is raw JSON Value (not stringified)
        assert_eq!(ctx.get_variable("find_text.result"), Some(&result));
        // .matches typed alias for the full array
        assert_eq!(ctx.get_variable("find_text.matches"), Some(&result));
        assert!(!vars.is_empty());
    }

    #[test]
    fn extract_variables_from_array_list_windows() {
        let mut ctx = RuntimeContext::new();
        let result = serde_json::json!([{"name": "Safari", "id": 1}]);
        let node_type = NodeType::ListWindows(clickweave_core::ListWindowsParams::default());
        extract_result_variables(&mut ctx, "list_windows", &result, &node_type);

        assert_eq!(
            ctx.get_variable("list_windows.found"),
            Some(&Value::Bool(true))
        );
        // .windows typed alias
        assert_eq!(ctx.get_variable("list_windows.windows"), Some(&result));
    }

    #[test]
    fn extract_variables_from_empty_array() {
        let mut ctx = RuntimeContext::new();
        let result = serde_json::json!([]);
        let node_type = NodeType::FindText(clickweave_core::FindTextParams::default());
        extract_result_variables(&mut ctx, "find_text", &result, &node_type);

        assert_eq!(
            ctx.get_variable("find_text.found"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            ctx.get_variable("find_text.count"),
            Some(&Value::Number(serde_json::Number::from(0)))
        );
    }

    #[test]
    fn extract_variables_from_string() {
        let mut ctx = RuntimeContext::new();
        let result = Value::String("screenshot taken".into());
        let node_type = NodeType::TakeScreenshot(clickweave_core::TakeScreenshotParams::default());
        extract_result_variables(&mut ctx, "screenshot", &result, &node_type);

        assert_eq!(
            ctx.get_variable("screenshot.result"),
            Some(&Value::String("screenshot taken".into()))
        );
    }

    #[test]
    fn extract_variables_null_sets_empty_result() {
        let mut ctx = RuntimeContext::new();
        let node_type = NodeType::Click(clickweave_core::ClickParams::default());
        let vars = extract_result_variables(&mut ctx, "node", &Value::Null, &node_type);
        assert_eq!(
            ctx.get_variable("node.result"),
            Some(&Value::String(String::new()))
        );
        assert_eq!(vars.len(), 1);
    }

    #[test]
    fn extract_variables_ai_step_returns_text() {
        let mut ctx = RuntimeContext::new();
        let result = Value::String("The login button is at the top right".into());
        let node_type = NodeType::AiStep(clickweave_core::AiStepParams::default());
        extract_result_variables(&mut ctx, "ai_step", &result, &node_type);

        assert_eq!(
            ctx.get_variable("ai_step.result"),
            Some(&Value::String(
                "The login button is at the top right".into()
            ))
        );
    }
}
