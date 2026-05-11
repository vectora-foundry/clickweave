use super::*;

/// Per-call overrides for a single `chat` invocation. Any field left at `None`
/// falls back to the backend's configured default.
#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

impl ChatOptions {
    pub fn with_temperature(temperature: f32) -> Self {
        Self {
            temperature: Some(temperature),
            max_tokens: None,
        }
    }
}

/// Seam for LLM interaction, allowing mock backends in tests.
pub trait ChatBackend: Send + Sync {
    /// Required. Real backends honor `options`; mocks that don't care can
    /// ignore them. `chat()` is a thin default wrapper so the common case
    /// stays concise.
    fn chat_with_options(
        &self,
        messages: &[Message],
        tools: Option<&[Value]>,
        options: &ChatOptions,
    ) -> impl Future<Output = Result<ChatResponse>> + Send;

    fn chat(
        &self,
        messages: &[Message],
        tools: Option<&[Value]>,
    ) -> impl Future<Output = Result<ChatResponse>> + Send {
        async move {
            self.chat_with_options(messages, tools, &ChatOptions::default())
                .await
        }
    }

    fn model_name(&self) -> &str;

    /// Query the provider for model metadata (context length, etc.).
    /// Returns None by default (e.g. for mock backends).
    fn fetch_model_info(&self) -> impl Future<Output = Result<Option<ModelInfo>>> + Send {
        async { Ok(None) }
    }
}
