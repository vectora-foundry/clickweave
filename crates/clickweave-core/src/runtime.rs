//! Runtime context for workflow execution.
//!
//! Holds variables produced by node outputs and loop iteration counters.
//! Variables are global to the execution — a variable set inside a loop
//! is visible after the loop ends (no nested scoping).

use crate::output_schema::{ConditionValue, OutputRef};
use crate::{Condition, Operator};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

/// Runtime state maintained during workflow execution.
#[derive(Debug, Default)]
pub struct RuntimeContext {
    /// Variables produced by node outputs.
    /// Key format: "<auto_id>.<field>" (e.g., "find_text_1.found").
    pub variables: HashMap<String, Value>,

    /// Loop iteration counters. Key: Loop node UUID, Value: current iteration (0-indexed).
    pub loop_counters: HashMap<Uuid, u32>,
}

impl RuntimeContext {
    /// Create a new, empty runtime context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or update a variable.
    pub fn set_variable(&mut self, name: impl Into<String>, value: Value) {
        self.variables.insert(name.into(), value);
    }

    /// Remove all variables whose key starts with the given prefix.
    pub fn remove_variables_with_prefix(&mut self, prefix: &str) {
        let dot_prefix = format!("{}.", prefix);
        self.variables
            .retain(|k, _| !k.starts_with(&dot_prefix) && k != prefix);
    }

    /// Look up a variable by name.
    pub fn get_variable(&self, name: &str) -> Option<&Value> {
        self.variables.get(name)
    }

    /// Resolve an [`OutputRef`] to the stored variable value.
    pub fn resolve_output_ref(&self, output_ref: &OutputRef) -> Value {
        let key = format!("{}.{}", output_ref.node, output_ref.field);
        self.variables.get(&key).cloned().unwrap_or(Value::Null)
    }

    /// Resolve a [`ConditionValue`] to a concrete [`Value`].
    pub fn resolve_condition_value(&self, cv: &ConditionValue) -> Value {
        match cv {
            ConditionValue::Literal { value } => value.to_json_value(),
            ConditionValue::Ref(output_ref) => self.resolve_output_ref(output_ref),
        }
    }

    /// Evaluate a [`Condition`] against the current runtime state.
    /// Left is always an OutputRef, right is either a literal or another OutputRef.
    pub fn evaluate_condition(&self, condition: &Condition) -> bool {
        let left = self.resolve_output_ref(&condition.left);
        let right = self.resolve_condition_value(&condition.right);
        evaluate_operator(&condition.operator, &left, &right)
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Apply an operator to two resolved JSON values.
fn evaluate_operator(op: &Operator, left: &Value, right: &Value) -> bool {
    match op {
        Operator::Equals => values_equal(left, right),
        Operator::NotEquals => !values_equal(left, right),
        Operator::GreaterThan => compare_numbers(left, right, |l, r| l > r),
        Operator::LessThan => compare_numbers(left, right, |l, r| l < r),
        Operator::GreaterThanOrEqual => compare_numbers(left, right, |l, r| l >= r),
        Operator::LessThanOrEqual => compare_numbers(left, right, |l, r| l <= r),
        Operator::Contains => string_contains(left, right),
        Operator::NotContains => !string_contains(left, right),
        Operator::IsEmpty => is_empty(left),
        Operator::IsNotEmpty => !is_empty(left),
    }
}

/// A value is considered "empty" when it carries no meaningful content.
fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        // Booleans and numbers are never empty — they always carry a value.
        Value::Bool(_) | Value::Number(_) => false,
    }
}

