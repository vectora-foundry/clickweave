use crate::types::*;
use anyhow::{Context, Result};
use serde_json::Value;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, error, info, trace};

/// Seam for LLM interaction, allowing mock backends in tests.
pub trait ChatBackend: Send + Sync {
    fn chat(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<Value>>,
    ) -> impl Future<Output = Result<ChatResponse>> + Send;

    fn model_name(&self) -> &str;

    /// Query the provider for model metadata (context length, etc.).
    /// Returns None by default (e.g. for mock backends).
    fn fetch_model_info(&self) -> impl Future<Output = Result<Option<ModelInfo>>> + Send {
        async { Ok(None) }
    }
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Extra provider-specific fields to include in the request body
    /// (e.g. `{"chat_template_kwargs": {"enable_thinking": false}}`).
    pub extra_body: serde_json::Map<String, serde_json::Value>,
}

impl LlmConfig {
    /// Set `max_tokens` on this config (chainable).
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    /// Enable or disable model thinking/reasoning via `chat_template_kwargs` (chainable).
    /// When `false`, injects `{"chat_template_kwargs": {"enable_thinking": false}}`
    /// into the request body. When `true`, removes the override (model default).
    pub fn with_thinking(mut self, enabled: bool) -> Self {
        if enabled {
            self.extra_body.remove("chat_template_kwargs");
        } else {
            self.extra_body.insert(
                "chat_template_kwargs".to_string(),
                serde_json::json!({"enable_thinking": false}),
            );
        }
        self
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            // LM Studio default
            base_url: "http://localhost:1234/v1".to_string(),
            api_key: None,
            model: "local-model".to_string(),
            temperature: Some(0.7),
            max_tokens: Some(4096),
            extra_body: serde_json::Map::new(),
        }
    }
}

pub struct LlmClient {
    config: LlmConfig,
    http: reqwest::Client,
    /// Cached context length from provider, 0 means unknown.
    context_length: AtomicU64,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            context_length: AtomicU64::new(0),
        }
    }

    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    pub fn config_mut(&mut self) -> &mut LlmConfig {
        &mut self.config
    }

    async fn try_models_endpoint(&self, url: &str, model_id: &str) -> Result<Option<ModelInfo>> {
        let mut req = self.http.get(url);
        if let Some(api_key) = &self.config.api_key {
            req = req.bearer_auth(api_key);
        }

        let response = req
            .send()
            .await
            .context("Failed to query models endpoint")?;

        if !response.status().is_success() {
            debug!(url = %url, status = %response.status(), "Models endpoint returned error");
            return Ok(None);
        }

        let response_text = response
            .text()
            .await
            .context("Failed to read models response")?;

        trace!(url = %url, response = %response_text, "Raw models response");

        let body: ModelsResponse =
            serde_json::from_str(&response_text).context("Failed to parse models response")?;

        let info = body
            .data
            .into_iter()
            .find(|m| m.id == model_id || model_id.contains(&m.id) || m.id.contains(model_id));

        Ok(info)
    }

    fn log_usage(&self, response: &ChatResponse) {
        let Some(usage) = &response.usage else {
            return;
        };

        let ctx = self.context_length.load(Ordering::Relaxed);
        if ctx > 0 {
            let pct = (usage.total_tokens as f64 / ctx as f64 * 100.0) as u32;
            info!(
                prompt_tokens = usage.prompt_tokens,
                completion_tokens = usage.completion_tokens,
                total_tokens = usage.total_tokens,
                context_length = ctx,
                usage_pct = pct,
                "LLM usage ({}/{}  {}%)",
                usage.total_tokens,
                ctx,
                pct
            );
        } else {
            info!(
                prompt_tokens = usage.prompt_tokens,
                completion_tokens = usage.completion_tokens,
                total_tokens = usage.total_tokens,
                "LLM usage"
            );
        }
    }
}

impl ChatBackend for LlmClient {
    fn model_name(&self) -> &str {
        &self.config.model
    }

    async fn chat(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<Value>>,
    ) -> Result<ChatResponse> {
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );

        let request = ChatRequest {
            model: self.config.model.clone(),
            messages,
            tools,
            tool_choice: None,
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            extra_body: self.config.extra_body.clone(),
        };

        debug!(
            url = %url,
            message_count = request.messages.len(),
            model = %request.model,
            "LLM request"
        );
        trace!(
            request_body = %serde_json::to_string(&request).unwrap_or_default(),
            "LLM request body"
        );

        let mut req_builder = self.http.post(&url).json(&request);

        if let Some(api_key) = &self.config.api_key {
            req_builder = req_builder.bearer_auth(api_key);
        }

