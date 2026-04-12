use super::super::*;
use crate::executor::Mcp;
use clickweave_core::runtime::RuntimeContext;
use clickweave_core::storage::RunStorage;
use clickweave_llm::{
    ChatBackend, ChatOptions, ChatResponse, Choice, Content, ContentPart, Message,
};
use clickweave_mcp::{ToolCallResult, ToolContent};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// A stub ChatBackend that never expects to be called.
/// Useful for tests that only exercise cache mechanics without LLM interaction.
pub(super) struct StubBackend;

impl ChatBackend for StubBackend {
    fn model_name(&self) -> &str {
        "stub"
    }

    async fn chat_with_options(
        &self,
        _messages: &[Message],
        _tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> anyhow::Result<ChatResponse> {
        panic!("StubBackend::chat should not be called in this test");
    }
}

/// Helper to create a `WorkflowExecutor<StubBackend>` with minimal setup.
pub(super) fn make_test_executor() -> WorkflowExecutor<StubBackend> {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let workflow = Workflow::default();
    let temp_dir = std::env::temp_dir().join("clickweave_test_executor");
    let storage = RunStorage::new_app_data(&temp_dir, &workflow.name, workflow.id);
    WorkflowExecutor::with_backends(
        workflow,
        StubBackend,
        None,
        String::new(),
        ExecutionMode::Run,
        None,
        tx,
        storage,
        CancellationToken::new(),
    )
}

/// A ChatBackend that returns a queue of scripted responses.
/// Used to test flows that call the LLM (e.g. resolve_element_name).
pub(super) struct ScriptedBackend {
    responses: Mutex<Vec<String>>,
}

impl ScriptedBackend {
    pub(super) fn new(responses: Vec<&str>) -> Self {
        // Reverse so pop() returns responses in FIFO order.
        let mut v: Vec<String> = responses.into_iter().map(String::from).collect();
        v.reverse();
        Self {
            responses: Mutex::new(v),
        }
    }
}

impl ChatBackend for ScriptedBackend {
    fn model_name(&self) -> &str {
        "scripted"
    }

    async fn chat_with_options(
        &self,
        _messages: &[Message],
        _tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> anyhow::Result<ChatResponse> {
        let text = self
            .responses
            .lock()
            .unwrap()
            .pop()
            .expect("ScriptedBackend exhausted — no more responses");
        Ok(ChatResponse {
            id: "scripted".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message::assistant(&text),
                finish_reason: Some("stop".to_string()),
            }],
            usage: None,
        })
    }
}

/// A stub Mcp implementation that returns queued responses and records calls.
///
/// Use `push_response` to queue tool call results in FIFO order. When a tool
/// is called, the next queued response is returned. If no responses are queued,
/// the call panics (test misconfiguration).
///
/// Call history is recorded in `calls` for assertions.
pub(super) struct StubToolProvider {
    responses: Mutex<Vec<ToolCallResult>>,
    calls: Mutex<Vec<(String, Option<Value>)>>,
    /// Tool names that `has_tool` reports as available.
    available_tools: Vec<String>,
    /// Schemas returned by `tools_as_openai`.
    openai_schemas: Vec<Value>,
}

impl StubToolProvider {
    #[allow(dead_code)]
    pub(super) fn new() -> Self {
        Self {
            responses: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            available_tools: Vec::new(),
            openai_schemas: Vec::new(),
        }
    }

    /// Set the tool names that `has_tool` will recognise.
    #[allow(dead_code)]
    pub(super) fn with_tools(mut self, tools: Vec<&str>) -> Self {
        self.available_tools = tools.into_iter().map(String::from).collect();
        self
    }

    /// Set the schemas returned by `tools_as_openai`.
    #[allow(dead_code)]
    pub(super) fn with_openai_schemas(mut self, schemas: Vec<Value>) -> Self {
        self.openai_schemas = schemas;
        self
    }

    /// Queue a successful text response.
    #[allow(dead_code)]
    pub(super) fn push_text_response(&self, text: &str) {
        self.responses.lock().unwrap().push(ToolCallResult {
            content: vec![ToolContent::Text {
                text: text.to_string(),
            }],
            is_error: None,
        });
    }

    /// Queue a raw ToolCallResult.
    #[allow(dead_code)]
    pub(super) fn push_response(&self, result: ToolCallResult) {
        self.responses.lock().unwrap().push(result);
    }

    /// Return all recorded calls as `(tool_name, arguments)` pairs.
    #[allow(dead_code)]
    pub(super) fn take_calls(&self) -> Vec<(String, Option<Value>)> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}

impl Mcp for StubToolProvider {
    async fn call_tool(
        &self,
        name: &str,
        arguments: Option<Value>,
    ) -> anyhow::Result<ToolCallResult> {
        self.calls
            .lock()
            .unwrap()
            .push((name.to_string(), arguments.clone()));
        let mut queue = self.responses.lock().unwrap();
        if queue.is_empty() {
            panic!("StubToolProvider: no queued response for '{}'", name);
        }
        Ok(queue.remove(0))
    }

    fn has_tool(&self, name: &str) -> bool {
        self.available_tools.iter().any(|t| t == name)
    }

