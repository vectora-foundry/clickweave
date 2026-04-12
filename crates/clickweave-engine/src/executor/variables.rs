use super::WorkflowExecutor;
use clickweave_core::runtime::RuntimeContext;
use clickweave_core::{NodeRun, NodeType};
use clickweave_llm::ChatBackend;
use serde_json::Value;

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Store node outputs in RuntimeContext for condition evaluation.
    pub(crate) fn extract_and_store_variables(
        &mut self,
        auto_id: &str,
        node_result: &Value,
        node_type: &NodeType,
        node_run: Option<&NodeRun>,
    ) {
        self.context.set_variable(
            format!("{}.success", auto_id),
            serde_json::Value::Bool(true),
        );
        self.record_event(
            node_run,
            "variable_set",
            serde_json::json!({
                "variable": format!("{}.success", auto_id),
                "value": true,
            }),
        );
        let extracted =
            extract_result_variables(&mut self.context, auto_id, node_result, node_type);
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
///   (e.g. `.apps` for `FindApp`), plus `.result` = raw Value.
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

            // Synthesize .found/.count/.coordinates from a `matches` array
            // for FindText/FindImage whose MCP response is object-shaped.
            if matches!(node_type, NodeType::FindText(_) | NodeType::FindImage(_))
                && let Some(Value::Array(matches)) = map.get("matches")
            {
                set(
                    format!("{}.found", prefix),
                    Value::Bool(!matches.is_empty()),
                );
                set(
                    format!("{}.count", prefix),
                    Value::Number(serde_json::Number::from(matches.len())),
                );
                if let Some(Value::Object(first)) = matches.first() {
                    let coords = if let (Some(x), Some(y)) =
                        (first.get("screen_x"), first.get("screen_y"))
                    {
                        Some(serde_json::json!({"x": x, "y": y}))
                    } else if let Some(Value::Object(center)) = first.get("center") {
                        match (center.get("x"), center.get("y")) {
                            (Some(x), Some(y)) => Some(serde_json::json!({"x": x, "y": y})),
                            _ => None,
                        }
                    } else if let (Some(x), Some(y)) = (first.get("x"), first.get("y")) {
                        Some(serde_json::json!({"x": x, "y": y}))
                    } else {
                        None
                    };
                    if let Some(coords) = coords {
                        set(format!("{}.coordinates", prefix), coords);
                    }
                    // Synthesize .confidence from score or confidence
                    let confidence = first
                        .get("score")
                        .or_else(|| first.get("confidence"))
                        .cloned();
                    if let Some(c) = confidence {
                        set(format!("{}.confidence", prefix), c);
                    }
                }
            }
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
                // Store a coordinates object for FindText/FindImage results
                if let (Some(x), Some(y)) = (first.get("x"), first.get("y"))
                    && matches!(node_type, NodeType::FindText(_) | NodeType::FindImage(_))
                {
                    set(
                        format!("{}.coordinates", prefix),
                        serde_json::json!({"x": x, "y": y}),
                    );
                }
                // Synthesize .confidence from score or confidence for FindImage
                if matches!(node_type, NodeType::FindImage(_))
                    && let Some(score) = first.get("score").or_else(|| first.get("confidence"))
                {
                    set(format!("{}.confidence", prefix), score.clone());
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
            // Query nodes that fall through to string result had no matches
            if matches!(node_type, NodeType::FindText(_) | NodeType::FindImage(_)) {
                set(format!("{}.found", prefix), Value::Bool(false));
                set(format!("{}.count", prefix), serde_json::json!(0));
            }
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
/// For example, `FindApp` results get stored as `<prefix>.apps`.
fn array_alias_for_node_type(node_type: &NodeType) -> Option<&'static str> {
    match node_type {
        NodeType::FindApp(_) => Some("apps"),
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
        // .coordinates object from first match
        assert_eq!(
            ctx.get_variable("find_text.coordinates"),
            Some(&serde_json::json!({"x": 100, "y": 200}))
        );
        assert!(!vars.is_empty());
    }

    #[test]
    fn extract_variables_from_array_find_app() {
        let mut ctx = RuntimeContext::new();
        let result = serde_json::json!([{"name": "Safari", "id": 1}]);
        let node_type = NodeType::FindApp(clickweave_core::FindAppParams::default());
        extract_result_variables(&mut ctx, "find_app", &result, &node_type);

        assert_eq!(ctx.get_variable("find_app.found"), Some(&Value::Bool(true)));
        // .apps typed alias
        assert_eq!(ctx.get_variable("find_app.apps"), Some(&result));
    }

    #[test]
    fn extract_coordinates_for_find_image() {
        let mut ctx = RuntimeContext::new();
        let result = serde_json::json!([{"x": 50.5, "y": 75.0, "confidence": 0.95}]);
        let node_type = NodeType::FindImage(clickweave_core::FindImageParams::default());
        extract_result_variables(&mut ctx, "find_image", &result, &node_type);

        assert_eq!(
            ctx.get_variable("find_image.coordinates"),
            Some(&serde_json::json!({"x": 50.5, "y": 75.0}))
        );
    }

    #[test]
    fn extract_find_image_from_object_shaped_matches() {
        let mut ctx = RuntimeContext::new();
        // Real MCP find_image response: object with matches array
        let result = serde_json::json!({
            "matches": [
                {"screen_x": 120.0, "screen_y": 340.0, "score": 0.92},
                {"screen_x": 500.0, "screen_y": 600.0, "score": 0.71}
            ]
        });
        let node_type = NodeType::FindImage(clickweave_core::FindImageParams::default());
        extract_result_variables(&mut ctx, "find_image", &result, &node_type);

        assert_eq!(
            ctx.get_variable("find_image.found"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            ctx.get_variable("find_image.count"),
            Some(&Value::Number(serde_json::Number::from(2)))
        );
        assert_eq!(
            ctx.get_variable("find_image.coordinates"),
            Some(&serde_json::json!({"x": 120.0, "y": 340.0}))
        );
        assert_eq!(
            ctx.get_variable("find_image.confidence"),
            Some(&serde_json::json!(0.92))
        );
        // Top-level keys still extracted
        assert!(ctx.get_variable("find_image.matches").is_some());
        assert_eq!(ctx.get_variable("find_image.result"), Some(&result));
    }

    #[test]
    fn no_coordinates_for_find_app() {
        let mut ctx = RuntimeContext::new();
        let result = serde_json::json!([{"name": "Safari", "id": 1}]);
        let node_type = NodeType::FindApp(clickweave_core::FindAppParams::default());
        extract_result_variables(&mut ctx, "find_app", &result, &node_type);

        assert!(ctx.get_variable("find_app.coordinates").is_none());
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

    #[test]
    fn find_image_array_result_synthesizes_confidence_from_score_or_confidence_field() {
        // Both "score" and "confidence" field names should be recognized
        for (field, value) in [("score", 0.88), ("confidence", 0.95)] {
            let mut ctx = RuntimeContext::new();
            let result = serde_json::json!([{"x": 100.0, "y": 200.0, field: value}]);
            let node_type = NodeType::FindImage(clickweave_core::FindImageParams::default());
            extract_result_variables(&mut ctx, "find_image", &result, &node_type);

            assert_eq!(
                ctx.get_variable("find_image.confidence"),
                Some(&serde_json::json!(value)),
                "field: {field}",
            );
        }
    }

    #[test]
    fn find_text_string_result_synthesizes_found_false_and_count_zero() {
        let mut ctx = RuntimeContext::new();
        let result = Value::String("no matches found".into());
        let node_type = NodeType::FindText(clickweave_core::FindTextParams::default());
        extract_result_variables(&mut ctx, "find_text", &result, &node_type);

        assert_eq!(
            ctx.get_variable("find_text.result"),
            Some(&Value::String("no matches found".into()))
        );
        assert_eq!(
            ctx.get_variable("find_text.found"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            ctx.get_variable("find_text.count"),
            Some(&serde_json::json!(0))
        );
    }

    #[test]
    fn find_image_string_result_synthesizes_found_false_and_count_zero() {
        let mut ctx = RuntimeContext::new();
        let result = Value::String("image not found".into());
        let node_type = NodeType::FindImage(clickweave_core::FindImageParams::default());
        extract_result_variables(&mut ctx, "find_image", &result, &node_type);

        assert_eq!(
            ctx.get_variable("find_image.found"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            ctx.get_variable("find_image.count"),
            Some(&serde_json::json!(0))
        );
    }

    #[test]
    fn non_query_node_string_result_does_not_synthesize_found() {
        let mut ctx = RuntimeContext::new();
        let result = Value::String("screenshot taken".into());
        let node_type = NodeType::TakeScreenshot(clickweave_core::TakeScreenshotParams::default());
        extract_result_variables(&mut ctx, "screenshot", &result, &node_type);

        assert!(ctx.get_variable("screenshot.found").is_none());
        assert!(ctx.get_variable("screenshot.count").is_none());
    }
}
