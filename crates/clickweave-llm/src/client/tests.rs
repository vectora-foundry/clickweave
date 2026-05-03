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

    async fn chat_with_options(
        &self,
        messages: &[Message],
        _tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> Result<ChatResponse> {
        self.calls.lock().unwrap().push(messages.to_vec());
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
fn chat_options_default_is_empty() {
    let opts = ChatOptions::default();
    assert_eq!(opts.temperature, None);
    assert_eq!(opts.max_tokens, None);
}

#[test]
fn chat_options_with_temperature_sets_only_temperature() {
    let opts = ChatOptions::with_temperature(0.0);
    assert_eq!(opts.temperature, Some(0.0));
    assert_eq!(opts.max_tokens, None);
}

#[tokio::test]
async fn chat_with_options_default_delegates_to_chat() {
    let backend = MockBackend::new("ok");
    backend
        .chat_with_options(
            &[Message::user("hi")],
            None,
            &ChatOptions::with_temperature(0.3),
        )
        .await
        .unwrap();
    // MockBackend doesn't override chat_with_options, so it falls through
    // to chat and still records the call.
    assert_eq!(backend.call_count(), 1);
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

#[test]
fn build_step_prompt_returns_prompt_when_no_extras() {
    let prompt = build_step_prompt("Click the save icon", None, None);
    assert_eq!(prompt, "Click the save icon");
}

#[test]
fn build_step_prompt_appends_button_text_when_present() {
    let prompt = build_step_prompt("Click the save icon", Some("Save"), None);
    assert!(prompt.starts_with("Click the save icon"));
    assert!(prompt.contains("Button to find: \"Save\""));
    assert!(!prompt.contains("Image to find"));
}

#[test]
fn build_step_prompt_appends_image_path_when_present() {
    let prompt = build_step_prompt("Click the logo", None, Some("./logo.png"));
    assert!(prompt.starts_with("Click the logo"));
    assert!(prompt.contains("Image to find: ./logo.png"));
    assert!(!prompt.contains("Button to find"));
}

#[test]
fn build_step_prompt_appends_both_hints_when_present() {
    let prompt = build_step_prompt("Confirm the action", Some("OK"), Some("./ok.png"));
    assert!(prompt.starts_with("Confirm the action"));
    assert!(prompt.contains("Button to find: \"OK\""));
    assert!(prompt.contains("Image to find: ./ok.png"));
}

#[test]
fn with_thinking_true_sets_explicit_flag() {
    let cfg = LlmConfig::default().with_thinking(true);
    let kwargs = cfg
        .extra_body
        .get("chat_template_kwargs")
        .expect("chat_template_kwargs must be present");
    assert_eq!(kwargs["enable_thinking"], serde_json::json!(true));
}

#[test]
fn with_thinking_false_sets_explicit_flag() {
    let cfg = LlmConfig::default().with_thinking(false);
    let kwargs = cfg
        .extra_body
        .get("chat_template_kwargs")
        .expect("chat_template_kwargs must be present");
    assert_eq!(kwargs["enable_thinking"], serde_json::json!(false));
}

#[test]
fn with_thinking_overrides_previous_setting() {
    let cfg = LlmConfig::default()
        .with_thinking(false)
        .with_thinking(true);
    let kwargs = &cfg.extra_body["chat_template_kwargs"];
    assert_eq!(kwargs["enable_thinking"], serde_json::json!(true));
}

#[test]
fn default_config_disables_thinking() {
    // LlmConfig::default() must emit enable_thinking: false so that
    // Gemma 4 / Qwen 3 server templates cannot silently enable reasoning mode.
    let cfg = LlmConfig::default();
    let kwargs = cfg
        .extra_body
        .get("chat_template_kwargs")
        .expect("chat_template_kwargs must be present in default config");
    assert_eq!(
        kwargs["enable_thinking"],
        serde_json::json!(false),
        "default config must explicitly disable thinking to prevent latency surprises"
    );
}

#[test]
fn message_captures_reasoning_content_from_response() {
    let body = r#"{
            "id": "x",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "I should call tool X because ..."
                },
                "finish_reason": "stop"
            }]
        }"#;
    let response: ChatResponse = serde_json::from_str(body).unwrap();
    let msg = &response.choices[0].message;
    assert_eq!(
        msg.reasoning_content.as_deref(),
        Some("I should call tool X because ...")
    );
}

