use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

#[derive(Debug, Serialize)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<&'a [Value]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Extra provider-specific fields flattened into the request body
    /// (e.g. `{"chat_template_kwargs": {"enable_thinking": false}}`).
    #[serde(flatten, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra_body: &'a serde_json::Map<String, Value>,
}

/// OpenAI chat message role.
///
/// Serializes to lowercase strings on the wire (`"system"`, `"user"`,
/// `"assistant"`, `"tool"`) to match the OpenAI Chat Completions API.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    /// Lowercase wire string for this role.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for Role {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<&str> for Role {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<str> for Role {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<Role> for &str {
    fn eq(&self, other: &Role) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<Role> for str {
    fn eq(&self, other: &Role) -> bool {
        self == other.as_str()
    }
}

/// OpenAI tool call type. The spec currently defines only `"function"`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum CallType {
    #[default]
    Function,
}

impl CallType {
    pub fn as_str(self) -> &'static str {
        match self {
            CallType::Function => "function",
        }
    }
}

impl fmt::Display for CallType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for CallType {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<&str> for CallType {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<str> for CallType {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

/// Message content can be a plain string or an array of content parts (for vision).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl Content {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text(s) => Some(s),
            Content::Parts(parts) => parts.iter().find_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    /// Thinking-model scratchpad captured from assistant responses for logging
    /// and observability. Must NOT be fed back into subsequent requests:
    /// Gemma 4's model card prohibits echoing prior-turn thought blocks, and
    /// doing so causes context accumulation and degraded tool selection.
    /// `LlmClient` strips this field before serializing outbound requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    fn with_role(role: Role) -> Self {
        Self {
            role,
            content: None,
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            content: Some(Content::Text(content.into())),
            ..Self::with_role(Role::System)
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            content: Some(Content::Text(content.into())),
            ..Self::with_role(Role::User)
        }
    }

    pub fn user_with_images(text: impl Into<String>, images: Vec<(String, String)>) -> Self {
        let mut parts = vec![ContentPart::Text { text: text.into() }];
        parts.extend(
            images
                .into_iter()
                .map(|(data, mime_type)| ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: format!("data:{};base64,{}", mime_type, data),
                    },
                }),
        );
        Self {
            content: Some(Content::Parts(parts)),
            ..Self::with_role(Role::User)
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            content: Some(Content::Text(content.into())),
            ..Self::with_role(Role::Assistant)
        }
    }

    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            tool_calls: Some(tool_calls),
            ..Self::with_role(Role::Assistant)
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            content: Some(Content::Text(content.into())),
            tool_call_id: Some(tool_call_id.into()),
            ..Self::with_role(Role::Tool)
        }
    }

    /// Get content as text, regardless of whether it's a plain string or parts.
    pub fn content_text(&self) -> Option<&str> {
        self.content.as_ref().and_then(|c| c.as_text())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default)]
    pub call_type: CallType,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Tool call arguments as a JSON value.
    /// The OpenAI wire format sends this as a JSON-encoded string, but some
    /// providers (e.g. vLLM/llama.cpp) return an object directly. Deserialization
    /// accepts both and always yields a `Value`; serialization emits the
    /// OpenAI-compatible stringified form so echoed tool calls round-trip
    /// correctly to strict backends.
    #[serde(
        deserialize_with = "deserialize_arguments",
        serialize_with = "serialize_arguments"
    )]
    pub arguments: Value,
}

fn serialize_arguments<S>(value: &Value, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        Value::String(s) => serializer.serialize_str(s),
        Value::Null => serializer.serialize_str(""),
        other => serializer
            .serialize_str(&serde_json::to_string(other).map_err(serde::ser::Error::custom)?),
    }
}

fn deserialize_arguments<'de, D>(deserializer: D) -> Result<Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct ArgsVisitor;

    impl<'de> de::Visitor<'de> for ArgsVisitor {
        type Value = Value;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a JSON string or object for tool call arguments")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
            if v.is_empty() {
                return Ok(Value::Null);
            }
            // Fall back to the raw string when the payload isn't valid JSON so
            // downstream code can surface the malformed arguments instead of
            // failing the whole response deserialization.
            Ok(serde_json::from_str(v).unwrap_or_else(|_| Value::String(v.to_string())))
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Value, E> {
            self.visit_str(&v)
        }

        fn visit_map<M: de::MapAccess<'de>>(self, map: M) -> Result<Value, M::Error> {
            Value::deserialize(de::value::MapAccessDeserializer::new(map))
                .map_err(de::Error::custom)
        }
    }

    deserializer.deserialize_any(ArgsVisitor)
}

