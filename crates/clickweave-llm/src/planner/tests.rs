use super::mapping::step_to_node_type;
use super::parse::{extract_json, layout_nodes, truncate_intent};
use super::prompt::planner_system_prompt;
use super::*;
use crate::{ChatBackend, ChatResponse, Choice, Message};
use clickweave_core::{
    ClickParams, FindTextParams, FocusMethod, FocusWindowParams, MouseButton, NodeType, Position,
    ScreenshotMode, TakeScreenshotParams,
};
use std::sync::Mutex;

// ── Test helpers ────────────────────────────────────────────────

/// Mock backend that returns a sequence of responses (for testing repair pass).
struct MockBackend {
    responses: Mutex<Vec<String>>,
    calls: Mutex<Vec<Vec<Message>>>,
}

impl MockBackend {
    fn new(responses: Vec<&str>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(String::from).collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn single(response: &str) -> Self {
        Self::new(vec![response])
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl ChatBackend for MockBackend {
    fn model_name(&self) -> &str {
        "mock"
    }

    async fn chat(
        &self,
        messages: Vec<Message>,
        _tools: Option<Vec<serde_json::Value>>,
    ) -> anyhow::Result<ChatResponse> {
        self.calls.lock().unwrap().push(messages);
        let mut responses = self.responses.lock().unwrap();
        let text = if responses.is_empty() {
            r#"{"steps": []}"#.to_string()
        } else {
            responses.remove(0)
        };
        Ok(ChatResponse {
            id: "mock".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message::assistant(&text),
                finish_reason: Some("stop".to_string()),
            }],
            usage: None,
        })
    }
}

/// Build a simple `Variable == Bool(true)` condition for tests.
fn bool_condition(var_name: &str) -> clickweave_core::Condition {
    clickweave_core::Condition {
        left: clickweave_core::ValueRef::Variable {
            name: var_name.to_string(),
        },
        operator: clickweave_core::Operator::Equals,
        right: clickweave_core::ValueRef::Literal {
            value: clickweave_core::LiteralValue::Bool { value: true },
        },
    }
}

fn sample_tools() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "focus_window",
                "description": "Focus a window",
                "parameters": {"type": "object", "properties": {"app_name": {"type": "string"}}}
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "take_screenshot",
                "description": "Take a screenshot",
                "parameters": {"type": "object", "properties": {"mode": {"type": "string"}}}
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "click",
                "description": "Click at coordinates",
                "parameters": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}}
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "find_text",
                "description": "Find text on screen",
                "parameters": {"type": "object", "properties": {"text": {"type": "string"}}}
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "type_text",
                "description": "Type text",
                "parameters": {"type": "object", "properties": {"text": {"type": "string"}}}
            }
        }),
    ]
}

/// Create a single-node workflow for patch tests. Returns the node ID and workflow.
fn single_node_workflow(node_type: NodeType, name: &str) -> (uuid::Uuid, Workflow) {
    let node = Node::new(node_type, Position { x: 300.0, y: 100.0 }, name);
    let id = node.id;
    let workflow = Workflow {
        id: uuid::Uuid::new_v4(),
        name: "Test".to_string(),
        nodes: vec![node],
        edges: vec![],
    };
    (id, workflow)
}

/// Run `patch_workflow_with_backend` with standard test defaults (no AI transforms, no agent steps).
async fn patch_with_mock(
    mock: &MockBackend,
    workflow: &Workflow,
    prompt: &str,
) -> anyhow::Result<PatchResult> {
    patch_workflow_with_backend(mock, workflow, prompt, &sample_tools(), false, false).await
}

// ── Unit tests ──────────────────────────────────────────────────

#[test]
fn test_extract_json_plain() {
    let input = r#"{"steps": []}"#;
    assert_eq!(extract_json(input), input);
}

#[test]
fn test_extract_json_code_fence() {
    let input = "```json\n{\"steps\": []}\n```";
    assert_eq!(extract_json(input), r#"{"steps": []}"#);
}

#[test]
fn test_extract_json_plain_fence() {
    let input = "```\n{\"steps\": []}\n```";
    assert_eq!(extract_json(input), r#"{"steps": []}"#);
}

#[test]
fn test_planner_system_prompt_includes_tools() {
    let tools = vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "click",
            "description": "Click at coordinates",
            "parameters": {}
        }
    })];
    let prompt = planner_system_prompt(&tools, false, false, None);
    assert!(prompt.contains("click"));
    assert!(prompt.contains("Tool"));
    assert!(!prompt.contains("step_type\": \"AiTransform\""));
    assert!(!prompt.contains("step_type\": \"AiStep\""));
}

#[test]
fn test_planner_system_prompt_with_all_features() {
    let prompt = planner_system_prompt(&[], true, true, None);
    assert!(prompt.contains("AiTransform"));
    assert!(prompt.contains("AiStep"));
}

#[test]
fn test_step_to_node_type_click() {
    let step = PlanStep::Tool {
        tool_name: "click".to_string(),
        arguments: serde_json::json!({"x": 100.0, "y": 200.0, "button": "left"}),
        name: Some("Click button".to_string()),
    };
    let (nt, name) = step_to_node_type(&step, &[]).unwrap();
    assert_eq!(name, "Click button");
    assert!(matches!(nt, NodeType::Click(_)));
}

#[test]
fn test_step_to_node_type_unknown_tool_uses_mcp_tool_call() {
    let tools = vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "custom_tool",
            "description": "A custom tool",
            "parameters": {}
        }
    })];
    let step = PlanStep::Tool {
        tool_name: "custom_tool".to_string(),
        arguments: serde_json::json!({"foo": "bar"}),
        name: None,
    };
    let (nt, _) = step_to_node_type(&step, &tools).unwrap();
    assert!(matches!(nt, NodeType::McpToolCall(_)));
}

#[test]
fn test_step_to_node_type_unknown_tool_fails_if_not_in_schema() {
    let result = step_to_node_type(
        &PlanStep::Tool {
            tool_name: "nonexistent".to_string(),
            arguments: serde_json::json!({}),
            name: None,
        },
        &[],
    );
    assert!(result.is_err());
}

#[test]
fn test_layout_nodes() {
    let positions = layout_nodes(3);
    assert_eq!(positions.len(), 3);
    assert!(positions[1].y > positions[0].y);
    assert!(positions[2].y > positions[1].y);
}

#[test]
fn test_truncate_intent() {
    assert_eq!(truncate_intent("short"), "short");
    let long = "a".repeat(60);
    let truncated = truncate_intent(&long);
    assert!(truncated.len() <= 50);
    assert!(truncated.ends_with("..."));
}

#[test]
fn test_truncate_intent_multibyte_utf8() {
    // Each emoji is 4 bytes; 13 emojis = 52 bytes > 50 limit
    let emojis = "🎉".repeat(13);
    let truncated = truncate_intent(&emojis);
    assert!(truncated.ends_with("..."));
    // Must not panic and must be valid UTF-8

    // Multi-byte char spanning the byte-47 boundary
    // 46 ASCII bytes + "é" (2 bytes) + padding = well over 50
    let mixed = format!("{}é{}", "a".repeat(46), "b".repeat(10));
    let truncated = truncate_intent(&mixed);
    assert!(truncated.ends_with("..."));
    // The "é" at byte 46-47 should be included or excluded cleanly
    assert!(!truncated.contains('\u{FFFD}')); // no replacement chars
}

#[test]
fn test_planner_prompt_includes_control_flow() {
    let prompt = planner_system_prompt(&[], false, false, None);
    assert!(
        prompt.contains("Loop"),
        "Prompt should mention Loop step type"
    );
    assert!(
        prompt.contains("EndLoop"),
        "Prompt should mention EndLoop step type"
    );
    assert!(prompt.contains("If"), "Prompt should mention If step type");
    assert!(
        prompt.contains("exit_condition"),
        "Prompt should describe exit_condition"
    );
    assert!(prompt.contains("loop_id"), "Prompt should describe loop_id");
    assert!(
        prompt.contains("\"nodes\""),
        "Prompt should describe graph output format"
    );
    assert!(
        prompt.contains("\"edges\""),
        "Prompt should describe graph output format"
    );
    assert!(
        prompt.contains(".found"),
        "Prompt should include variable examples"
    );
}

// ── Planning integration tests ──────────────────────────────────

#[tokio::test]
async fn test_plan_focus_screenshot_click() {
    let response = r#"{"steps": [
        {"step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Safari"}},
        {"step_type": "Tool", "tool_name": "take_screenshot", "arguments": {"mode": "window", "app_name": "Safari", "include_ocr": true}},
        {"step_type": "Tool", "tool_name": "find_text", "arguments": {"text": "Login"}},
        {"step_type": "Tool", "tool_name": "click", "arguments": {"x": 100, "y": 200}}
    ]}"#;
    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Focus Safari and click the Login button",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.workflow.nodes.len(), 4);
    assert_eq!(result.workflow.edges.len(), 3);
    assert!(result.warnings.is_empty());
    assert!(matches!(
        result.workflow.nodes[0].node_type,
        NodeType::FocusWindow(_)
    ));
    assert!(matches!(
        result.workflow.nodes[1].node_type,
        NodeType::TakeScreenshot(_)
    ));
    assert!(matches!(
        result.workflow.nodes[2].node_type,
        NodeType::FindText(_)
    ));
    assert!(matches!(
        result.workflow.nodes[3].node_type,
        NodeType::Click(_)
    ));
    assert_eq!(mock.call_count(), 1);
}