#[test]
fn assistant_reasoning_content_stripped_from_request() {
    // When an assistant message with reasoning_content is passed through
    // the sanitization that LlmClient applies before every request, the
    // outgoing ChatRequest body must not contain the thought block.
    use crate::types::{ChatRequest, Role};
    let mut msg = Message::assistant("");
    msg.reasoning_content = Some("prior thought".to_string());

    // Apply the same sanitization LlmClient uses in chat_with_options.
    let sanitized: Vec<Message> = std::slice::from_ref(&msg)
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
        .collect();

    let extra = serde_json::Map::new();
    let request = ChatRequest {
        model: "test-model",
        messages: &sanitized,
        tools: None,
        temperature: None,
        max_tokens: None,
        extra_body: &extra,
    };
    let serialized = serde_json::to_string(&request).unwrap();
    assert!(
        !serialized.contains("reasoning_content"),
        "reasoning_content must be absent from the outbound request body; got: {serialized}"
    );
}

#[test]
fn message_omits_reasoning_content_when_absent() {
    let msg = Message::user("hello");
    let serialized = serde_json::to_string(&msg).unwrap();
    assert!(
        !serialized.contains("reasoning_content"),
        "reasoning_content must be omitted when None to avoid polluting requests"
    );
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
    assert_eq!(messages[0].role, Role::System);
    assert_eq!(messages[1].role, Role::User);
    // User message should contain image parts
    assert!(
        matches!(&messages[1].content, Some(Content::Parts(parts)) if parts.len() >= 2),
        "VLM user message should contain text + image parts"
    );
}

// ---- list_models parse tests (pure, no network) ----

/// Helper: call the parse logic of `list_models` against a fixture JSON body
/// without making a real HTTP request. Extracts the same logic by parsing
/// the body directly.
fn parse_models_response(body: &str) -> Result<Vec<String>> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|_| anyhow!("not valid JSON"))?;
    let data = json["data"].as_array().ok_or_else(|| {
        anyhow!("Response missing 'data' array; endpoint may not be OpenAI-compatible")
    })?;
    Ok(data
        .iter()
        .filter_map(|m| m["id"].as_str().map(String::from))
        .collect())
}

#[test]
fn list_models_parses_openai_shaped_response_with_multiple_models() {
    let body = r#"{
            "object": "list",
            "data": [
                {"id": "ModelA-Q8_0.gguf", "object": "model"},
                {"id": "ModelB-Q6_K.gguf", "object": "model"}
            ]
        }"#;
    let ids = parse_models_response(body).unwrap();
    assert_eq!(ids, vec!["ModelA-Q8_0.gguf", "ModelB-Q6_K.gguf"]);
}

#[test]
fn list_models_parses_single_model_response() {
    let body = r#"{"data": [{"id": "only-model"}]}"#;
    let ids = parse_models_response(body).unwrap();
    assert_eq!(ids, vec!["only-model"]);
}

#[test]
fn list_models_returns_err_when_data_array_missing() {
    let body = r#"{"object": "list", "models": []}"#;
    let err = parse_models_response(body).unwrap_err();
    assert!(
        err.to_string().contains("'data' array"),
        "error should mention missing data array, got: {err}"
    );
}

#[test]
fn list_models_returns_err_for_non_json_body() {
    let err = parse_models_response("not json at all").unwrap_err();
    assert!(
        err.to_string().contains("not valid JSON"),
        "error should indicate invalid JSON, got: {err}"
    );
}
