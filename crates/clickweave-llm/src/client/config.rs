#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Extra provider-specific fields to include in the request body.
    /// Defaults to `{"chat_template_kwargs": {"enable_thinking": false}}` — this is
    /// not merely illustrative; it is the actual default. The explicit `false` is
    /// required because server-side templates for Gemma 4 and Qwen 3 default to
    /// reasoning mode ON, which would silently add ~15× latency for any caller
    /// that constructs `LlmConfig::default()` without an explicit `.with_thinking()` chain.
    pub extra_body: serde_json::Map<String, serde_json::Value>,
}

impl LlmConfig {
    /// Set `max_tokens` on this config (chainable).
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    /// Enable or disable model thinking/reasoning via `chat_template_kwargs` (chainable).
    /// Always sends an explicit `{"chat_template_kwargs": {"enable_thinking": <bool>}}`
    /// so the server/template default cannot silently override the caller's intent.
    pub fn with_thinking(mut self, enabled: bool) -> Self {
        self.extra_body.insert(
            "chat_template_kwargs".to_string(),
            serde_json::json!({"enable_thinking": enabled}),
        );
        self
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        let base = Self {
            // LM Studio default
            base_url: "http://localhost:1234/v1".to_string(),
            api_key: None,
            model: "local-model".to_string(),
            temperature: Some(0.7),
            max_tokens: Some(4096),
            extra_body: serde_json::Map::new(),
        };
        // Explicit `enable_thinking: false` so the server template default (which is ON
        // for Gemma 4 / Qwen 3) cannot silently add latency to callers that forget to
        // chain `.with_thinking(false)`.
        base.with_thinking(false)
    }
}