        let response = match req_builder.send().await {
            Ok(r) => r,
            Err(e) => {
                error!(url = %url, error = %e, "LLM request failed to send");
                return Err(e).context("Failed to send request to LLM");
            }
        };

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            error!(url = %url, status = %status, body = %error_text, "LLM returned error");
            // Try to extract a clean message from the JSON error body
            let user_msg = serde_json::from_str::<Value>(&error_text)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(String::from))
                .unwrap_or_else(|| format!("LLM request failed ({})", status));
            anyhow::bail!("{}", user_msg);
        }

        let response_text = match response.text().await {
            Ok(t) => t,
            Err(e) => {
                error!(url = %url, error = %e, "Failed to read LLM response body");
                return Err(e).context("Failed to read LLM response body");
            }
        };

        trace!(response_body = %response_text, "LLM response body");

        let chat_response: ChatResponse = match serde_json::from_str(&response_text) {
            Ok(r) => r,
            Err(e) => {
                error!(
                    error = %e,
                    body = %&response_text[..response_text.len().min(500)],
                    "Failed to parse LLM response"
                );
                return Err(e).context("Failed to parse LLM response");
            }
        };

        self.log_usage(&chat_response);

        let first_choice = chat_response.choices.first();
        let tool_names: Vec<&str> = first_choice
            .and_then(|c| c.message.tool_calls.as_ref())
            .map(|tcs| tcs.iter().map(|tc| tc.function.name.as_str()).collect())
            .unwrap_or_default();

        let tool_calls_display = if tool_names.is_empty() {
            None
        } else {
            Some(&tool_names)
        };
        info!(
            finish_reason = ?first_choice.and_then(|c| c.finish_reason.as_ref()),
            tool_calls = ?tool_calls_display,
            "LLM response"
        );

        if let Some(choice) = first_choice {
            if let Some(content) = choice.message.content_text() {
                debug!(content = %content, "LLM response content");
            }
            if let Some(tool_calls) = &choice.message.tool_calls {
                for tc in tool_calls {
                    debug!(
                        tool = %tc.function.name,
                        arguments = %tc.function.arguments,
                        "LLM tool call"
                    );
                }
            }
        }

        Ok(chat_response)
    }

    async fn fetch_model_info(&self) -> Result<Option<ModelInfo>> {
        let base = self.config.base_url.trim_end_matches('/');
        let model_id = &self.config.model;

        // Try endpoints in order of richness:
        // 1. LM Studio /api/v0/models (has context length, arch, quantization)
        // 2. OpenAI-compatible /v1/models (minimal, but widely supported)
        let base_origin = base.find("/v1").map(|i| &base[..i]).unwrap_or(base);

        let endpoints = [
            format!("{}/api/v0/models", base_origin),
            format!("{}/models", base),
        ];

        let mut fallback: Option<ModelInfo> = None;

        for endpoint in &endpoints {
            match self.try_models_endpoint(endpoint, model_id).await {
                Ok(Some(info)) if info.effective_context_length().is_some() => {
                    if let Some(ctx) = info.effective_context_length() {
                        self.context_length.store(ctx, Ordering::Relaxed);
                    }
                    return Ok(Some(info));
                }
                Ok(Some(info)) => {
                    debug!(endpoint = %endpoint, "Model found but no context length, trying next");
                    fallback.get_or_insert(info);
                }
                Ok(None) => continue,
                Err(e) => {
                    debug!(endpoint = %endpoint, error = %e, "Endpoint failed");
                }
            }
        }

        Ok(fallback)
    }
}

/// System prompt for the agent (text-only, no images).
pub fn workflow_system_prompt() -> String {
    r#"You are a UI automation assistant executing an AI Step node within a workflow.

You have access to MCP tools for native UI interaction:
- take_screenshot: capture the screen, a window, or a region (optionally with OCR)
- find_text: locate text on screen using OCR
- find_image: template-match an image on screen
- click: click at coordinates or on an element
- type_text: type text at the cursor
- scroll: scroll at a position
- list_apps: list running applications (use user_apps_only=true to filter system processes)
- list_windows / focus_window: manage windows (focus_window accepts app_name, window_id, or pid)

For each step, you will receive:
- A prompt describing the objective
- Optional button_text: specific text to find and click
- Optional template_image: path to an image to locate on screen

Image outputs from tools are analyzed by a separate vision model. You will receive
their analysis as a VLM_IMAGE_SUMMARY message containing a JSON object with:
- summary: what is visible on screen
- visible_text: key labels, buttons, headings
- alerts: errors, popups, permission prompts
- notes_for_agent: non-prescriptive hints

Use find_text / find_image for precise coordinate targeting. Do not guess coordinates.

Strategy:
1. If you need to see the screen, take a screenshot to observe the current state
2. Use find_text or find_image to locate targets precisely
3. Perform the required input actions (click, type, scroll)
4. Verify the result with another screenshot if needed

When you have completed the step's objective, respond with a JSON object:
{"step_complete": true, "summary": "Brief description of what was done"}

If you cannot complete the step:
{"step_complete": false, "error": "Description of the problem"}

Be precise with coordinates. Always verify actions when the outcome matters.
Only use tool parameters that exist in the tool schema. Do not invent parameters."#
        .to_string()
}