#[tokio::test]
async fn test_plan_with_code_fence_wrapping() {
    let response = r#"```json
{"steps": [
    {"step_type": "Tool", "tool_name": "type_text", "arguments": {"text": "hello"}}
]}
```"#;
    let mock = MockBackend::single(response);
    let result =
        plan_workflow_with_backend(&mock, "Type hello", &sample_tools(), false, false, None)
            .await
            .unwrap();

    assert_eq!(result.workflow.nodes.len(), 1);
    assert!(matches!(
        result.workflow.nodes[0].node_type,
        NodeType::TypeText(_)
    ));
}

#[tokio::test]
async fn test_plan_agent_steps_filtered_when_disabled() {
    let response = r#"{"steps": [
        {"step_type": "Tool", "tool_name": "take_screenshot", "arguments": {}},
        {"step_type": "AiStep", "prompt": "Decide what to do"}
    ]}"#;
    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Take a screenshot and decide",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.workflow.nodes.len(), 1);
    assert!(!result.warnings.is_empty());
}

#[tokio::test]
async fn test_plan_agent_steps_kept_when_enabled() {
    let response = r#"{"steps": [
        {"step_type": "Tool", "tool_name": "take_screenshot", "arguments": {}},
        {"step_type": "AiStep", "prompt": "Decide what to do", "allowed_tools": ["click"]}
    ]}"#;
    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Take a screenshot and decide",
        &sample_tools(),
        false,
        true,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.workflow.nodes.len(), 2);
    assert!(matches!(
        result.workflow.nodes[1].node_type,
        NodeType::AiStep(_)
    ));
}

#[tokio::test]
async fn test_repair_pass_fixes_invalid_json() {
    let bad_response = r#"Here is the plan: {"steps": [invalid json}]}"#;
    let good_response = r#"{"steps": [{"step_type": "Tool", "tool_name": "click", "arguments": {"x": 50, "y": 50}}]}"#;
    let mock = MockBackend::new(vec![bad_response, good_response]);

    let result = plan_workflow_with_backend(
        &mock,
        "Click somewhere",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.workflow.nodes.len(), 1);
    // Should have called the backend twice (initial + repair)
    assert_eq!(mock.call_count(), 2);
}

#[tokio::test]
async fn test_repair_pass_fails_after_max_attempts() {
    let bad = r#"not json at all"#;
    let mock = MockBackend::new(vec![bad, bad]);

    let result = plan_workflow_with_backend(
        &mock,
        "Click somewhere",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await;

    assert!(result.is_err());
    assert_eq!(mock.call_count(), 2);
}

#[tokio::test]
async fn test_plan_empty_steps_returns_error() {
    let response = r#"{"steps": []}"#;
    let mock = MockBackend::single(response);
    let result =
        plan_workflow_with_backend(&mock, "Do nothing", &sample_tools(), false, false, None).await;

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("no steps"));
}

// ── Patcher prompt tests ────────────────────────────────────────

#[test]
fn test_patcher_prompt_includes_node_arguments() {
    use super::prompt::patcher_system_prompt;

    let mut workflow = Workflow::new("Test");
    workflow.add_node(
        NodeType::FocusWindow(FocusWindowParams {
            method: FocusMethod::AppName,
            value: Some("Signal".into()),
            bring_to_front: true,
        }),
        Position { x: 0.0, y: 0.0 },
    );
    workflow.add_node(
        NodeType::FindText(FindTextParams {
            search_text: "Vesna".into(),
            ..Default::default()
        }),
        Position { x: 0.0, y: 100.0 },
    );
    workflow.add_node(
        NodeType::Click(ClickParams {
            target: Some("Vesna".into()),
            ..Default::default()
        }),
        Position { x: 0.0, y: 200.0 },
    );

    let prompt = patcher_system_prompt(&workflow, &sample_tools(), false, false);

    // Must contain the actual tool arguments so the LLM knows what to change
    assert!(
        prompt.contains("\"text\": \"Vesna\""),
        "Patcher prompt must include find_text arguments"
    );
    assert!(
        prompt.contains("\"tool_name\": \"find_text\""),
        "Patcher prompt must include tool_name"
    );
    assert!(
        prompt.contains("\"tool_name\": \"focus_window\""),
        "Patcher prompt must include focus_window tool_name"
    );
    // Click target is internal but must appear in prompt for the LLM
    assert!(
        prompt.contains("\"target\": \"Vesna\""),
        "Patcher prompt must include click target"
    );
}

// ── Patching integration tests ──────────────────────────────────

#[tokio::test]
async fn test_patch_adds_node() {
    let (_id, workflow) = single_node_workflow(
        NodeType::TakeScreenshot(TakeScreenshotParams {
            mode: ScreenshotMode::Window,
            target: None,
            include_ocr: true,
        }),
        "Screenshot",
    );

    let response = r#"{"add": [{"step_type": "Tool", "tool_name": "click", "arguments": {"x": 100, "y": 200}}]}"#;
    let mock = MockBackend::single(response);

    let result = patch_with_mock(&mock, &workflow, "Add a click after the screenshot")
        .await
        .unwrap();

    assert_eq!(result.added_nodes.len(), 1);
    assert!(matches!(
        result.added_nodes[0].node_type,
        NodeType::Click(_)
    ));
    assert_eq!(result.added_edges.len(), 1);
    assert_eq!(result.added_edges[0].from, workflow.nodes[0].id);
}

#[tokio::test]
async fn test_patch_removes_node() {
    let (node_id, workflow) = single_node_workflow(
        NodeType::Click(ClickParams {
            target: None,
            x: Some(100.0),
            y: Some(200.0),
            button: MouseButton::Left,
            click_count: 1,
        }),
        "Click",
    );

    let response = format!(r#"{{"remove_node_ids": ["{}"]}}"#, node_id);
    let mock = MockBackend::single(&response);

    let result = patch_with_mock(&mock, &workflow, "Remove the click")
        .await
        .unwrap();

    assert_eq!(result.removed_node_ids.len(), 1);
    assert_eq!(result.removed_node_ids[0], node_id);
}

#[tokio::test]
async fn test_patch_add_filters_disallowed_step_types() {
    let (_id, workflow) = single_node_workflow(
        NodeType::TakeScreenshot(TakeScreenshotParams {
            mode: ScreenshotMode::Window,
            target: None,
            include_ocr: true,
        }),
        "Screenshot",
    );

    // Patcher tries to add an AiStep and an AiTransform, but both flags are disabled
    let response = r#"{"add": [
        {"step_type": "AiStep", "prompt": "Decide what to do"},
        {"step_type": "AiTransform", "kind": "summarize", "input_ref": "step1"},
        {"step_type": "Tool", "tool_name": "click", "arguments": {"x": 50, "y": 50}}
    ]}"#;
    let mock = MockBackend::single(response);

    let result = patch_with_mock(&mock, &workflow, "Add some steps")
        .await
        .unwrap();

    // Only the Tool step should survive
    assert_eq!(result.added_nodes.len(), 1);
    assert!(matches!(
        result.added_nodes[0].node_type,
        NodeType::Click(_)
    ));
    assert!(result.warnings.len() >= 2);
}

#[tokio::test]
async fn test_patch_update_with_flat_arguments_only() {
    let (node_id, workflow) = single_node_workflow(
        NodeType::FindText(FindTextParams {
            search_text: "Vesna".into(),
            ..Default::default()
        }),
        "Find Vesna",
    );

    // LLM returns only `arguments` (no tool_name, no node_type) -- tool inferred from existing node
    let response = format!(
        r#"{{"update": [{{"node_id": "{}", "name": "Find Me", "arguments": {{"text": "Me"}}}}]}}"#,
        node_id
    );
    let mock = MockBackend::single(&response);

    let result = patch_with_mock(&mock, &workflow, "Change target")
        .await
        .unwrap();

    assert_eq!(result.updated_nodes.len(), 1);
    assert_eq!(result.updated_nodes[0].name, "Find Me");
    match &result.updated_nodes[0].node_type {
        NodeType::FindText(p) => assert_eq!(p.search_text, "Me"),
        other => panic!("Expected FindText, got {:?}", other),
    }
}

#[tokio::test]
async fn test_patch_update_with_flat_tool_name_and_arguments() {
    let (node_id, workflow) = single_node_workflow(
        NodeType::FindText(FindTextParams {
            search_text: "old".into(),
            ..Default::default()
        }),
        "Find Old",
    );

    // LLM returns `tool_name` + `arguments` (no nested node_type)
    let response = format!(
        r#"{{"update": [{{"node_id": "{}", "tool_name": "type_text", "arguments": {{"text": "hello"}}}}]}}"#,
        node_id
    );
    let mock = MockBackend::single(&response);

    let result = patch_with_mock(&mock, &workflow, "Change to type_text")
        .await
        .unwrap();

    assert_eq!(result.updated_nodes.len(), 1);
    match &result.updated_nodes[0].node_type {
        NodeType::TypeText(p) => assert_eq!(p.text, "hello"),
        other => panic!("Expected TypeText, got {:?}", other),
    }
}

#[tokio::test]
async fn test_patch_update_rejects_disallowed_node_type_change() {
    let (node_id, workflow) = single_node_workflow(
        NodeType::Click(ClickParams {
            target: None,
            x: Some(100.0),
            y: Some(200.0),
            button: MouseButton::Left,
            click_count: 1,
        }),
        "Click",
    );

    // Try to update the node to an AiStep with agent steps disabled
    let response = format!(
        r#"{{"update": [{{"node_id": "{}", "node_type": {{"step_type": "AiStep", "prompt": "do something"}}}}]}}"#,
        node_id
    );
    let mock = MockBackend::single(&response);

    let result = patch_with_mock(&mock, &workflow, "Change to AI")
        .await
        .unwrap();

    // Node should still be in updated_nodes (name update still applies) but type unchanged
    assert_eq!(result.updated_nodes.len(), 1);
    assert!(matches!(
        result.updated_nodes[0].node_type,
        NodeType::Click(_)
    ));
    assert!(!result.warnings.is_empty());
}

#[tokio::test]
async fn test_patch_repair_pass_fixes_invalid_json() {
    let (_id, workflow) = single_node_workflow(
        NodeType::TakeScreenshot(TakeScreenshotParams {
            mode: ScreenshotMode::Window,
            target: None,
            include_ocr: true,
        }),
        "Screenshot",
    );

    let bad_response = r#"Here's the patch: {"add": [invalid}]}"#;
    let good_response = r#"{"add": [{"step_type": "Tool", "tool_name": "click", "arguments": {"x": 50, "y": 50}}]}"#;
    let mock = MockBackend::new(vec![bad_response, good_response]);

    let result = patch_with_mock(&mock, &workflow, "Add a click")
        .await
        .unwrap();

    assert_eq!(result.added_nodes.len(), 1);
    assert_eq!(mock.call_count(), 2);
}

#[tokio::test]
async fn test_patch_repair_pass_fails_after_max_attempts() {
    let (_id, workflow) = single_node_workflow(
        NodeType::TakeScreenshot(TakeScreenshotParams {
            mode: ScreenshotMode::Window,
            target: None,
            include_ocr: true,
        }),
        "Screenshot",
    );

    let bad = r#"not json at all"#;
    let mock = MockBackend::new(vec![bad, bad]);

    let result = patch_with_mock(&mock, &workflow, "Add a click").await;

    assert!(result.is_err());
    assert_eq!(mock.call_count(), 2);
}

// ── Conversation tests ─────────────────────────────────────────

#[test]
fn test_conversation_recent_window_small() {
    use super::conversation::*;
    let mut session = ConversationSession::new();
    session.push_user("hello".into(), None);
    session.push_assistant("hi".into(), None);
    assert_eq!(session.recent_window(None).len(), 2);
    assert!(!session.needs_summarization(None));
}

#[test]
fn test_conversation_recent_window_overflow() {
    use super::conversation::*;
    let mut session = ConversationSession::new();
    for i in 0..8 {
        session.push_user(format!("q{}", i), None);
        session.push_assistant(format!("a{}", i), None);
    }
    let window = session.recent_window(Some(3));
    assert_eq!(window.len(), 6);
    assert_eq!(window[0].content, "q5");
    assert!(session.needs_summarization(Some(3)));
    assert_eq!(session.unsummarized_overflow(Some(3)).len(), 10);
}

#[test]
fn test_conversation_set_summary_updates_cutoff() {
    use super::conversation::*;
    let mut session = ConversationSession::new();
    for i in 0..8 {
        session.push_user(format!("q{}", i), None);
        session.push_assistant(format!("a{}", i), None);
    }
    session.set_summary("summary of q0-q4".into(), Some(3));
    assert_eq!(session.summary_cutoff, 10);
    assert!(!session.needs_summarization(Some(3)));
}

#[tokio::test]
async fn test_summarize_overflow_produces_summary() {
    use super::conversation::*;
    use super::summarize::summarize_overflow;

    let mut session = ConversationSession::new();
    for i in 0..8 {
        session.push_user(format!("add step {}", i), None);
        session.push_assistant(format!("added step {}", i), None);
    }

    let mock = MockBackend::single("User added 8 steps to the workflow iteratively.");
    let summary = summarize_overflow(&mock, &session, Some(3)).await.unwrap();
    assert!(!summary.is_empty());
    assert_eq!(mock.call_count(), 1);
}

#[tokio::test]
async fn test_summarize_overflow_noop_when_no_overflow() {
    use super::conversation::*;
    use super::summarize::summarize_overflow;

    let mut session = ConversationSession::new();
    session.push_user("hello".into(), None);
    session.push_assistant("hi".into(), None);

    let mock = MockBackend::single("should not be called");
    let summary = summarize_overflow(&mock, &session, None).await.unwrap();
    assert!(summary.is_empty());
    assert_eq!(mock.call_count(), 0);
}

// ── Assistant chat tests ───────────────────────────────────────

#[tokio::test]
async fn test_assistant_chat_plans_empty_workflow() {
    use super::assistant::assistant_chat_with_backend;
    use super::conversation::ConversationSession;

    let response = r#"{"steps": [
        {"step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}},
        {"step_type": "Tool", "tool_name": "click", "arguments": {"x": 100, "y": 200}}
    ]}"#;
    let mock = MockBackend::single(response);
    let workflow = Workflow::new("Test");
    let session = ConversationSession::new();

    let result = assistant_chat_with_backend(
        &mock,
        &workflow,
        "Open calculator and click a button",
        &session,
        None,
        &sample_tools(),
        false,
        false,
        0,
        None,
    )
    .await
    .unwrap();

    assert!(result.patch.is_some());
    let patch = result.patch.unwrap();
    assert_eq!(patch.added_nodes.len(), 2);
    assert!(result.warnings.is_empty());
}

#[tokio::test]
async fn test_assistant_chat_patches_existing_workflow() {
    use super::assistant::assistant_chat_with_backend;
    use super::conversation::ConversationSession;

    let (_id, workflow) = single_node_workflow(
        NodeType::TakeScreenshot(TakeScreenshotParams {
            mode: ScreenshotMode::Window,
            target: None,
            include_ocr: true,
        }),
        "Screenshot",
    );

    let response = r#"{"add": [{"step_type": "Tool", "tool_name": "click", "arguments": {"x": 50, "y": 50}}]}"#;
    let mock = MockBackend::single(response);
    let session = ConversationSession::new();

    let result = assistant_chat_with_backend(
        &mock,
        &workflow,
        "Add a click after the screenshot",
        &session,
        None,
        &sample_tools(),
        false,
        false,
        0,
        None,
    )
    .await
    .unwrap();

    assert!(result.patch.is_some());
    assert_eq!(result.patch.unwrap().added_nodes.len(), 1);
}

#[tokio::test]
async fn test_assistant_chat_conversational_response() {
    use super::assistant::assistant_chat_with_backend;
    use super::conversation::ConversationSession;

    let (_id, workflow) = single_node_workflow(
        NodeType::TakeScreenshot(TakeScreenshotParams {
            mode: ScreenshotMode::Window,
            target: None,
            include_ocr: true,
        }),
        "Screenshot",
    );

    let response = "The workflow currently has one step that takes a screenshot. Would you like me to add more steps?";
    let mock = MockBackend::single(response);
    let session = ConversationSession::new();

    let result = assistant_chat_with_backend(
        &mock,
        &workflow,
        "What does my workflow do?",
        &session,
        None,
        &sample_tools(),
        false,
        false,
        0,
        None,
    )
    .await
    .unwrap();

    assert!(result.patch.is_none());
    assert!(result.message.contains("screenshot"));
}

// ── Control flow PlanStep parsing tests ─────────────────────────

#[test]
fn test_parse_loop_plan_step() {
    let json = r#"{
        "step_type": "Loop",
        "name": "Multiply Loop",
        "exit_condition": {
            "left": {"type": "Variable", "name": "check_result.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        },
        "max_iterations": 20
    }"#;
    let step: PlanStep = serde_json::from_str(json).unwrap();
    assert!(matches!(step, PlanStep::Loop { .. }));
}

#[test]
fn test_parse_end_loop_plan_step() {
    let json = r#"{"step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"}"#;
    let step: PlanStep = serde_json::from_str(json).unwrap();
    assert!(matches!(step, PlanStep::EndLoop { .. }));
}

#[test]
fn test_parse_if_plan_step() {
    let json = r#"{
        "step_type": "If",
        "name": "Check Found",
        "condition": {
            "left": {"type": "Variable", "name": "find_text.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }
    }"#;
    let step: PlanStep = serde_json::from_str(json).unwrap();
    assert!(matches!(step, PlanStep::If { .. }));
}

#[test]
fn test_parse_planner_graph_output() {
    let json = r#"{
        "nodes": [
            {"id": "n1", "step_type": "Tool", "tool_name": "launch_app", "arguments": {"app_name": "Calculator"}, "name": "Launch Calculator"},
            {"id": "n2", "step_type": "Loop", "name": "Multiply", "exit_condition": {
                "left": {"type": "Variable", "name": "check.found"},
                "operator": "Equals",
                "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
            }, "max_iterations": 20},
            {"id": "n3", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "="}, "name": "Click Equals"},
            {"id": "n4", "step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"}
        ],
        "edges": [
            {"from": "n1", "to": "n2"},
            {"from": "n2", "to": "n3", "output": {"type": "LoopBody"}},
            {"from": "n2", "to": "n1", "output": {"type": "LoopDone"}},
            {"from": "n3", "to": "n4"},
            {"from": "n4", "to": "n2"}
        ]
    }"#;
    let output: PlannerGraphOutput = serde_json::from_str(json).unwrap();
    assert_eq!(output.nodes.len(), 4);
    assert_eq!(output.edges.len(), 5);
    let node1: PlanNode = serde_json::from_value(output.nodes[1].clone()).unwrap();
    assert!(matches!(node1.step, PlanStep::Loop { .. }));
    let edge1: PlanEdge = serde_json::from_value(output.edges[1].clone()).unwrap();
    assert_eq!(edge1.output, Some(clickweave_core::EdgeOutput::LoopBody));
}

#[tokio::test]
async fn test_patch_adds_loop() {
    let (_id, workflow) = single_node_workflow(
        NodeType::FocusWindow(FocusWindowParams {
            method: FocusMethod::AppName,
            value: Some("Calculator".into()),
            bring_to_front: true,
        }),
        "Focus Calculator",
    );

    let response = format!(
        r#"{{
        "add_nodes": [
            {{"id": "n1", "step_type": "Loop", "name": "Repeat", "exit_condition": {{
                "left": {{"type": "Variable", "name": "check.found"}},
                "operator": "Equals",
                "right": {{"type": "Literal", "value": {{"type": "Bool", "value": true}}}}
            }}, "max_iterations": 10}},
            {{"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {{"target": "="}}, "name": "Click"}},
            {{"id": "n3", "step_type": "EndLoop", "loop_id": "n1", "name": "End Loop"}}
        ],
        "add_edges": [
            {{"from": "{}", "to": "n1"}},
            {{"from": "n1", "to": "n2", "output": {{"type": "LoopBody"}}}},
            {{"from": "n2", "to": "n3"}},
            {{"from": "n3", "to": "n1"}}
        ]
    }}"#,
        workflow.nodes[0].id
    );

    let mock = MockBackend::single(&response);
    let result = patch_with_mock(&mock, &workflow, "Add a loop")
        .await
        .unwrap();

    assert_eq!(result.added_nodes.len(), 3);
    assert!(
        result
            .added_nodes
            .iter()
            .any(|n| matches!(n.node_type, NodeType::Loop(_)))
    );
    assert!(
        result
            .added_nodes
            .iter()
            .any(|n| matches!(n.node_type, NodeType::EndLoop(_)))
    );
    // Verify edges were created (from existing node to new loop + internal edges)
    assert!(result.added_edges.len() >= 3);
}

// ── Control flow mapping tests ──────────────────────────────────

#[test]
fn test_step_to_node_type_loop() {
    let step = PlanStep::Loop {
        name: Some("Repeat".to_string()),
        exit_condition: bool_condition("check.found"),
        max_iterations: Some(20),
    };
    let (nt, name) = step_to_node_type(&step, &[]).unwrap();
    assert_eq!(name, "Repeat");
    assert!(matches!(nt, NodeType::Loop(_)));
    if let NodeType::Loop(p) = nt {
        assert_eq!(p.max_iterations, 20);
    }
}

#[test]
fn test_step_to_node_type_end_loop() {
    let step = PlanStep::EndLoop {
        name: Some("End Loop".to_string()),
        loop_id: "n2".to_string(),
    };
    let (nt, name) = step_to_node_type(&step, &[]).unwrap();
    assert_eq!(name, "End Loop");
    assert!(matches!(nt, NodeType::EndLoop(_)));
}

#[test]
fn test_step_to_node_type_if() {
    let step = PlanStep::If {
        name: Some("Check Result".to_string()),
        condition: bool_condition("find_text.found"),
    };
    let (nt, name) = step_to_node_type(&step, &[]).unwrap();
    assert_eq!(name, "Check Result");
    assert!(matches!(nt, NodeType::If(_)));
}

#[test]
fn test_control_flow_steps_never_rejected() {
    use super::parse::step_rejected_reason;

    let condition = bool_condition("x.found");

    let loop_step = PlanStep::Loop {
        name: None,
        exit_condition: condition.clone(),
        max_iterations: Some(10),
    };
    let end_loop_step = PlanStep::EndLoop {
        name: None,
        loop_id: "n1".into(),
    };
    let if_step = PlanStep::If {
        name: None,
        condition,
    };

    // Even with all features disabled, control flow steps pass through
    assert!(step_rejected_reason(&loop_step, false, false).is_none());
    assert!(step_rejected_reason(&end_loop_step, false, false).is_none());
    assert!(step_rejected_reason(&if_step, false, false).is_none());
}

// ── Assistant prompt tests ──────────────────────────────────────

#[test]
fn test_assistant_prompt_empty_workflow_includes_control_flow() {
    use super::prompt::assistant_system_prompt;

    let wf = Workflow::new("Test");
    let prompt = assistant_system_prompt(&wf, &[], false, false, None);
    assert!(
        prompt.contains("Loop"),
        "Assistant prompt should mention Loop"
    );
    assert!(
        prompt.contains("EndLoop"),
        "Assistant prompt should mention EndLoop"
    );
}

#[test]
fn test_assistant_prompt_existing_workflow_includes_control_flow() {
    use super::prompt::assistant_system_prompt;

    let (_, workflow) = single_node_workflow(NodeType::Click(ClickParams::default()), "Click");
    let prompt = assistant_system_prompt(&workflow, &[], false, false, None);
    assert!(
        prompt.contains("add_nodes"),
        "Patcher assistant prompt should mention add_nodes for control flow"
    );
}

// ── Full integration test ───────────────────────────────────────

#[tokio::test]
async fn test_plan_calculator_loop_scenario() {
    // Simulates what the LLM should produce for:
    // "Open the calculator app and keep calculating 2x2 until you get to 1024"
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus Calculator"},
        {"id": "n2", "step_type": "Loop", "name": "Multiply Loop", "exit_condition": {
            "left": {"type": "Variable", "name": "check_for_1024.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "max_iterations": 20},
        {"id": "n3", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2"},
        {"id": "n4", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "×"}, "name": "Click Multiply"},
        {"id": "n5", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2 Again"},
        {"id": "n6", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "="}, "name": "Click Equals"},
        {"id": "n7", "step_type": "Tool", "tool_name": "find_text", "arguments": {"text": "1024", "app_name": "Calculator"}, "name": "Check for 1024"},
        {"id": "n8", "step_type": "EndLoop", "loop_id": "n2", "name": "End Multiply Loop"},
        {"id": "n9", "step_type": "Tool", "tool_name": "take_screenshot", "arguments": {"app_name": "Calculator"}, "name": "Final Screenshot"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3", "output": {"type": "LoopBody"}},
        {"from": "n2", "to": "n9", "output": {"type": "LoopDone"}},
        {"from": "n3", "to": "n4"},
        {"from": "n4", "to": "n5"},
        {"from": "n5", "to": "n6"},
        {"from": "n6", "to": "n7"},
        {"from": "n7", "to": "n8"},
        {"from": "n8", "to": "n2"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Open the calculator app and keep calculating 2x2 until you get to 1024",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    let wf = &result.workflow;
    assert_eq!(wf.nodes.len(), 9);
    assert_eq!(wf.edges.len(), 9);

    // Verify structure
    let loop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Loop(_)))
        .unwrap();
    let end_loop = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::EndLoop(_)))
        .unwrap();
    if let NodeType::EndLoop(p) = &end_loop.node_type {
        assert_eq!(
            p.loop_id, loop_node.id,
            "EndLoop must reference Loop's UUID"
        );
    }
    if let NodeType::Loop(p) = &loop_node.node_type {
        assert_eq!(p.max_iterations, 20);
    }

    // Verify LoopBody and LoopDone edges on Loop node
    let loop_edges: Vec<_> = wf.edges.iter().filter(|e| e.from == loop_node.id).collect();
    assert_eq!(loop_edges.len(), 2);
    assert!(
        loop_edges
            .iter()
            .any(|e| e.output == Some(clickweave_core::EdgeOutput::LoopBody))
    );
    assert!(
        loop_edges
            .iter()
            .any(|e| e.output == Some(clickweave_core::EdgeOutput::LoopDone))
    );

    assert!(result.warnings.is_empty());
}

// ── R2 regression tests: assistant control-flow parsing ─────────

#[tokio::test]
async fn test_assistant_patches_with_add_nodes_and_add_edges() {
    use super::assistant::assistant_chat_with_backend;
    use super::conversation::ConversationSession;

    let (_id, workflow) = single_node_workflow(
        NodeType::FocusWindow(FocusWindowParams {
            method: FocusMethod::AppName,
            value: Some("Calculator".to_string()),
            bring_to_front: true,
        }),
        "Focus Calculator",
    );

    // Patcher output using add_nodes + add_edges (control-flow patch)
    let response = r#"{"add_nodes": [
        {"id": "n1", "step_type": "Loop", "name": "My Loop", "exit_condition": {
            "left": {"type": "Variable", "name": "done"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"x": 10, "y": 20}, "name": "Click"},
        {"id": "n3", "step_type": "EndLoop", "loop_id": "n1", "name": "End Loop"}
    ], "add_edges": [
        {"from": "n1", "to": "n2", "output": {"type": "LoopBody"}},
        {"from": "n1", "to": "DONE", "output": {"type": "LoopDone"}},
        {"from": "n2", "to": "n3"},
        {"from": "n3", "to": "n1"}
    ]}"#;
    let mock = MockBackend::single(response);
    let session = ConversationSession::new();

    let result = assistant_chat_with_backend(
        &mock,
        &workflow,
        "Wrap this in a loop",
        &session,
        None,
        &sample_tools(),
        false,
        false,
        0,
        None,
    )
    .await
    .unwrap();

    assert!(
        result.patch.is_some(),
        "control-flow patch should not be dropped"
    );
    let patch = result.patch.unwrap();
    assert_eq!(patch.added_nodes.len(), 3);
    // LoopBody + 2 linear + back-edge (LoopDone to "DONE" is skipped by design)
    assert_eq!(patch.added_edges.len(), 3);
    assert!(
        result.warnings.is_empty(),
        "unexpected warnings: {:?}",
        result.warnings
    );
}

#[tokio::test]
async fn test_assistant_plans_graph_format_for_empty_workflow() {
    use super::assistant::assistant_chat_with_backend;
    use super::conversation::ConversationSession;

    // Graph-format planner output (control-flow plan for empty workflow)
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n2", "step_type": "Loop", "name": "Loop", "exit_condition": {
            "left": {"type": "Variable", "name": "done"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }},
        {"id": "n3", "step_type": "Tool", "tool_name": "click", "arguments": {"x": 1, "y": 2}, "name": "Click"},
        {"id": "n4", "step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3", "output": {"type": "LoopBody"}},
        {"from": "n2", "to": "DONE", "output": {"type": "LoopDone"}},
        {"from": "n3", "to": "n4"},
        {"from": "n4", "to": "n2"}
    ]}"#;
    let mock = MockBackend::single(response);
    let workflow = Workflow::new("Test");
    let session = ConversationSession::new();

    let result = assistant_chat_with_backend(
        &mock,
        &workflow,
        "Create a loop workflow for the calculator",
        &session,
        None,
        &sample_tools(),
        false,
        false,
        0,
        None,
    )
    .await
    .unwrap();

    assert!(
        result.patch.is_some(),
        "graph-format plan should produce a patch"
    );
    let patch = result.patch.unwrap();
    assert_eq!(patch.added_nodes.len(), 4);
    // n1→n2, LoopBody, 2 linear (n3→n4), back-edge (n4→n2) = 4 (LoopDone to "DONE" skipped)
    assert_eq!(patch.added_edges.len(), 4);
    assert!(
        result.warnings.is_empty(),
        "unexpected warnings: {:?}",
        result.warnings
    );
}

#[test]
fn test_mixed_add_and_add_nodes_warns_and_skips_flat() {
    let (_id, workflow) = single_node_workflow(
        NodeType::FocusWindow(FocusWindowParams {
            method: FocusMethod::AppName,
            value: Some("Calculator".to_string()),
            bring_to_front: true,
        }),
        "Focus Calculator",
    );

    let output: PatcherOutput = serde_json::from_str(r#"{
        "add": [{"step_type": "Tool", "tool_name": "click", "arguments": {"x": 10, "y": 20}}],
        "add_nodes": [
            {"id": "n1", "step_type": "Tool", "tool_name": "click", "arguments": {"x": 30, "y": 40}, "name": "Click 2"}
        ],
        "add_edges": []
    }"#).unwrap();

    let patch = build_patch_from_output(&output, &workflow, &sample_tools(), false, false);

    // Only graph-based node should be added; flat add should be skipped
    assert_eq!(
        patch.added_nodes.len(),
        1,
        "flat add items should be skipped"
    );
    assert!(
        patch
            .warnings
            .iter()
            .any(|w| w.contains("flat 'add' steps")),
        "should warn about ignored flat adds: {:?}",
        patch.warnings,
    );
}

// ── Flat plan loop tests ────────────────────────────────────────

#[test]
fn test_flat_plan_with_loop_gets_control_flow_edges() {
    // Flat plans (sequential steps) with Loop/EndLoop need
    // infer_control_flow_edges to add LoopBody/LoopDone labels
    // and EndLoop→Loop back-edges.
    let raw_steps: Vec<serde_json::Value> = serde_json::from_str(r#"[
        {"step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"step_type": "Loop", "name": "Repeat", "exit_condition": {
            "left": {"type": "Variable", "name": "click_2.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "max_iterations": 10},
        {"step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2"},
        {"step_type": "Tool", "tool_name": "click", "arguments": {"target": "="}, "name": "Click Equals"},
        {"step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"}
    ]"#)
    .unwrap();

    let patch = super::build_plan_as_patch(&raw_steps, &sample_tools(), false, false);
    assert!(
        patch.warnings.is_empty(),
        "Unexpected warnings: {:?}",
        patch.warnings
    );

    // Build a workflow from the patch
    let mut wf = clickweave_core::Workflow::new("test");
    wf.nodes = patch.added_nodes;
    wf.edges = patch.added_edges;

    let loop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Loop(_)))
        .unwrap();
    let endloop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::EndLoop(_)))
        .unwrap();

    // Loop should have LoopBody edge
    let loop_body = wf
        .edges
        .iter()
        .find(|e| e.from == loop_node.id && e.output == Some(EdgeOutput::LoopBody));
    assert!(loop_body.is_some(), "Loop should have a LoopBody edge");

    // LoopDone is optional for terminal loops (no steps after EndLoop)

    // EndLoop should have back-edge to Loop
    let back_edge = wf
        .edges
        .iter()
        .find(|e| e.from == endloop_node.id && e.to == loop_node.id);
    assert!(back_edge.is_some(), "EndLoop should have back-edge to Loop");

    // Workflow should pass validation
    clickweave_core::validate_workflow(&wf).expect("Flat plan with loop should pass validation");
}

#[test]
fn test_flat_plan_with_post_loop_node_gets_loop_done() {
    // Flat plan: [Focus, Loop, Click, EndLoop, Screenshot]
    // EndLoop→Screenshot should be converted to Loop→Screenshot (LoopDone).
    let raw_steps: Vec<serde_json::Value> = serde_json::from_str(r#"[
        {"step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"step_type": "Loop", "name": "Repeat", "exit_condition": {
            "left": {"type": "Variable", "name": "click_2.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "max_iterations": 10},
        {"step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2"},
        {"step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"},
        {"step_type": "Tool", "tool_name": "take_screenshot", "arguments": {}, "name": "Final Screenshot"}
    ]"#)
    .unwrap();

    let patch = super::build_plan_as_patch(&raw_steps, &sample_tools(), false, false);
    assert!(
        patch.warnings.is_empty(),
        "Unexpected warnings: {:?}",
        patch.warnings
    );

    let mut wf = clickweave_core::Workflow::new("test");
    wf.nodes = patch.added_nodes;
    wf.edges = patch.added_edges;

    let loop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Loop(_)))
        .unwrap();
    let screenshot_node = wf
        .nodes
        .iter()
        .find(|n| n.name == "Final Screenshot")
        .unwrap();

    // Loop should have LoopDone edge pointing to Screenshot
    let loop_done = wf
        .edges
        .iter()
        .find(|e| e.from == loop_node.id && e.output == Some(EdgeOutput::LoopDone));
    assert!(loop_done.is_some(), "Loop should have a LoopDone edge");
    assert_eq!(
        loop_done.unwrap().to,
        screenshot_node.id,
        "LoopDone should target the post-loop node"
    );

    clickweave_core::validate_workflow(&wf)
        .expect("Flat plan with post-loop node should pass validation");
}

// ── Edge inference tests ────────────────────────────────────────

#[tokio::test]
async fn test_infer_loop_edges_from_unlabeled() {
    // LLM produces unlabeled Loop edges — inference should label them.
    // Pattern: Loop→body (unlabeled), Loop→EndLoop (unlabeled)
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n2", "step_type": "Loop", "name": "My Loop", "exit_condition": {
            "left": {"type": "Variable", "name": "click_equals.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "max_iterations": 10},
        {"id": "n3", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "="}, "name": "Click Equals"},
        {"id": "n4", "step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"},
        {"id": "n5", "step_type": "Tool", "tool_name": "take_screenshot", "arguments": {}, "name": "Done Screenshot"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"},
        {"from": "n2", "to": "n5"},
        {"from": "n3", "to": "n4"},
        {"from": "n4", "to": "n2"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result =
        plan_workflow_with_backend(&mock, "Loop test", &sample_tools(), false, false, None)
            .await
            .unwrap();

    let wf = &result.workflow;
    let loop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Loop(_)))
        .unwrap();

    let loop_edges: Vec<_> = wf.edges.iter().filter(|e| e.from == loop_node.id).collect();
    assert_eq!(loop_edges.len(), 2);
    assert!(
        loop_edges
            .iter()
            .any(|e| e.output == Some(EdgeOutput::LoopBody)),
        "Should infer LoopBody edge"
    );
    assert!(
        loop_edges
            .iter()
            .any(|e| e.output == Some(EdgeOutput::LoopDone)),
        "Should infer LoopDone edge"
    );
}

#[tokio::test]
async fn test_infer_loop_reroutes_back_edge_through_endloop() {
    // LLM produces: body_end→Loop (bypassing EndLoop), Loop→EndLoop (as exit).
    // This is the exact pattern from the calculator bug.
    // Expected fix: body_end→EndLoop, EndLoop→Loop, LoopDone removed (terminal).
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n2", "step_type": "Loop", "name": "Multiply", "exit_condition": {
            "left": {"type": "Variable", "name": "check_result.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "max_iterations": 20},
        {"id": "n3", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2"},
        {"id": "n4", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "="}, "name": "Click Equals"},
        {"id": "n5", "step_type": "Tool", "tool_name": "find_text", "arguments": {"text": "1024"}, "name": "Check Result"},
        {"id": "n6", "step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"},
        {"from": "n3", "to": "n4"},
        {"from": "n4", "to": "n5"},
        {"from": "n5", "to": "n2"},
        {"from": "n2", "to": "n6"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Calculator multiply loop",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    let wf = &result.workflow;
    let loop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Loop(_)))
        .unwrap();
    let endloop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::EndLoop(_)))
        .unwrap();

    // Loop should have LoopBody edge (to Click 2) but no LoopDone (removed: targeted EndLoop)
    let loop_edges: Vec<_> = wf.edges.iter().filter(|e| e.from == loop_node.id).collect();
    assert_eq!(loop_edges.len(), 1, "Terminal loop: only LoopBody edge");
    assert_eq!(
        loop_edges[0].output,
        Some(EdgeOutput::LoopBody),
        "The single Loop edge should be LoopBody"
    );

    // EndLoop should have back-edge to Loop
    let endloop_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == endloop_node.id)
        .collect();
    assert_eq!(
        endloop_edges.len(),
        1,
        "EndLoop should have one outgoing edge"
    );
    assert_eq!(
        endloop_edges[0].to, loop_node.id,
        "EndLoop should point back to Loop"
    );

    // Check Result (n5) should connect to EndLoop, not directly to Loop
    let check_node = wf.nodes.iter().find(|n| n.name == "Check Result").unwrap();
    let check_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == check_node.id)
        .collect();
    assert_eq!(check_edges.len(), 1);
    assert_eq!(
        check_edges[0].to, endloop_node.id,
        "Back-edge should be rerouted through EndLoop"
    );
    assert_eq!(
        check_edges[0].output, None,
        "Rerouted edge should be a regular edge (no output label)"
    );
}

#[tokio::test]
async fn test_infer_loop_clears_stale_output_on_rerouted_back_edge() {
    // LLM produces: body_end→Loop with LoopBody label (stale), plus EndLoop node.
    // This is the exact pattern from the 2x2=128 calculator bug where the LLM
    // labeled the back-edge as LoopBody instead of routing through EndLoop.
    // Without clearing the output, follow_single_edge can't find the edge.
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "launch_app", "arguments": {"app_name": "Calculator"}, "name": "Launch"},
        {"id": "n2", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n3", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2 Setup"},
        {"id": "n4", "step_type": "Loop", "name": "Multiply Loop", "exit_condition": {
            "left": {"type": "Variable", "name": "click_equals.result"},
            "operator": "GreaterThan",
            "right": {"type": "Literal", "value": {"type": "Number", "value": 128}}
        }, "max_iterations": 10},
        {"id": "n5", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "×"}, "name": "Click Multiply"},
        {"id": "n6", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2"},
        {"id": "n7", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "="}, "name": "Click Equals"},
        {"id": "n8", "step_type": "EndLoop", "loop_id": "n4", "name": "End Loop"},
        {"id": "n9", "step_type": "Tool", "tool_name": "take_screenshot", "arguments": {}, "name": "Screenshot"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"},
        {"from": "n3", "to": "n4"},
        {"from": "n4", "to": "n5", "output": {"type": "LoopBody"}},
        {"from": "n5", "to": "n6"},
        {"from": "n6", "to": "n7"},
        {"from": "n7", "to": "n4", "output": {"type": "LoopBody"}},
        {"from": "n4", "to": "n9", "output": {"type": "LoopDone"}}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Calculator 2x2 loop",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    let wf = &result.workflow;
    let endloop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::EndLoop(_)))
        .unwrap();

    // Click Equals should connect to EndLoop with a regular (unlabeled) edge
    let equals_node = wf.nodes.iter().find(|n| n.name == "Click Equals").unwrap();
    let equals_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == equals_node.id)
        .collect();
    assert_eq!(equals_edges.len(), 1, "Click Equals should have one edge");
    assert_eq!(
        equals_edges[0].to, endloop_node.id,
        "Click Equals should connect to EndLoop"
    );
    assert_eq!(
        equals_edges[0].output, None,
        "Rerouted edge must have output cleared (was LoopBody)"
    );
}

#[tokio::test]
async fn test_infer_if_edges_from_unlabeled() {
    // LLM produces If node with two unlabeled edges — first should be IfTrue, second IfFalse.
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "If", "name": "Check Found", "condition": {
            "left": {"type": "Variable", "name": "click_ok.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "OK"}, "name": "Click OK"},
        {"id": "n3", "step_type": "Tool", "tool_name": "take_screenshot", "arguments": {}, "name": "Screenshot"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n1", "to": "n3"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(&mock, "If check", &sample_tools(), false, false, None)
        .await
        .unwrap();

    let wf = &result.workflow;
    let if_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::If(_)))
        .unwrap();

    let if_edges: Vec<_> = wf.edges.iter().filter(|e| e.from == if_node.id).collect();
    assert_eq!(if_edges.len(), 2);
    assert!(
        if_edges
            .iter()
            .any(|e| e.output == Some(EdgeOutput::IfTrue)),
        "Should infer IfTrue edge"
    );
    assert!(
        if_edges
            .iter()
            .any(|e| e.output == Some(EdgeOutput::IfFalse)),
        "Should infer IfFalse edge"
    );
}

#[test]
fn test_infer_noop_for_already_labeled_edges() {
    // Edges that already have labels should not be modified.
    use super::infer_control_flow_edges;
    use clickweave_core::{EndLoopParams, LoopParams, Node, NodeType, Position};

    let pos = |y| Position { x: 0.0, y };
    let loop_node = Node::new(
        NodeType::Loop(LoopParams {
            exit_condition: bool_condition("x"),
            max_iterations: 10,
        }),
        pos(0.0),
        "Loop",
    );
    let body_node = Node::new(NodeType::Click(ClickParams::default()), pos(100.0), "Body");
    let endloop_node = Node::new(
        NodeType::EndLoop(EndLoopParams {
            loop_id: loop_node.id,
        }),
        pos(200.0),
        "EndLoop",
    );
    let done_node = Node::new(NodeType::Click(ClickParams::default()), pos(300.0), "Done");

    let mut edges = vec![
        Edge {
            from: loop_node.id,
            to: body_node.id,
            output: Some(EdgeOutput::LoopBody),
        },
        Edge {
            from: loop_node.id,
            to: done_node.id,
            output: Some(EdgeOutput::LoopDone),
        },
        Edge {
            from: body_node.id,
            to: endloop_node.id,
            output: None,
        },
        Edge {
            from: endloop_node.id,
            to: loop_node.id,
            output: None,
        },
    ];
    let nodes = vec![loop_node, body_node, endloop_node, done_node];
    let mut warnings = Vec::new();

    infer_control_flow_edges(&nodes, &mut edges, &mut warnings);

    assert_eq!(edges.len(), 4);
    assert_eq!(edges[0].output, Some(EdgeOutput::LoopBody));
    assert_eq!(edges[1].output, Some(EdgeOutput::LoopDone));
    assert!(warnings.is_empty());
}

#[tokio::test]
async fn test_infer_loop_reroutes_body_back_edge_when_endloop_edge_already_exists() {
    // LLM produces both EndLoop→Loop AND body→Loop edges. Previously the
    // `continue` in Phase 2 skipped rerouting when the back-edge existed,
    // leaving a body→Loop cycle that validation rejected.
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n2", "step_type": "Loop", "name": "Multiply", "exit_condition": {
            "left": {"type": "Variable", "name": "check_result.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "max_iterations": 20},
        {"id": "n3", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2"},
        {"id": "n4", "step_type": "Tool", "tool_name": "find_text", "arguments": {"text": "1024"}, "name": "Check Result"},
        {"id": "n5", "step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"},
        {"from": "n3", "to": "n4"},
        {"from": "n4", "to": "n2"},
        {"from": "n5", "to": "n2"},
        {"from": "n2", "to": "n5"}
    ]}"#;
    // Both n4→n2 (body→Loop) and n5→n2 (EndLoop→Loop) are present.

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Calculator multiply with both back-edges",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    let wf = &result.workflow;
    let loop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Loop(_)))
        .unwrap();
    let endloop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::EndLoop(_)))
        .unwrap();

    // n4 (Check Result) should be rerouted to EndLoop, not Loop
    let check_node = wf.nodes.iter().find(|n| n.name == "Check Result").unwrap();
    let check_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == check_node.id)
        .collect();
    assert_eq!(check_edges.len(), 1);
    assert_eq!(
        check_edges[0].to, endloop_node.id,
        "body→Loop should be rerouted through EndLoop even when EndLoop→Loop already exists"
    );

    // EndLoop should have exactly one back-edge (no duplicates)
    let endloop_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == endloop_node.id && e.to == loop_node.id)
        .collect();
    assert_eq!(
        endloop_edges.len(),
        1,
        "Should not duplicate EndLoop→Loop back-edge"
    );

    // Workflow should pass validation (no cycle error)
    clickweave_core::validate_workflow(wf).expect("Workflow with both back-edges should validate");
}

#[tokio::test]
async fn test_infer_loop_reroutes_if_false_back_edge_to_loop() {
    // LLM produces If(IfFalse)→Loop edge inside a loop body. This should be
    // rerouted through EndLoop, otherwise cycle detection rejects it.
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus Calculator"},
        {"id": "n2", "step_type": "Loop", "name": "Repeat Multiply", "exit_condition": {
            "left": {"type": "Variable", "name": "check_result.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "max_iterations": 10},
        {"id": "n3", "step_type": "Tool", "tool_name": "find_text", "arguments": {"text": "1024"}, "name": "Check Result"},
        {"id": "n4", "step_type": "If", "condition": {
            "left": {"type": "Variable", "name": "check_result.found"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }, "name": "Is 1024?"},
        {"id": "n5", "step_type": "Tool", "tool_name": "press_key", "arguments": {"key": "escape"}, "name": "Done"},
        {"id": "n6", "step_type": "EndLoop", "loop_id": "n2", "name": "End Loop"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"},
        {"from": "n3", "to": "n4"},
        {"from": "n4", "to": "n5", "output": {"type": "IfTrue"}},
        {"from": "n4", "to": "n2", "output": {"type": "IfFalse"}},
        {"from": "n5", "to": "n6"},
        {"from": "n6", "to": "n2"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Calculator multiply with If→Loop back-edge",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    let wf = &result.workflow;
    let endloop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::EndLoop(_)))
        .unwrap();
    let if_node = wf.nodes.iter().find(|n| n.name == "Is 1024?").unwrap();

    // The IfFalse edge from n4 should now target EndLoop, not Loop
    let if_false_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == if_node.id && e.output == Some(EdgeOutput::IfFalse))
        .collect();
    assert_eq!(if_false_edges.len(), 1);
    assert_eq!(
        if_false_edges[0].to, endloop_node.id,
        "IfFalse→Loop should be rerouted through EndLoop"
    );

    // Workflow should pass validation (no cycle error)
    clickweave_core::validate_workflow(wf)
        .expect("Workflow with IfFalse rerouted through EndLoop should validate");
}

#[tokio::test]
async fn test_infer_loop_removes_stray_endloop_forward_edge_when_loop_done_exists() {
    // LLM emits LoopDone on the Loop node AND a forward edge from EndLoop to a
    // post-loop node. The forward edge is stray — EndLoop must only point back to
    // its paired Loop. Phase 2 should remove it.
    //
    // This matches the exact pattern seen with Qwen 30B:
    //   n4→n5 (LoopBody), n5→n6, n6→n7, n7→n4 (LoopBody),  // body→Loop
    //   n4→n8 (LoopDone),                                     // exit already present
    //   n10→n9                                                 // EndLoop→post-loop (WRONG)
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "launch_app", "arguments": {"app_name": "Calculator"}, "name": "Launch Calculator"},
        {"id": "n2", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus Calculator"},
        {"id": "n3", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2 Start"},
        {"id": "n4", "step_type": "Loop", "name": "Loop Until > 128", "exit_condition": {
            "left": {"type": "Variable", "name": "click_equals.result"},
            "operator": "GreaterThan",
            "right": {"type": "Literal", "value": {"type": "Number", "value": 128}}
        }, "max_iterations": 10},
        {"id": "n5", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "×"}, "name": "Click Multiply"},
        {"id": "n6", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "2"}, "name": "Click 2"},
        {"id": "n7", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "="}, "name": "Click Equals"},
        {"id": "n8", "step_type": "Tool", "tool_name": "take_screenshot", "arguments": {"app_name": "Calculator"}, "name": "Final Screenshot"},
        {"id": "n9", "step_type": "Tool", "tool_name": "find_text", "arguments": {"text": "128"}, "name": "Verify Result"},
        {"id": "n10", "step_type": "EndLoop", "loop_id": "n4", "name": "End Loop"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"},
        {"from": "n3", "to": "n4"},
        {"from": "n4", "to": "n5", "output": {"type": "LoopBody"}},
        {"from": "n5", "to": "n6"},
        {"from": "n6", "to": "n7"},
        {"from": "n7", "to": "n4", "output": {"type": "LoopBody"}},
        {"from": "n4", "to": "n8", "output": {"type": "LoopDone"}},
        {"from": "n8", "to": "n9"},
        {"from": "n10", "to": "n9"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Calculator multiply loop with stray EndLoop edge",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    let wf = &result.workflow;
    let loop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::Loop(_)))
        .unwrap();
    let endloop_node = wf
        .nodes
        .iter()
        .find(|n| matches!(n.node_type, NodeType::EndLoop(_)))
        .unwrap();

    // EndLoop should have exactly one outgoing edge pointing to Loop
    let endloop_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == endloop_node.id)
        .collect();
    assert_eq!(
        endloop_edges.len(),
        1,
        "EndLoop should have exactly 1 outgoing edge"
    );
    assert_eq!(
        endloop_edges[0].to, loop_node.id,
        "EndLoop's only edge must point back to its paired Loop"
    );

    // Last body step (Click Equals) should be rerouted to EndLoop
    let equals_node = wf.nodes.iter().find(|n| n.name == "Click Equals").unwrap();
    let equals_edges: Vec<_> = wf
        .edges
        .iter()
        .filter(|e| e.from == equals_node.id)
        .collect();
    assert_eq!(equals_edges.len(), 1);
    assert_eq!(
        equals_edges[0].to, endloop_node.id,
        "Last body step should route to EndLoop, not directly to Loop"
    );

    // LoopDone should still exist on the Loop node
    let loop_done = wf
        .edges
        .iter()
        .find(|e| e.from == loop_node.id && e.output == Some(EdgeOutput::LoopDone));
    assert!(loop_done.is_some(), "LoopDone edge should be preserved");

    // Workflow should pass validation
    clickweave_core::validate_workflow(wf)
        .expect("Workflow with stray EndLoop forward edge removed should validate");
}

#[test]
fn test_unknown_step_type_deserializes() {
    // Verify that an unknown step_type like "End" deserializes as PlanStep::Unknown
    // via #[serde(other)], allowing the rest of the graph to parse.
    let json = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "launch_app", "arguments": {"app_name": "Calculator"}, "name": "Launch"},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "5"}, "name": "Click 5"},
        {"id": "n3", "step_type": "End"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"}
    ]}"#;

    let parsed: Result<PlannerGraphOutput, _> = serde_json::from_str(json);
    assert!(
        parsed.is_ok(),
        "PlannerGraphOutput should parse with unknown step type: {:?}",
        parsed.err()
    );
    let graph = parsed.unwrap();
    assert_eq!(graph.nodes.len(), 3);
    // The third node ("End") parses as PlanStep::Unknown via #[serde(other)]
    let node2: PlanNode = serde_json::from_value(graph.nodes[2].clone()).unwrap();
    assert!(matches!(node2.step, PlanStep::Unknown));
}

#[tokio::test]
async fn test_unknown_step_type_skipped_not_fatal() {
    // LLM invents "End" step type — should be skipped, not crash the parse.
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "5"}, "name": "Click 5"},
        {"id": "n3", "step_type": "End"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Calculator click with End node",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    // n3 (End) should be skipped; workflow should have 2 valid nodes
    assert_eq!(result.workflow.nodes.len(), 2);
    assert!(result.warnings.iter().any(|w| w.contains("skipped")));
}

#[tokio::test]
async fn test_malformed_node_missing_fields_skipped() {
    // EndLoop without loop_id — should be skipped during node parse, not crash the graph.
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "5"}, "name": "Click 5"},
        {"id": "n3", "step_type": "EndLoop"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n3"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "EndLoop without loop_id",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    // n3 (EndLoop without loop_id) should be skipped; workflow has 2 valid nodes
    assert_eq!(result.workflow.nodes.len(), 2);
    assert!(
        result.warnings.iter().any(|w| w.contains("malformed")),
        "Expected malformed warning, got: {:?}",
        result.warnings
    );
}

#[tokio::test]
async fn test_malformed_edge_unknown_output_type_skipped() {
    // LLM invents "LoopCondition" edge output type — should be skipped, not crash.
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"target": "5"}, "name": "Click 5"}
    ], "edges": [
        {"from": "n1", "to": "n2"},
        {"from": "n2", "to": "n1", "output": {"type": "LoopCondition"}}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Calculator click with malformed edge",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    // Workflow should have 2 nodes; the malformed edge is skipped
    assert_eq!(result.workflow.nodes.len(), 2);
    assert_eq!(result.workflow.edges.len(), 1);
    assert!(
        result.warnings.iter().any(|w| w.contains("malformed")),
        "Expected malformed edge warning, got: {:?}",
        result.warnings
    );
}

#[tokio::test]
async fn test_malformed_flat_step_skipped() {
    // One valid step and one missing required fields — malformed step skipped.
    let response = r#"{"steps": [
        {"step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}},
        {"step_type": "Tool"}
    ]}"#;

    let mock = MockBackend::single(response);
    let result = plan_workflow_with_backend(
        &mock,
        "Mixed valid and malformed steps",
        &sample_tools(),
        false,
        false,
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.workflow.nodes.len(), 1);
    assert!(
        result.warnings.iter().any(|w| w.contains("malformed")),
        "Expected malformed step warning, got: {:?}",
        result.warnings
    );
}

#[tokio::test]
async fn test_malformed_patcher_update_skipped() {
    // Patcher returns a malformed update entry — should be skipped, not crash.
    let (node_id, workflow) = single_node_workflow(
        NodeType::FindText(FindTextParams {
            search_text: "test".into(),
            ..Default::default()
        }),
        "Find test",
    );

    let response = format!(
        r#"{{
        "update": [
            {{"node_id": "{}", "name": "Find Updated", "arguments": {{"text": "updated"}}}},
            {{"completely": "invalid", "garbage": true}}
        ]
    }}"#,
        node_id
    );
    let mock = MockBackend::single(&response);

    let result = patch_with_mock(&mock, &workflow, "Update and garbage")
        .await
        .unwrap();

    // Valid update should apply, malformed entry skipped
    assert_eq!(result.updated_nodes.len(), 1);
    assert_eq!(result.updated_nodes[0].name, "Find Updated");
    assert!(
        result.warnings.iter().any(|w| w.contains("malformed")),
        "Expected malformed update warning, got: {:?}",
        result.warnings
    );
}

// ── Validation retry tests ─────────────────────────────────────

#[tokio::test]
async fn test_assistant_retry_succeeds_on_second_attempt() {
    use super::assistant::assistant_chat_with_backend;
    use super::conversation::ConversationSession;

    let (_id, workflow) = single_node_workflow(NodeType::Click(ClickParams::default()), "Click");

    // First response: If node with only IfTrue edge (missing IfFalse → validation fails)
    let invalid_response = r#"{"add_nodes": [
        {"id": "n1", "step_type": "If", "name": "Check", "condition": {
            "left": {"type": "Variable", "name": "x"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"x": 1, "y": 2}, "name": "A"}
    ], "add_edges": [
        {"from": "n1", "to": "n2", "output": {"type": "IfTrue"}}
    ]}"#;

    // Second response: simple valid patch (just adds a click node)
    let valid_response =
        r#"{"add": [{"step_type": "Tool", "tool_name": "click", "arguments": {"x": 5, "y": 5}}]}"#;

    let mock = MockBackend::new(vec![invalid_response, valid_response]);
    let session = ConversationSession::new();

    let result = assistant_chat_with_backend(
        &mock,
        &workflow,
        "Add an if check",
        &session,
        None,
        &sample_tools(),
        false,
        false,
        3,
        None,
    )
    .await
    .unwrap();

    // Should have called LLM twice (initial + 1 retry)
    assert_eq!(mock.call_count(), 2);
    assert!(result.patch.is_some());
}

#[tokio::test]
async fn test_assistant_repair_callback_is_invoked() {
    use super::assistant::assistant_chat_with_backend;
    use super::conversation::ConversationSession;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (_id, workflow) = single_node_workflow(NodeType::Click(ClickParams::default()), "Click");

    let invalid_response = r#"{"add_nodes": [
        {"id": "n1", "step_type": "If", "name": "Check", "condition": {
            "left": {"type": "Variable", "name": "x"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"x": 1, "y": 2}, "name": "A"}
    ], "add_edges": [
        {"from": "n1", "to": "n2", "output": {"type": "IfTrue"}}
    ]}"#;

    let valid_response =
        r#"{"add": [{"step_type": "Tool", "tool_name": "click", "arguments": {"x": 5, "y": 5}}]}"#;

    let mock = MockBackend::new(vec![invalid_response, valid_response]);
    let session = ConversationSession::new();

    let repair_count = Arc::new(AtomicUsize::new(0));
    let repair_count_clone = repair_count.clone();
    let on_repair = move |attempt: usize, _max: usize| {
        repair_count_clone.fetch_add(1, Ordering::SeqCst);
        assert_eq!(attempt, 1);
    };

    let result = assistant_chat_with_backend(
        &mock,
        &workflow,
        "Add an if check",
        &session,
        None,
        &sample_tools(),
        false,
        false,
        3,
        Some(&on_repair),
    )
    .await
    .unwrap();

    assert_eq!(repair_count.load(Ordering::SeqCst), 1);
    assert_eq!(mock.call_count(), 2);
    assert!(result.patch.is_some());
}

#[tokio::test]
async fn test_assistant_retry_exhausted_returns_last_patch() {
    use super::assistant::assistant_chat_with_backend;
    use super::conversation::ConversationSession;

    let (_id, workflow) = single_node_workflow(NodeType::Click(ClickParams::default()), "Click");

    // Always returns invalid patch (If with missing IfFalse)
    let invalid_response = r#"{"add_nodes": [
        {"id": "n1", "step_type": "If", "name": "Check", "condition": {
            "left": {"type": "Variable", "name": "x"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"x": 1, "y": 2}, "name": "A"}
    ], "add_edges": [
        {"from": "n1", "to": "n2", "output": {"type": "IfTrue"}}
    ]}"#;

    // 3 identical responses: initial + 2 retries (max_repair_attempts=3 → 2 retries)
    let mock = MockBackend::new(vec![invalid_response, invalid_response, invalid_response]);
    let session = ConversationSession::new();

    let result = assistant_chat_with_backend(
        &mock,
        &workflow,
        "Add an if check",
        &session,
        None,
        &sample_tools(),
        false,
        false,
        3, // value 3 → validate + 2 retries = 3 LLM calls max
        None,
    )
    .await
    .unwrap();

    // Should have called LLM 3 times (initial + 2 retries)
    assert_eq!(mock.call_count(), 3);
    // Should still return the patch (let frontend handle rejection)
    assert!(result.patch.is_some());
}

#[tokio::test]
async fn test_assistant_no_validation_when_max_is_zero() {
    use super::assistant::assistant_chat_with_backend;
    use super::conversation::ConversationSession;

    let (_id, workflow) = single_node_workflow(NodeType::Click(ClickParams::default()), "Click");

    // Invalid patch, but max_repair_attempts = 0 skips validation entirely
    let invalid_response = r#"{"add_nodes": [
        {"id": "n1", "step_type": "If", "name": "Check", "condition": {
            "left": {"type": "Variable", "name": "x"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"x": 1, "y": 2}, "name": "A"}
    ], "add_edges": [
        {"from": "n1", "to": "n2", "output": {"type": "IfTrue"}}
    ]}"#;

    let mock = MockBackend::single(invalid_response);
    let session = ConversationSession::new();

    let result = assistant_chat_with_backend(
        &mock,
        &workflow,
        "Add an if check",
        &session,
        None,
        &sample_tools(),
        false,
        false,
        0, // 0 = skip validation entirely
        None,
    )
    .await
    .unwrap();

    // Only 1 LLM call, validation skipped
    assert_eq!(mock.call_count(), 1);
    assert!(result.patch.is_some());
}

#[tokio::test]
async fn test_assistant_validate_only_no_retry_when_max_is_one() {
    use super::assistant::assistant_chat_with_backend;
    use super::conversation::ConversationSession;

    let (_id, workflow) = single_node_workflow(NodeType::Click(ClickParams::default()), "Click");

    // Invalid patch — max_repair_attempts = 1 means validate but no retry
    let invalid_response = r#"{"add_nodes": [
        {"id": "n1", "step_type": "If", "name": "Check", "condition": {
            "left": {"type": "Variable", "name": "x"},
            "operator": "Equals",
            "right": {"type": "Literal", "value": {"type": "Bool", "value": true}}
        }},
        {"id": "n2", "step_type": "Tool", "tool_name": "click", "arguments": {"x": 1, "y": 2}, "name": "A"}
    ], "add_edges": [
        {"from": "n1", "to": "n2", "output": {"type": "IfTrue"}}
    ]}"#;

    let mock = MockBackend::single(invalid_response);
    let session = ConversationSession::new();

    let result = assistant_chat_with_backend(
        &mock,
        &workflow,
        "Add an if check",
        &session,
        None,
        &sample_tools(),
        false,
        false,
        1, // 1 = validate only, no retry
        None,
    )
    .await
    .unwrap();

    // Only 1 LLM call — validated but no retry
    assert_eq!(mock.call_count(), 1);
    assert!(result.patch.is_some());
}

#[tokio::test]
async fn test_assistant_valid_patch_no_retry_needed() {
    use super::assistant::assistant_chat_with_backend;
    use super::conversation::ConversationSession;

    let (_id, workflow) = single_node_workflow(NodeType::Click(ClickParams::default()), "Click");

    let valid_response =
        r#"{"add": [{"step_type": "Tool", "tool_name": "click", "arguments": {"x": 5, "y": 5}}]}"#;

    let mock = MockBackend::single(valid_response);
    let session = ConversationSession::new();

    let result = assistant_chat_with_backend(
        &mock,
        &workflow,
        "Add another click",
        &session,
        None,
        &sample_tools(),
        false,
        false,
        3, // retries enabled, but not needed
        None,
    )
    .await
    .unwrap();

    // Only 1 LLM call, validation passed first time
    assert_eq!(mock.call_count(), 1);
    assert!(result.patch.is_some());
    assert_eq!(result.patch.unwrap().added_nodes.len(), 1);
}
