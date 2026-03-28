use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Extra provider-specific fields flattened into the request body
    /// (e.g. `{"chat_template_kwargs": {"enable_thinking": false}}`).
    #[serde(flatten, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra_body: serde_json::Map<String, Value>,
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
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    fn with_role(role: &str) -> Self {
        Self {
            role: role.to_string(),
            content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            content: Some(Content::Text(content.into())),
            ..Self::with_role("system")
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            content: Some(Content::Text(content.into())),
            ..Self::with_role("user")
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
            ..Self::with_role("user")
        }
    }

    pub fn text_content(&self) -> Option<&str> {
        match &self.content {
            Some(Content::Text(s)) => Some(s),
            Some(Content::Parts(parts)) => parts.iter().find_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            }),
            None => None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            content: Some(Content::Text(content.into())),
            ..Self::with_role("assistant")
        }
    }

    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            tool_calls: Some(tool_calls),
            ..Self::with_role("assistant")
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            content: Some(Content::Text(content.into())),
            tool_call_id: Some(tool_call_id.into()),
            ..Self::with_role("tool")
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
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Tool call arguments as a JSON string.
    /// Some providers (e.g. vLLM/llama.cpp) return this as an object instead
    /// of a string — the custom deserializer handles both.
    #[serde(deserialize_with = "deserialize_arguments")]
    pub arguments: String,
}

fn deserialize_arguments<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct ArgsVisitor;

    impl<'de> de::Visitor<'de> for ArgsVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a JSON string or object for tool call arguments")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.to_string())
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<String, E> {
            Ok(v)
        }

        fn visit_map<M: de::MapAccess<'de>>(self, map: M) -> Result<String, M::Error> {
            let obj = serde_json::Value::deserialize(de::value::MapAccessDeserializer::new(map))
                .map_err(de::Error::custom)?;
            serde_json::to_string(&obj).map_err(de::Error::custom)
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