/// Equality with light type coercion.
///
/// Coercion rules:
/// - `bool` == `"true"` / `"false"` (case-sensitive)
/// - `number` == `number` using f64 epsilon comparison
/// - `string` == `string` exact match
/// - `null` == `null`
/// - Mismatched types that don't match a coercion rule → `false`
fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        // null == null
        (Value::Null, Value::Null) => true,

        // string == string
        (Value::String(l), Value::String(r)) => l == r,

        // number == number (epsilon)
        (Value::Number(_), Value::Number(_)) => match (value_as_f64(left), value_as_f64(right)) {
            (Some(l), Some(r)) => (l - r).abs() < f64::EPSILON,
            _ => false,
        },

        // bool == bool
        (Value::Bool(l), Value::Bool(r)) => l == r,

        // bool <-> string coercion
        (Value::Bool(b), Value::String(s)) | (Value::String(s), Value::Bool(b)) => {
            if *b {
                s == "true"
            } else {
                s == "false"
            }
        }

        // number <-> string coercion (consistent with compare_numbers)
        (Value::Number(_), Value::String(_)) | (Value::String(_), Value::Number(_)) => {
            match (value_as_f64(left), value_as_f64(right)) {
                (Some(l), Some(r)) => (l - r).abs() < f64::EPSILON,
                _ => false,
            }
        }

        // Everything else (arrays, objects, mismatched primitives) → not equal.
        _ => false,
    }
}

/// Numeric comparison with extraction from Number or parseable String.
fn compare_numbers(left: &Value, right: &Value, cmp: impl Fn(f64, f64) -> bool) -> bool {
    match (value_as_f64(left), value_as_f64(right)) {
        (Some(l), Some(r)) => cmp(l, r),
        _ => false,
    }
}

/// Try to extract an f64 from a JSON value.
///
/// Works for `Value::Number` and `Value::String` that can be parsed as f64.
fn value_as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

