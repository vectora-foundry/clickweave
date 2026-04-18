//! Typed wrapper over an MCP tool's textual result plus its JSON parse.
//!
//! `raw_text` always carries the concatenated MCP text blocks (empty
//! string for null results); `parsed` carries the optional JSON
//! interpretation. Callers that need structured variable extraction
//! consult `parsed`; callers that want the original text (supervision
//! prompts, logging) reach for `raw_text` directly.
//!
//! [`Self::into_value`] preserves the wire-compatible projection used
//! throughout the executor: empty → `Value::Null`, non-JSON → `Value::
//! String`, valid JSON → the parsed value. Downstream consumers of
//! `execute_deterministic`'s return value depend on that exact shape.

use serde_json::Value;

/// The textual side of an MCP tool call plus the optional JSON parse.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolResult {
    raw_text: String,
    parsed: Option<Value>,
}

impl ToolResult {
    /// Build a [`ToolResult`] from the concatenated tool output. Attempts
    /// to parse the text as JSON so downstream variable extraction can
    /// walk the structure without re-parsing. A non-JSON or empty body
    /// leaves `parsed` as `None`.
    pub(crate) fn from_text(raw_text: String) -> Self {
        let parsed = if raw_text.is_empty() {
            None
        } else {
            serde_json::from_str::<Value>(&raw_text).ok()
        };
        Self { raw_text, parsed }
    }

    /// The raw (concatenated) text emitted by the tool. Always non-`None`;
    /// empty text is represented as an empty string.
    pub(crate) fn raw_text(&self) -> &str {
        &self.raw_text
    }

    /// The parsed JSON interpretation, if the raw text was valid JSON.
    #[cfg(test)]
    pub(crate) fn parsed(&self) -> Option<&Value> {
        self.parsed.as_ref()
    }

    /// Project this [`ToolResult`] onto a single [`serde_json::Value`] with
    /// the legacy shape used throughout the executor:
    ///
    /// * empty raw text → [`Value::Null`]
    /// * valid JSON → the parsed [`Value`]
    /// * otherwise → [`Value::String`] wrapping the raw text
    ///
    /// Currently only used by the `into_value` round-trip tests; kept as a
    /// borrow-friendly peer so call sites that need both the raw text and
    /// the projected value do not have to clone through [`Self::into_value`].
    #[cfg(test)]
    pub(crate) fn as_value(&self) -> Value {
        if self.raw_text.is_empty() {
            Value::Null
        } else {
            match &self.parsed {
                Some(v) => v.clone(),
                None => Value::String(self.raw_text.clone()),
            }
        }
    }

    /// Consuming variant of [`Self::as_value`] — avoids cloning when the
    /// caller owns the [`ToolResult`].
    pub(crate) fn into_value(self) -> Value {
        if self.raw_text.is_empty() {
            Value::Null
        } else {
            match self.parsed {
                Some(v) => v,
                None => Value::String(self.raw_text),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_text_empty_parses_to_none() {
        let tr = ToolResult::from_text(String::new());
        assert_eq!(tr.raw_text(), "");
        assert!(tr.parsed().is_none());
        assert_eq!(tr.as_value(), Value::Null);
    }

    #[test]
    fn from_text_plain_string_parsed_is_none() {
        let tr = ToolResult::from_text("not valid json".to_string());
        assert_eq!(tr.raw_text(), "not valid json");
        assert!(tr.parsed().is_none());
        assert_eq!(tr.as_value(), Value::String("not valid json".to_string()));
    }

    #[test]
    fn from_text_json_object_is_parsed() {
        let tr = ToolResult::from_text(r#"{"x":1,"y":2}"#.to_string());
        assert_eq!(tr.raw_text(), r#"{"x":1,"y":2}"#);
        assert_eq!(tr.parsed(), Some(&serde_json::json!({"x": 1, "y": 2})));
        assert_eq!(tr.as_value(), serde_json::json!({"x": 1, "y": 2}));
    }

    #[test]
    fn from_text_json_array_is_parsed() {
        let tr = ToolResult::from_text(r#"[1,2,3]"#.to_string());
        assert_eq!(tr.parsed(), Some(&serde_json::json!([1, 2, 3])));
        assert_eq!(tr.as_value(), serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn into_value_does_not_re_parse() {
        // Projections must match as_value exactly — the only reason to
        // prefer into_value is to avoid the clone.
        let cases = vec![
            ("", Value::Null),
            ("plain", Value::String("plain".to_string())),
            (r#"{"k":1}"#, serde_json::json!({"k": 1})),
            (r#"[true, null]"#, serde_json::json!([true, null])),
        ];
        for (raw, expected) in cases {
            let tr = ToolResult::from_text(raw.to_string());
            let projected_borrowed = tr.as_value();
            let projected_owned = tr.into_value();
            assert_eq!(projected_borrowed, expected);
            assert_eq!(projected_owned, expected);
        }
    }
}
