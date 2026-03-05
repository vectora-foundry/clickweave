use super::*;
use clickweave_core::runtime::RuntimeContext;
use clickweave_core::storage::RunStorage;
use clickweave_core::{
    ClickParams, Condition, EdgeOutput, EndLoopParams, ExecutionMode, FindTextParams, FocusMethod,
    FocusWindowParams, IfParams, LiteralValue, LoopParams, McpToolCallParams, NodeType, Operator,
    Position, ScreenshotMode, TakeScreenshotParams, TypeTextParams, ValueRef, Workflow,
};
use clickweave_llm::{ChatBackend, ChatResponse, Choice, Content, ContentPart, Message};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Mutex;
use uuid::Uuid;

/// A stub ChatBackend that never expects to be called.
/// Useful for tests that only exercise cache mechanics without LLM interaction.
struct StubBackend;

impl ChatBackend for StubBackend {
    fn model_name(&self) -> &str {
        "stub"
    }

    async fn chat(
        &self,
        _messages: Vec<Message>,
        _tools: Option<Vec<Value>>,
    ) -> anyhow::Result<ChatResponse> {
        panic!("StubBackend::chat should not be called in this test");
    }
}

/// Helper to create a `WorkflowExecutor<StubBackend>` with minimal setup.
fn make_test_executor() -> WorkflowExecutor<StubBackend> {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let workflow = Workflow::default();
    let temp_dir = std::env::temp_dir().join("clickweave_test_executor");
    let storage = RunStorage::new_app_data(&temp_dir, &workflow.name, workflow.id);
    WorkflowExecutor::with_backends(
        workflow,
        StubBackend,
        None,
        vec![],
        ExecutionMode::Run,
        None,
        tx,
        storage,
    )
}

/// A ChatBackend that returns a queue of scripted responses.
/// Used to test flows that call the LLM (e.g. resolve_element_name).
struct ScriptedBackend {
    responses: Mutex<Vec<String>>,
}