/// System prompt for the VLM (vision model).
pub fn vlm_system_prompt() -> String {
    r#"You are a visual analyst for UI automation. You receive screenshots and images from tool results and produce structured descriptions for an agent model that cannot see images.

Output ONLY a JSON object with these fields:
{
  "summary": "1-3 sentences describing what is visible on screen",
  "visible_text": ["key labels", "button text", "dialog headings"],
  "alerts": ["any errors", "popups", "permission prompts"],
  "notes_for_agent": "Non-prescriptive hints, e.g. 'There is a modal blocking the UI' or 'The search field is focused'"
}

Rules:
- Be factual and concise. Describe what you see, not what to do.
- Include coordinates only if they are clearly visible (e.g. OCR overlay).
- Do NOT suggest actions or next steps — the agent decides.
- If nothing notable is on screen, keep fields empty but still return valid JSON."#
        .to_string()
}

/// Build the user prompt for a VLM image analysis call.
pub fn build_vlm_prompt(step_goal: &str, tool_name: &str) -> String {
    format!(
        "The agent is working on: \"{}\"\n\
         The following image(s) were returned by the \"{}\" tool.\n\
         Analyze the image(s) and produce the JSON summary.",
        step_goal, tool_name
    )
}

/// Call the VLM to analyze images and return a text summary.
pub async fn analyze_images(
    vlm: &(impl ChatBackend + ?Sized),
    step_goal: &str,
    tool_name: &str,
    images: Vec<(String, String)>,
) -> Result<String> {
    let messages = vec![
        Message::system(vlm_system_prompt()),
        Message::user_with_images(build_vlm_prompt(step_goal, tool_name), images),
    ];

    let response = vlm.chat(messages, None).await?;

    let text = response
        .choices
        .first()
        .and_then(|c| c.message.content_text())
        .unwrap_or("")
        .to_string();

    Ok(text)
}

/// Build user message for a workflow step.
pub fn build_step_prompt(
    prompt: &str,
    button_text: Option<&str>,
    image_path: Option<&str>,
) -> String {
    let mut result = prompt.to_string();

    if let Some(text) = button_text {
        result.push_str(&format!("\nButton to find: \"{}\"", text));
    }

    if let Some(path) = image_path {
        result.push_str(&format!("\nImage to find: {}", path));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Mock backend that records calls and returns a canned response.
    struct MockBackend {
        response_text: String,
        calls: Mutex<Vec<Vec<Message>>>,
    }

    impl MockBackend {
        fn new(response_text: &str) -> Self {
            Self {
                response_text: response_text.to_string(),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }

        fn last_messages(&self) -> Vec<Message> {
            self.calls
                .lock()
                .unwrap()
                .last()
                .cloned()
                .unwrap_or_default()
        }
    }

    impl ChatBackend for MockBackend {
        fn model_name(&self) -> &str {
            "mock-model"
        }

        async fn chat(
            &self,
            messages: Vec<Message>,
            _tools: Option<Vec<Value>>,
        ) -> Result<ChatResponse> {
            self.calls.lock().unwrap().push(messages);
            Ok(ChatResponse {
                id: "mock".to_string(),
                choices: vec![Choice {
                    index: 0,
                    message: Message::assistant(&self.response_text),
                    finish_reason: Some("stop".to_string()),
                }],
                usage: None,
            })
        }
    }

    #[test]
    fn vlm_system_prompt_requests_json() {
        let prompt = vlm_system_prompt();
        assert!(
            prompt.contains("JSON"),
            "VLM prompt should request JSON output"
        );
        assert!(
            prompt.contains("summary"),
            "VLM prompt should mention summary field"
        );
        assert!(
            prompt.contains("visible_text"),
            "VLM prompt should mention visible_text field"
        );
        assert!(
            prompt.contains("alerts"),
            "VLM prompt should mention alerts field"
        );
        assert!(
            prompt.contains("notes_for_agent"),
            "VLM prompt should mention notes_for_agent field"
        );
    }

    #[test]
    fn vlm_prompt_is_non_prescriptive() {
        let prompt = vlm_system_prompt();
        assert!(
            prompt.contains("Do NOT suggest actions"),
            "VLM prompt should forbid suggesting actions"
        );
    }

    #[test]
    fn agent_prompt_mentions_vlm_summary() {
        let prompt = workflow_system_prompt();
        assert!(
            prompt.contains("VLM_IMAGE_SUMMARY"),
            "Agent prompt should describe VLM summary format"
        );
    }

    #[test]
    fn build_vlm_prompt_includes_context() {
        let prompt = build_vlm_prompt("click the login button", "take_screenshot");
        assert!(prompt.contains("click the login button"));
        assert!(prompt.contains("take_screenshot"));
    }

    #[tokio::test]
    async fn analyze_images_returns_vlm_text() {
        let mock = MockBackend::new(r#"{"summary": "a login screen"}"#);
        let result = analyze_images(
            &mock,
            "click the login button",
            "take_screenshot",
            vec![("base64data".to_string(), "image/png".to_string())],
        )
        .await
        .unwrap();

        assert_eq!(result, r#"{"summary": "a login screen"}"#);
        assert_eq!(mock.call_count(), 1);

        // Verify the VLM received the system prompt + user message with images
        let messages = mock.last_messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        // User message should contain image parts
        assert!(
            matches!(&messages[1].content, Some(Content::Parts(parts)) if parts.len() >= 2),
            "VLM user message should contain text + image parts"
        );
    }
}
