use super::*;

impl LlmClient {
    pub fn new(config: LlmConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .build()
            .expect("reqwest::Client builder failed");

        Self {
            config,
            http,
            context_length: AtomicU64::new(0),
        }
    }

    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    pub fn config_mut(&mut self) -> &mut LlmConfig {
        &mut self.config
    }

    fn chat_url(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }

    fn sanitize_request_messages(messages: &[Message]) -> Vec<Message> {
        // Strip reasoning_content from all assistant messages before sending.
        // Gemma 4 (and other thinking-capable models) must not receive prior
        // turns' thought blocks in subsequent requests — only visible content
        // and tool_calls should be echoed back. This is a defense-in-depth
        // guard; the primary guard lives in AgentRunner::append_assistant_message.
        messages
            .iter()
            .map(|m| {
                if m.role == Role::Assistant && m.reasoning_content.is_some() {
                    Message {
                        reasoning_content: None,
                        ..m.clone()
                    }
                } else {
                    m.clone()
                }
            })
            .collect()
    }

    fn build_chat_request<'a>(
        &'a self,
        messages: &'a [Message],
        tools: Option<&'a [Value]>,
        options: &ChatOptions,
    ) -> ChatRequest<'a> {
        ChatRequest {
            model: &self.config.model,
            messages,
            tools,
            temperature: options.temperature.or(self.config.temperature),
            max_tokens: options.max_tokens.or(self.config.max_tokens),
            extra_body: &self.config.extra_body,
        }
    }

    async fn send_chat_request(&self, url: &str, request: &ChatRequest<'_>) -> Result<String> {
        debug!(
            url = %url,
            message_count = request.messages.len(),
            model = %request.model,
            "LLM request"
        );
        trace!(
            request_body = %serde_json::to_string(request)
                .unwrap_or_else(|e| format!("<serialization failed: {e}>")),
            "LLM request body"
        );

        let mut req_builder = self.http.post(url).json(request);

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
            let user_msg = llm_error_message(&error_text, status);
            anyhow::bail!("{}", user_msg);
        }

        match response.text().await {
            Ok(t) => Ok(t),
            Err(e) => {
                error!(url = %url, error = %e, "Failed to read LLM response body");
                Err(e).context("Failed to read LLM response body")
            }
        }
    }

    fn parse_chat_response(response_text: &str) -> Result<ChatResponse> {
        trace!(response_body = %response_text, "LLM response body");

        serde_json::from_str(response_text).map_err(|e| {
            error!(
                error = %e,
                body = %&response_text[..response_text.len().min(500)],
                "Failed to parse LLM response"
            );
            anyhow::Error::new(e).context("Failed to parse LLM response")
        })
    }

    fn log_chat_response_summary(chat_response: &ChatResponse) {
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

    async fn chat_with_options(
        &self,
        messages: &[Message],
        tools: Option<&[Value]>,
        options: &ChatOptions,
    ) -> Result<ChatResponse> {
        let url = self.chat_url();
        let sanitized = Self::sanitize_request_messages(messages);
        let request = self.build_chat_request(&sanitized, tools, options);
        let response_text = self.send_chat_request(&url, &request).await?;
        let chat_response = Self::parse_chat_response(&response_text)?;

        self.log_usage(&chat_response);
        Self::log_chat_response_summary(&chat_response);
        Ok(chat_response)
    }

    async fn fetch_model_info(&self) -> Result<Option<ModelInfo>> {
        let base = self.config.base_url.trim_end_matches('/');
        let model_id = &self.config.model;

        // Try endpoints in order of richness:
        // 1. LM Studio /api/v0/models (has context length, arch, quantization)
        // 2. OpenAI-compatible /v1/models (minimal, but widely supported)
        let base_origin = base.strip_suffix("/v1").unwrap_or(base);

        let endpoints = [
            format!("{}/api/v0/models", base_origin),
            format!("{}/models", base),
        ];

        let mut fallback: Option<ModelInfo> = None;
        let mut had_error = false;

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
                    had_error = true;
                    debug!(endpoint = %endpoint, error = %e, "Endpoint failed");
                }
            }
        }

        if fallback.is_none() && had_error {
            warn!(
                base_url = %self.config.base_url,
                "All model-info endpoints failed; context length unavailable"
            );
        }

        Ok(fallback)
    }
}

fn llm_error_message(error_text: &str, status: reqwest::StatusCode) -> String {
    serde_json::from_str::<Value>(error_text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(String::from))
        .unwrap_or_else(|| format!("LLM request failed ({})", status))
}