#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: Message,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Provider-agnostic model info from /v1/models.
///
/// Different providers return different fields:
/// - LM Studio: `max_context_length`, `loaded_context_length`, `arch`, `quantization`
/// - vLLM: `max_model_len`
/// - OpenRouter: `context_length`
/// - OpenAI standard: only `id`, `object`, `created`, `owned_by`
///
/// Extra fields are captured in `extra` for forward compatibility.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub owned_by: Option<String>,
    // LM Studio fields
    pub max_context_length: Option<u64>,
    pub loaded_context_length: Option<u64>,
    pub arch: Option<String>,
    pub quantization: Option<String>,
    // vLLM fields
    pub max_model_len: Option<u64>,
    // OpenRouter fields
    pub context_length: Option<u64>,
    /// All other provider-specific fields.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl ModelInfo {
    /// Best-effort context length from whichever provider field is available.
    /// Prefers loaded (actual) over max (theoretical).
    pub fn effective_context_length(&self) -> Option<u64> {
        self.loaded_context_length
            .or(self.max_context_length)
            .or(self.max_model_len)
            .or(self.context_length)
    }
}

#[derive(Debug, Deserialize)]
pub struct ModelsResponse {
    pub data: Vec<ModelInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_serializes_to_lowercase_string() {
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), "\"system\"");
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"assistant\""
        );
        assert_eq!(serde_json::to_string(&Role::Tool).unwrap(), "\"tool\"");
    }

    #[test]
    fn role_deserializes_from_lowercase_string() {
        let r: Role = serde_json::from_str("\"assistant\"").unwrap();
        assert_eq!(r, Role::Assistant);
    }

    #[test]
    fn role_display_matches_wire_format() {
        assert_eq!(Role::System.to_string(), "system");
        assert_eq!(Role::Tool.to_string(), "tool");
    }

    #[test]
    fn role_compares_equal_to_string_slice() {
        // Both directions of PartialEq<&str> must work for ergonomic
        // comparisons at migration sites.
        let r = Role::User;
        assert!(r == "user");
        assert!("user" == r);
        assert!(r != "system");
    }

    #[test]
    fn role_as_ref_str_returns_wire_value() {
        let r: &str = Role::Assistant.as_ref();
        assert_eq!(r, "assistant");
    }

    #[test]
    fn call_type_serializes_to_function_string() {
        assert_eq!(
            serde_json::to_string(&CallType::Function).unwrap(),
            "\"function\""
        );
    }

    #[test]
    fn call_type_deserializes_from_function_string() {
        let c: CallType = serde_json::from_str("\"function\"").unwrap();
        assert_eq!(c, CallType::Function);
    }

    #[test]
    fn call_type_default_is_function() {
        // OpenAI spec currently only defines `function`; the default keeps
        // deserialization resilient if a provider omits the field.
        assert_eq!(CallType::default(), CallType::Function);
    }

    #[test]
    fn message_serializes_role_as_lowercase_string() {
        // On-wire compatibility with the OpenAI API.
        let msg = Message::system("hi");
        let serialized = serde_json::to_string(&msg).unwrap();
        assert!(
            serialized.contains("\"role\":\"system\""),
            "role must serialize to lowercase string, got: {serialized}"
        );
    }

    #[test]
    fn message_round_trips_through_json() {
        let msg = Message::assistant("thinking");
        let serialized = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&serialized).unwrap();
        assert_eq!(back.role, Role::Assistant);
        assert_eq!(back.content_text(), Some("thinking"));
    }

    #[test]
    fn tool_call_serializes_type_as_function_string() {
        // Use ToolCall inside a serialization harness to exercise the
        // `#[serde(rename = "type")]` wire contract.
        let tc = ToolCall {
            id: "call_0".to_string(),
            call_type: CallType::Function,
            function: FunctionCall {
                name: "click".to_string(),
                arguments: Value::Object(serde_json::Map::new()),
            },
        };
        let serialized = serde_json::to_string(&tc).unwrap();
        assert!(
            serialized.contains("\"type\":\"function\""),
            "call_type must serialize as `type: \"function\"`, got: {serialized}"
        );
    }

    #[test]
    fn tool_call_round_trips_with_call_type() {
        let payload =
            r#"{"id":"call_1","type":"function","function":{"name":"f","arguments":"{}"}}"#;
        let tc: ToolCall = serde_json::from_str(payload).unwrap();
        assert_eq!(tc.call_type, CallType::Function);
        assert_eq!(tc.id, "call_1");
        assert_eq!(tc.function.name, "f");
    }
}