/// Check whether `haystack` (as a string) contains `needle` (as a string).
///
/// Non-string values are converted via `Value::to_string()`.
fn string_contains(haystack: &Value, needle: &Value) -> bool {
    let h = match haystack {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let n = match needle {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    h.contains(&n)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LiteralValue;

    /// Helper: build an OutputRef.
    fn out_ref(node: &str, field: &str) -> OutputRef {
        OutputRef {
            node: node.to_string(),
            field: field.to_string(),
        }
    }

    /// Helper: build a ConditionValue literal bool.
    fn lit_bool(v: bool) -> ConditionValue {
        ConditionValue::Literal {
            value: LiteralValue::Bool { value: v },
        }
    }

    /// Helper: build a ConditionValue literal string.
    fn lit_str(s: &str) -> ConditionValue {
        ConditionValue::Literal {
            value: LiteralValue::String {
                value: s.to_string(),
            },
        }
    }

    /// Helper: build a ConditionValue literal number.
    fn lit_num(n: f64) -> ConditionValue {
        ConditionValue::Literal {
            value: LiteralValue::Number { value: n },
        }
    }

    #[test]
    fn equals_bool_true() {
        let mut ctx = RuntimeContext::new();
        ctx.set_variable("ft.found", Value::Bool(true));

        let cond = Condition {
            left: out_ref("ft", "found"),
            operator: Operator::Equals,
            right: lit_bool(true),
        };

        assert!(ctx.evaluate_condition(&cond));
    }

    #[test]
    fn equals_bool_false() {
        let mut ctx = RuntimeContext::new();
        ctx.set_variable("ft.found", Value::Bool(false));

        let cond = Condition {
            left: out_ref("ft", "found"),
            operator: Operator::Equals,
            right: lit_bool(true),
        };

        assert!(!ctx.evaluate_condition(&cond));
    }

    #[test]
    fn not_equals() {
        let mut ctx = RuntimeContext::new();
        ctx.set_variable("ai.status", Value::String("error".into()));

        let cond = Condition {
            left: out_ref("ai", "status"),
            operator: Operator::NotEquals,
            right: lit_str("ok"),
        };

        assert!(ctx.evaluate_condition(&cond));
    }

    #[test]
    fn greater_than() {
        let mut ctx = RuntimeContext::new();
        ctx.set_variable(
            "fi.confidence",
            Value::Number(serde_json::Number::from_f64(0.95).unwrap()),
        );

        let cond = Condition {
            left: out_ref("fi", "confidence"),
            operator: Operator::GreaterThan,
            right: lit_num(0.8),
        };

        assert!(ctx.evaluate_condition(&cond));
    }

    #[test]
    fn contains_string() {
        let mut ctx = RuntimeContext::new();
        ctx.set_variable("ft.text", Value::String("Login successful".into()));

        let cond = Condition {
            left: out_ref("ft", "text"),
            operator: Operator::Contains,
            right: lit_str("successful"),
        };

        assert!(ctx.evaluate_condition(&cond));
    }

    #[test]
    fn is_empty_null() {
        let ctx = RuntimeContext::new();

        let cond = Condition {
            left: out_ref("missing", "field"),
            operator: Operator::IsEmpty,
            right: lit_bool(true), // right side is ignored for IsEmpty
        };

        assert!(ctx.evaluate_condition(&cond));
    }

    #[test]
    fn is_not_empty_with_value() {
        let mut ctx = RuntimeContext::new();
        ctx.set_variable("ai.result", Value::String("data".into()));

        let cond = Condition {
            left: out_ref("ai", "result"),
            operator: Operator::IsNotEmpty,
            right: lit_bool(true), // right side is ignored for IsNotEmpty
        };

        assert!(ctx.evaluate_condition(&cond));
    }

    #[test]
    fn missing_variable_equals_null() {
        let ctx = RuntimeContext::new();

        let cond = Condition {
            left: out_ref("missing", "var"),
            operator: Operator::Equals,
            right: lit_str(""),
        };

        assert!(!ctx.evaluate_condition(&cond));
    }

    #[test]
    fn bool_string_coercion() {
        let mut ctx = RuntimeContext::new();
        ctx.set_variable("ft.found", Value::Bool(true));

        let cond = Condition {
            left: out_ref("ft", "found"),
            operator: Operator::Equals,
            right: lit_str("true"),
        };

        assert!(ctx.evaluate_condition(&cond));
    }

    #[test]
    fn number_string_coercion() {
        let mut ctx = RuntimeContext::new();
        ctx.set_variable(
            "ft.count",
            Value::Number(serde_json::Number::from_f64(42.0).unwrap()),
        );

        let cond = Condition {
            left: out_ref("ft", "count"),
            operator: Operator::Equals,
            right: lit_str("42"),
        };
        assert!(ctx.evaluate_condition(&cond));

        let cond2 = Condition {
            left: out_ref("ft", "count"),
            operator: Operator::Equals,
            right: lit_str("not_a_number"),
        };
        assert!(!ctx.evaluate_condition(&cond2));
    }

    #[test]
    fn variable_vs_variable_comparison() {
        let mut ctx = RuntimeContext::new();
        ctx.set_variable(
            "ft1.count",
            Value::Number(serde_json::Number::from_f64(5.0).unwrap()),
        );
        ctx.set_variable(
            "ft2.count",
            Value::Number(serde_json::Number::from_f64(3.0).unwrap()),
        );

        let cond = Condition {
            left: out_ref("ft1", "count"),
            operator: Operator::GreaterThan,
            right: ConditionValue::Ref(out_ref("ft2", "count")),
        };
        assert!(ctx.evaluate_condition(&cond));
    }

    #[test]
    fn loop_counter_tracking() {
        let mut ctx = RuntimeContext::new();
        let loop_id = Uuid::new_v4();

        assert_eq!(ctx.loop_counters.get(&loop_id), None);

        ctx.loop_counters.insert(loop_id, 0);
        assert_eq!(ctx.loop_counters[&loop_id], 0);

        *ctx.loop_counters.get_mut(&loop_id).unwrap() += 1;
        assert_eq!(ctx.loop_counters[&loop_id], 1);

        *ctx.loop_counters.get_mut(&loop_id).unwrap() += 1;
        assert_eq!(ctx.loop_counters[&loop_id], 2);
    }
}