    fn tools_as_openai(&self) -> Vec<Value> {
        self.openai_schemas.clone()
    }

    async fn refresh_tools(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

pub(super) fn make_scripted_executor(responses: Vec<&str>) -> WorkflowExecutor<ScriptedBackend> {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let workflow = Workflow::default();
    let temp_dir = std::env::temp_dir().join("clickweave_test_scripted");
    let storage = RunStorage::new_app_data(&temp_dir, &workflow.name, workflow.id);
    WorkflowExecutor::with_backends(
        workflow,
        ScriptedBackend::new(responses),
        None,
        String::new(),
        ExecutionMode::Run,
        None,
        tx,
        storage,
        CancellationToken::new(),
    )
}

pub(super) fn strs(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Check that a list of messages contains no image content parts.
pub(super) fn assert_no_images(messages: &[Message]) {
    for (i, msg) in messages.iter().enumerate() {
        if let Some(Content::Parts(parts)) = &msg.content {
            for part in parts {
                if matches!(part, ContentPart::ImageUrl { .. }) {
                    panic!(
                        "Message[{}] (role={}) contains image content — agent should never receive images when VLM is configured",
                        i, msg.role
                    );
                }
            }
        }
    }
}

impl<C: ChatBackend> WorkflowExecutor<C> {
    #[allow(clippy::too_many_arguments)]
    pub fn with_backends(
        workflow: Workflow,
        agent: C,
        fast: Option<C>,
        mcp_binary_path: String,
        execution_mode: ExecutionMode,
        project_path: Option<PathBuf>,
        event_tx: Sender<ExecutorEvent>,
        storage: RunStorage,
        cancel_token: CancellationToken,
    ) -> Self {
        let decision_cache = clickweave_core::decision_cache::DecisionCache::new(workflow.id);
        Self {
            workflow,
            agent,
            fast,
            supervision: None,
            verdict_fast: None,
            mcp_binary_path,
            execution_mode,
            project_path,
            event_tx,
            storage,
            app_cache: RwLock::new(HashMap::new()),
            focused_app: RwLock::new(None),
            element_cache: RwLock::new(HashMap::new()),
            context: RuntimeContext::new(),
            decision_cache: RwLock::new(decision_cache),
            cdp_connected_app: None,
            cancel_token,
            chrome_profile_store: clickweave_core::chrome_profiles::ChromeProfileStore::new(
                std::env::temp_dir().join("clickweave_test_profiles"),
            ),
            chrome_profiles: Vec::new(),
            supervision_delay_ms: 500,
        }
    }
}

/// Helper to create a WorkflowExecutor with a specific workflow for testing.
pub(super) fn make_executor_with_workflow(workflow: Workflow) -> WorkflowExecutor<StubBackend> {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let temp_dir = std::env::temp_dir().join("clickweave_test_walker");
    let storage = RunStorage::new_app_data(&temp_dir, &workflow.name, workflow.id);
    WorkflowExecutor::with_backends(
        workflow,
        StubBackend,
        None,
        String::new(),
        ExecutionMode::Run,
        None,
        tx,
        storage,
        CancellationToken::new(),
    )
}

/// Helper to create find_text match entries for click disambiguation tests.
pub(super) fn make_find_text_matches(entries: &[(&str, &str)]) -> Vec<Value> {
    entries
        .iter()
        .enumerate()
        .map(|(i, (text, role))| {
            serde_json::json!({
                "text": text,
                "role": role,
                "x": 100.0 + i as f64 * 50.0,
                "y": 200.0 + i as f64 * 50.0,
            })
        })
        .collect()
}

#[test]
fn assert_no_images_passes_for_text_only() {
    let messages = vec![
        Message::system("system prompt"),
        Message::user("hello"),
        Message::assistant("world"),
        Message::user("VLM_IMAGE_SUMMARY:\n{\"summary\": \"a screen\"}"),
    ];
    assert_no_images(&messages);
}

#[test]
#[should_panic(expected = "contains image content")]
fn assert_no_images_catches_image_parts() {
    let messages = vec![Message::user_with_images(
        "Here are images",
        vec![("base64".to_string(), "image/png".to_string())],
    )];
    assert_no_images(&messages);
}

#[test]
fn vlm_summary_replaces_images_in_message_flow() {
    use clickweave_llm::workflow_system_prompt;

    // Simulate the message flow when VLM is configured:
    // After tool results, we should append a text VLM_IMAGE_SUMMARY
    // instead of images.
    let mut messages = vec![
        Message::system(workflow_system_prompt()),
        Message::user("Click the login button"),
    ];

    // Simulate: agent made a tool call, got a result with images
    messages.push(Message::tool_result("call_1", "screenshot taken"));

    // VLM analyzed the images and produced a summary
    let vlm_summary = r#"{"summary": "Login page with username/password fields"}"#;
    messages.push(Message::user(format!(
        "VLM_IMAGE_SUMMARY:\n{}",
        vlm_summary
    )));

    // Verify: no images in the agent messages
    assert_no_images(&messages);

    // Verify: the VLM summary is present as plain text
    let last = messages.last().unwrap();
    assert!(matches!(&last.content, Some(Content::Text(t)) if t.contains("VLM_IMAGE_SUMMARY")));
}