impl ScriptedBackend {
    fn new(responses: Vec<&str>) -> Self {
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

    async fn chat(
        &self,
        _messages: Vec<Message>,
        _tools: Option<Vec<Value>>,
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

fn make_scripted_executor(responses: Vec<&str>) -> WorkflowExecutor<ScriptedBackend> {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let workflow = Workflow::default();
    let temp_dir = std::env::temp_dir().join("clickweave_test_scripted");
    let storage = RunStorage::new_app_data(&temp_dir, &workflow.name, workflow.id);
    WorkflowExecutor::with_backends(
        workflow,
        ScriptedBackend::new(responses),
        None,
        vec![],
        ExecutionMode::Run,
        None,
        tx,
        storage,
    )
}

fn strs(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Check that a list of messages contains no image content parts.
fn assert_no_images(messages: &[Message]) {
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
    pub fn with_backends(
        workflow: Workflow,
        agent: C,
        vlm: Option<C>,
        mcp_configs: Vec<clickweave_mcp::McpServerConfig>,
        execution_mode: ExecutionMode,
        project_path: Option<PathBuf>,
        event_tx: Sender<ExecutorEvent>,
        storage: RunStorage,
    ) -> Self {
        let decision_cache = clickweave_core::decision_cache::DecisionCache::new(workflow.id);
        Self {
            workflow,
            agent,
            vlm,
            supervision: None,
            verdict_vlm: None,
            mcp_configs,
            execution_mode,
            project_path,
            event_tx,
            storage,
            app_cache: RwLock::new(HashMap::new()),
            focused_app: RwLock::new(None),
            element_cache: RwLock::new(HashMap::new()),
            context: RuntimeContext::new(),
            decision_cache: RwLock::new(decision_cache),
            supervision_history: RwLock::new(Vec::new()),
            runtime_verdicts: Vec::new(),
            pending_loop_exit: None,
        }
    }
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

// ---------------------------------------------------------------------------
// App cache tests
// ---------------------------------------------------------------------------

#[test]
fn evict_app_cache_removes_entry() {
    let exec = make_test_executor();

    // Insert a resolved app into the cache
    exec.app_cache.write().unwrap().insert(
        "chrome".to_string(),
        ResolvedApp {
            name: "Google Chrome".to_string(),
            pid: 1234,
        },
    );
    assert!(exec.app_cache.read().unwrap().contains_key("chrome"));

    // Evict it
    exec.evict_app_cache("chrome");
    assert!(
        !exec.app_cache.read().unwrap().contains_key("chrome"),
        "cache entry should be removed after eviction"
    );
}

#[test]
fn evict_app_cache_noop_for_missing_key() {
    let exec = make_test_executor();

    // Evicting a key that was never cached should not panic
    exec.evict_app_cache("nonexistent");
    assert!(exec.app_cache.read().unwrap().is_empty());
}

#[test]
fn evict_app_cache_leaves_other_entries() {
    let exec = make_test_executor();

    exec.app_cache.write().unwrap().insert(
        "chrome".to_string(),
        ResolvedApp {
            name: "Google Chrome".to_string(),
            pid: 1234,
        },
    );
    exec.app_cache.write().unwrap().insert(
        "firefox".to_string(),
        ResolvedApp {
            name: "Firefox".to_string(),
            pid: 5678,
        },
    );

    exec.evict_app_cache("chrome");

    assert!(
        !exec.app_cache.read().unwrap().contains_key("chrome"),
        "evicted entry should be gone"
    );
    assert!(
        exec.app_cache.read().unwrap().contains_key("firefox"),
        "other entries should remain"
    );
}

#[test]
fn evict_app_cache_for_focus_window_node() {
    let exec = make_test_executor();
    exec.app_cache.write().unwrap().insert(
        "chrome".to_string(),
        ResolvedApp {
            name: "Google Chrome".to_string(),
            pid: 1234,
        },
    );

    let node = NodeType::FocusWindow(FocusWindowParams {
        method: FocusMethod::AppName,
        value: Some("chrome".to_string()),
        bring_to_front: true,
    });
    exec.evict_caches_for_node(&node);
    assert!(!exec.app_cache.read().unwrap().contains_key("chrome"));
}

#[test]
fn evict_app_cache_for_screenshot_node() {
    let exec = make_test_executor();
    exec.app_cache.write().unwrap().insert(
        "safari".to_string(),
        ResolvedApp {
            name: "Safari".to_string(),
            pid: 999,
        },
    );

    let node = NodeType::TakeScreenshot(TakeScreenshotParams {
        mode: ScreenshotMode::Window,
        target: Some("safari".to_string()),
        include_ocr: true,
    });
    exec.evict_caches_for_node(&node);
    assert!(!exec.app_cache.read().unwrap().contains_key("safari"));
}

#[test]
fn evict_app_cache_for_unrelated_node_is_noop() {
    let exec = make_test_executor();
    exec.app_cache.write().unwrap().insert(
        "chrome".to_string(),
        ResolvedApp {
            name: "Google Chrome".to_string(),
            pid: 1234,
        },
    );

    let node = NodeType::Click(clickweave_core::ClickParams::default());
    exec.evict_caches_for_node(&node);
    assert!(exec.app_cache.read().unwrap().contains_key("chrome"));
}

// ---------------------------------------------------------------------------
// Element cache eviction tests
// ---------------------------------------------------------------------------

#[test]
fn evict_element_cache_for_click_node() {
    let exec = make_test_executor();
    let cache_key = ("×".to_string(), Some("Calculator".to_string()));
    exec.element_cache
        .write()
        .unwrap()
        .insert(cache_key.clone(), "Multiply".to_string());

    // Set focused_app so eviction uses the right cache key
    *exec.focused_app.write().unwrap() = Some("Calculator".to_string());

    let node = NodeType::Click(ClickParams {
        target: Some("×".to_string()),
        ..ClickParams::default()
    });
    exec.evict_caches_for_node(&node);
    assert!(
        !exec.element_cache.read().unwrap().contains_key(&cache_key),
        "element cache entry should be evicted for Click node"
    );
}

#[test]
fn evict_element_cache_for_find_text_node() {
    let exec = make_test_executor();
    let cache_key = ("÷".to_string(), None);
    exec.element_cache
        .write()
        .unwrap()
        .insert(cache_key.clone(), "Divide".to_string());

    let node = NodeType::FindText(FindTextParams {
        search_text: "÷".to_string(),
        ..FindTextParams::default()
    });
    exec.evict_caches_for_node(&node);
    assert!(
        !exec.element_cache.read().unwrap().contains_key(&cache_key),
        "element cache entry should be evicted for FindText node"
    );
}

#[test]
fn evict_element_cache_noop_for_unrelated_node() {
    let exec = make_test_executor();
    let cache_key = ("×".to_string(), Some("Calculator".to_string()));
    exec.element_cache
        .write()
        .unwrap()
        .insert(cache_key.clone(), "Multiply".to_string());

    let node = NodeType::TypeText(TypeTextParams {
        text: "hello".to_string(),
    });
    exec.evict_caches_for_node(&node);
    assert!(
        exec.element_cache.read().unwrap().contains_key(&cache_key),
        "element cache should not be evicted for unrelated node type"
    );
}

#[test]
fn evict_element_cache_for_mcp_find_text_node() {
    let exec = make_test_executor();
    let cache_key = ("×".to_string(), Some("Calculator".to_string()));
    exec.element_cache
        .write()
        .unwrap()
        .insert(cache_key.clone(), "Multiply".to_string());

    *exec.focused_app.write().unwrap() = Some("Calculator".to_string());

    let node = NodeType::McpToolCall(McpToolCallParams {
        tool_name: "find_text".to_string(),
        arguments: serde_json::json!({"text": "×"}),
    });
    exec.evict_caches_for_node(&node);
    assert!(
        !exec.element_cache.read().unwrap().contains_key(&cache_key),
        "element cache entry should be evicted for McpToolCall(find_text) node"
    );
}

#[test]
fn evict_element_cache_for_mcp_find_text_with_explicit_app_name() {
    let exec = make_test_executor();
    // Cache keyed to explicit app_name "Safari", not focused_app "Calculator"
    let cache_key = ("link".to_string(), Some("Safari".to_string()));
    exec.element_cache
        .write()
        .unwrap()
        .insert(cache_key.clone(), "AXLink".to_string());

    *exec.focused_app.write().unwrap() = Some("Calculator".to_string());

    let node = NodeType::McpToolCall(McpToolCallParams {
        tool_name: "find_text".to_string(),
        arguments: serde_json::json!({"text": "link", "app_name": "Safari"}),
    });
    exec.evict_caches_for_node(&node);
    assert!(
        !exec.element_cache.read().unwrap().contains_key(&cache_key),
        "element cache should use explicit app_name from arguments, not focused_app"
    );
}

// ---------------------------------------------------------------------------
// resolve_element_name integration tests (scripted LLM backend)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resolve_element_name_successful_match() {
    let exec = make_scripted_executor(vec![r#"{"name": "Multiply"}"#]);
    let available = strs(&["Calculator", "Multiply", "Divide"]);
    assert_eq!(
        exec.resolve_element_name(Uuid::new_v4(), "×", &available, Some("Calculator"), None)
            .await
            .unwrap(),
        "Multiply"
    );
}

#[tokio::test]
async fn resolve_element_name_caches_result() {
    // Only one scripted response — second call must hit cache.
    let exec = make_scripted_executor(vec![r#"{"name": "Subtract"}"#]);
    let available = strs(&["Subtract", "Add"]);
    let node_id = Uuid::new_v4();
    let first = exec
        .resolve_element_name(node_id, "−", &available, None, None)
        .await
        .unwrap();
    let second = exec
        .resolve_element_name(node_id, "−", &available, None, None)
        .await
        .unwrap();
    assert_eq!(first, "Subtract");
    assert_eq!(second, "Subtract");
}

#[tokio::test]
async fn resolve_element_name_null_match_returns_error() {
    let exec = make_scripted_executor(vec![r#"{"name": null}"#]);
    let err = exec
        .resolve_element_name(
            Uuid::new_v4(),
            "nonexistent",
            &strs(&["Multiply"]),
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(err.contains("not found"));
}

#[tokio::test]
async fn resolve_element_name_rejects_hallucinated_name() {
    let exec = make_scripted_executor(vec![r#"{"name": "Hallucinated"}"#]);
    let err = exec
        .resolve_element_name(
            Uuid::new_v4(),
            "×",
            &strs(&["Multiply", "Divide"]),
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(err.contains("not in available elements list"));
}

#[tokio::test]
async fn resolve_element_name_handles_code_block_wrapped_response() {
    let exec = make_scripted_executor(vec!["```json\n{\"name\": \"All Clear\"}\n```"]);
    assert_eq!(
        exec.resolve_element_name(
            Uuid::new_v4(),
            "AC",
            &strs(&["All Clear", "Equals"]),
            Some("Calculator"),
            None
        )
        .await
        .unwrap(),
        "All Clear"
    );
}

#[tokio::test]
async fn resolve_element_name_handles_prose_wrapped_response() {
    let exec = make_scripted_executor(vec![
        "The matching element is:\n{\"name\": \"Divide\"}\nThis maps the ÷ symbol.",
    ]);
    assert_eq!(
        exec.resolve_element_name(
            Uuid::new_v4(),
            "÷",
            &strs(&["Multiply", "Divide"]),
            None,
            None
        )
        .await
        .unwrap(),
        "Divide"
    );
}

// ---------------------------------------------------------------------------
// prepare_find_text_retry end-to-end tests (parse → LLM resolve → retry args)
// ---------------------------------------------------------------------------

const AVAILABLE_ELEMENTS_RESPONSE: &str =
    "[]\n{\"available_elements\":[\"Multiply\",\"Divide\",\"Subtract\"]}";

#[tokio::test]
async fn prepare_find_text_retry_full_flow() {
    let exec = make_scripted_executor(vec![r#"{"name": "Multiply"}"#]);
    let args = exec
        .prepare_find_text_retry(
            Uuid::new_v4(),
            &serde_json::json!({"text": "×", "app_name": "Calculator"}),
            AVAILABLE_ELEMENTS_RESPONSE,
            None,
        )
        .await
        .expect("should produce retry args");
    assert_eq!(args["text"], "Multiply");
    assert_eq!(args["app_name"], "Calculator");
}

#[tokio::test]
async fn prepare_find_text_retry_preserves_extra_fields() {
    let exec = make_scripted_executor(vec![r#"{"name": "Subtract"}"#]);
    let args = exec
        .prepare_find_text_retry(
            Uuid::new_v4(),
            &serde_json::json!({"text": "−", "app_name": "Calculator", "match_mode": "exact"}),
            "[]\n{\"available_elements\":[\"Add\",\"Subtract\"]}",
            None,
        )
        .await
        .unwrap();
    assert_eq!(args["text"], "Subtract");
    assert_eq!(args["app_name"], "Calculator");
    assert_eq!(args["match_mode"], "exact");
}

#[tokio::test]
async fn prepare_find_text_retry_falls_back_to_focused_app() {
    let exec = make_scripted_executor(vec![r#"{"name": "Multiply"}"#]);
    *exec.focused_app.write().unwrap() = Some("Calculator".to_string());

    let args = exec
        .prepare_find_text_retry(
            Uuid::new_v4(),
            &serde_json::json!({"text": "×"}),
            AVAILABLE_ELEMENTS_RESPONSE,
            None,
        )
        .await
        .unwrap();
    assert_eq!(args["text"], "Multiply");
    let cache_key = ("×".to_string(), Some("Calculator".to_string()));
    assert!(exec.element_cache.read().unwrap().contains_key(&cache_key));
}

#[tokio::test]
async fn prepare_find_text_retry_none_when_no_available_elements() {
    let exec = make_scripted_executor(vec![]);
    assert!(
        exec.prepare_find_text_retry(
            Uuid::new_v4(),
            &serde_json::json!({"text": "×"}),
            "[{\"text\":\"×\",\"x\":100,\"y\":200}]",
            None,
        )
        .await
        .is_none()
    );
}

#[tokio::test]
async fn prepare_find_text_retry_none_when_llm_finds_no_match() {
    let exec = make_scripted_executor(vec![r#"{"name": null}"#]);
    assert!(
        exec.prepare_find_text_retry(
            Uuid::new_v4(),
            &serde_json::json!({"text": "zzz"}),
            AVAILABLE_ELEMENTS_RESPONSE,
            None,
        )
        .await
        .is_none()
    );
}

// ---------------------------------------------------------------------------
// Click disambiguation tests
// ---------------------------------------------------------------------------

fn make_find_text_matches(entries: &[(&str, &str)]) -> Vec<Value> {
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

#[tokio::test]
async fn disambiguate_click_matches_picks_llm_choice() {
    let exec = make_scripted_executor(vec![r#"{"index": 1}"#]);
    let matches = make_find_text_matches(&[("2×", "AXStaticText"), ("2", "AXButton")]);
    let idx = exec
        .disambiguate_click_matches(Uuid::new_v4(), "2", &matches, Some("Calculator"), None)
        .await
        .unwrap();
    assert_eq!(idx, 1);
}

#[tokio::test]
async fn disambiguate_click_matches_out_of_bounds() {
    let exec = make_scripted_executor(vec![r#"{"index": 5}"#]);
    let matches = make_find_text_matches(&[("2×", "AXStaticText"), ("2", "AXButton")]);
    let err = exec
        .disambiguate_click_matches(Uuid::new_v4(), "2", &matches, None, None)
        .await
        .unwrap_err();
    assert!(err.contains("out-of-bounds"));
}

#[tokio::test]
async fn disambiguate_click_matches_missing_index_key() {
    let exec = make_scripted_executor(vec![r#"{"choice": 0}"#]);
    let matches = make_find_text_matches(&[("Save", "AXButton"), ("Save as...", "AXMenuItem")]);
    let err = exec
        .disambiguate_click_matches(Uuid::new_v4(), "Save", &matches, None, None)
        .await
        .unwrap_err();
    assert!(err.contains("no valid index"));
}

#[tokio::test]
async fn disambiguate_click_matches_code_block_wrapped() {
    let exec = make_scripted_executor(vec!["```json\n{\"index\": 0}\n```"]);
    let matches = make_find_text_matches(&[("OK", "AXButton"), ("OK", "AXStaticText")]);
    let idx = exec
        .disambiguate_click_matches(Uuid::new_v4(), "OK", &matches, Some("MyApp"), None)
        .await
        .unwrap();
    assert_eq!(idx, 0);
}

// ---------------------------------------------------------------------------
// Graph walker tests
// ---------------------------------------------------------------------------

/// Helper to create a WorkflowExecutor with a specific workflow for testing.
fn make_executor_with_workflow(workflow: Workflow) -> WorkflowExecutor<StubBackend> {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let temp_dir = std::env::temp_dir().join("clickweave_test_walker");
    let storage = RunStorage::new_app_data(&temp_dir, &workflow.name, workflow.id);
    WorkflowExecutor::with_backends(
        workflow,
        StubBackend,
        None,
        vec![],
        ExecutionMode::Run,
        None,
        tx,
        storage,
    )
}

/// Helper: build a dummy condition that always evaluates to true when the
/// variable "done" is set to true.
fn dummy_condition() -> Condition {
    Condition {
        left: ValueRef::Variable {
            name: "done".to_string(),
        },
        operator: Operator::Equals,
        right: ValueRef::Literal {
            value: LiteralValue::Bool { value: true },
        },
    }
}

#[test]
fn test_entry_points_linear() {
    let mut workflow = Workflow::new("test");
    let a = workflow.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 0.0, y: 0.0 },
    );
    let b = workflow.add_node(
        NodeType::TypeText(TypeTextParams::default()),
        Position { x: 100.0, y: 0.0 },
    );
    workflow.add_edge(a, b);

    let exec = make_executor_with_workflow(workflow);
    let entries = exec.entry_points();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], a);
}

#[test]
fn test_entry_points_ignores_endloop_backedge() {
    let mut workflow = Workflow::new("test-loop");

    // Loop → (body) → Click → EndLoop → (back to Loop)
    let loop_id = workflow.add_node(
        NodeType::Loop(LoopParams {
            exit_condition: dummy_condition(),
            max_iterations: 10,
        }),
        Position { x: 0.0, y: 0.0 },
    );
    let click_id = workflow.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 100.0, y: 0.0 },
    );
    let endloop_id = workflow.add_node(
        NodeType::EndLoop(EndLoopParams { loop_id }),
        Position { x: 200.0, y: 0.0 },
    );

    // Loop --LoopBody--> Click
    workflow.add_edge_with_output(loop_id, click_id, EdgeOutput::LoopBody);
    // Click --> EndLoop
    workflow.add_edge(click_id, endloop_id);
    // EndLoop --> Loop (back-edge)
    workflow.add_edge(endloop_id, loop_id);

    let exec = make_executor_with_workflow(workflow);
    let entries = exec.entry_points();

    // The Loop node should be the only entry point.
    // The EndLoop back-edge to Loop should NOT disqualify Loop as an entry point.
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], loop_id);
}

#[test]
fn test_follow_single_edge() {
    let mut workflow = Workflow::new("test");
    let a = workflow.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 0.0, y: 0.0 },
    );
    let b = workflow.add_node(
        NodeType::TypeText(TypeTextParams::default()),
        Position { x: 100.0, y: 0.0 },
    );
    workflow.add_edge(a, b);

    let exec = make_executor_with_workflow(workflow);
    assert_eq!(exec.follow_single_edge(a), Some(b));
}

#[test]
fn test_follow_single_edge_no_edge() {
    let mut workflow = Workflow::new("test");
    let a = workflow.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 0.0, y: 0.0 },
    );

    let exec = make_executor_with_workflow(workflow);
    assert_eq!(exec.follow_single_edge(a), None);
}

#[test]
fn test_follow_edge_if_true() {
    let mut workflow = Workflow::new("test-if");
    let if_id = workflow.add_node(
        NodeType::If(IfParams {
            condition: dummy_condition(),
        }),
        Position { x: 0.0, y: 0.0 },
    );
    let true_id = workflow.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 100.0, y: -50.0 },
    );
    let false_id = workflow.add_node(
        NodeType::TypeText(TypeTextParams::default()),
        Position { x: 100.0, y: 50.0 },
    );

    workflow.add_edge_with_output(if_id, true_id, EdgeOutput::IfTrue);
    workflow.add_edge_with_output(if_id, false_id, EdgeOutput::IfFalse);

    let exec = make_executor_with_workflow(workflow);

    assert_eq!(exec.follow_edge(if_id, &EdgeOutput::IfTrue), Some(true_id));
    assert_eq!(
        exec.follow_edge(if_id, &EdgeOutput::IfFalse),
        Some(false_id)
    );
    // No LoopBody edge from an If node
    assert_eq!(exec.follow_edge(if_id, &EdgeOutput::LoopBody), None);
}

#[test]
fn test_follow_edge_loop_body_and_done() {
    let mut workflow = Workflow::new("test-loop-edges");
    let loop_id = workflow.add_node(
        NodeType::Loop(LoopParams {
            exit_condition: dummy_condition(),
            max_iterations: 10,
        }),
        Position { x: 0.0, y: 0.0 },
    );
    let body_id = workflow.add_node(
        NodeType::Click(ClickParams::default()),
        Position { x: 100.0, y: -50.0 },
    );
    let done_id = workflow.add_node(
        NodeType::TypeText(TypeTextParams::default()),
        Position { x: 100.0, y: 50.0 },
    );

    workflow.add_edge_with_output(loop_id, body_id, EdgeOutput::LoopBody);
    workflow.add_edge_with_output(loop_id, done_id, EdgeOutput::LoopDone);

    let exec = make_executor_with_workflow(workflow);

    assert_eq!(
        exec.follow_edge(loop_id, &EdgeOutput::LoopBody),
        Some(body_id)
    );
    assert_eq!(
        exec.follow_edge(loop_id, &EdgeOutput::LoopDone),
        Some(done_id)
    );
    // No IfTrue edge from a Loop node
    assert_eq!(exec.follow_edge(loop_id, &EdgeOutput::IfTrue), None);
}

#[test]
fn test_runtime_context_variables_through_executor() {
    let workflow = Workflow::new("test-ctx");
    let mut exec = make_executor_with_workflow(workflow);

    // Set variables through the executor's context
    exec.context
        .set_variable("find_text.success", Value::Bool(true));
    exec.context
        .set_variable("find_text.text", Value::String("hello world".into()));

    // Verify get_variable
    assert_eq!(
        exec.context.get_variable("find_text.success"),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        exec.context.get_variable("find_text.text"),
        Some(&Value::String("hello world".into()))
    );
    assert_eq!(exec.context.get_variable("nonexistent"), None);

    // Verify condition evaluation through the context
    let cond = Condition {
        left: ValueRef::Variable {
            name: "find_text.success".to_string(),
        },
        operator: Operator::Equals,
        right: ValueRef::Literal {
            value: LiteralValue::Bool { value: true },
        },
    };
    assert!(exec.context.evaluate_condition(&cond));

    // Test with a Contains condition
    let contains_cond = Condition {
        left: ValueRef::Variable {
            name: "find_text.text".to_string(),
        },
        operator: Operator::Contains,
        right: ValueRef::Literal {
            value: LiteralValue::String {
                value: "hello".to_string(),
            },
        },
    };
    assert!(exec.context.evaluate_condition(&contains_cond));

    // Test loop counter through context
    let loop_id = uuid::Uuid::new_v4();
    exec.context.loop_counters.insert(loop_id, 3);
    assert_eq!(exec.context.loop_counters[&loop_id], 3);
}
