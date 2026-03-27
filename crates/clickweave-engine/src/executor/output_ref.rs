use clickweave_core::output_schema::OutputRef;
use clickweave_core::runtime::RuntimeContext;
use serde_json::Value;

use super::error::{ExecutorError, ExecutorResult};

/// Resolve an OutputRef against the runtime context. Fails if the variable is missing.
pub(crate) fn resolve_ref(ctx: &RuntimeContext, output_ref: &OutputRef) -> ExecutorResult<Value> {
    let value = ctx.resolve_output_ref(output_ref);
    if value.is_null() {
        let key = format!("{}.{}", output_ref.node, output_ref.field);
        Err(ExecutorError::VariableNotFound { reference: key })
    } else {
        Ok(value)
    }
}

/// Resolve an optional OutputRef — returns None if the ref is None, Some(value) if present.
#[allow(dead_code)]
pub(crate) fn resolve_optional_ref(
    ctx: &RuntimeContext,
    output_ref: &Option<OutputRef>,
) -> ExecutorResult<Option<Value>> {
    match output_ref {
        Some(r) => resolve_ref(ctx, r).map(Some),
        None => Ok(None),
    }
}

/// Coerce a Value to a String (for text/url params).
pub(crate) fn coerce_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Extract x/y coordinates from a Value (Object with x, y fields).
pub(crate) fn extract_coordinates(value: &Value) -> ExecutorResult<(f64, f64)> {
    let x = value
        .get("x")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| ExecutorError::InvalidCoordinates("missing x field".into()))?;
    let y = value
        .get("y")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| ExecutorError::InvalidCoordinates("missing y field".into()))?;
    Ok((x, y))
}
