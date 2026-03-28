use super::helpers::*;
use crate::planner::assistant::assistant_chat_with_backend;
use crate::planner::conversation::ConversationSession;
use crate::planner::conversation_loop::NoExecutor;
use crate::planner::prompt::assistant_system_prompt;
use crate::planner::summarize::summarize_overflow;
use crate::planner::*;
use clickweave_core::{
    ClickParams, FocusMethod, FocusWindowParams, NodeType, ScreenshotMode, TakeScreenshotParams,
};

// ── Conversation tests ─────────────────────────────────────────

#[test]
fn test_conversation_recent_window_small() {
    let mut session = ConversationSession::new();
    session.push_user("hello".into(), None);
    session.push_assistant("hi".into(), None);
    assert_eq!(session.recent_window(None).len(), 2);
    assert!(!session.needs_summarization(None));
}

#[test]
fn test_conversation_recent_window_overflow() {
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
        None,
        None::<&NoExecutor>,
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
        None,
        None::<&NoExecutor>,
    )
    .await
    .unwrap();

    assert!(result.patch.is_some());
    assert_eq!(result.patch.unwrap().added_nodes.len(), 1);
}

#[tokio::test]
async fn test_assistant_chat_conversational_response() {
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
        None,
        None::<&NoExecutor>,
    )
    .await
    .unwrap();

    assert!(result.patch.is_none());
    assert!(result.message.contains("screenshot"));
}

// ── Assistant prompt tests ──────────────────────────────────────

#[test]
fn test_assistant_prompt_empty_workflow_includes_control_flow() {
    let wf = Workflow::new("Test");
    let prompt = assistant_system_prompt(&wf, &[], false, false, None, None, false);
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
    let (_, workflow) = single_node_workflow(NodeType::Click(ClickParams::default()), "Click");
    let prompt = assistant_system_prompt(&workflow, &[], false, false, None, None, false);
    assert!(
        prompt.contains("add_nodes"),
        "Patcher assistant prompt should mention add_nodes for control flow"
    );
}

// ── R2 regression tests: assistant control-flow parsing ─────────

#[tokio::test]
async fn test_assistant_patches_with_add_nodes_and_add_edges() {
    let (_id, workflow) = single_node_workflow(
        NodeType::FocusWindow(FocusWindowParams {
            method: FocusMethod::AppName,
            value: Some("Calculator".to_string()),
            bring_to_front: true,
            app_kind: clickweave_core::AppKind::Native,
            chrome_profile_id: None,
            ..Default::default()
        }),
        "Focus Calculator",
    );

    // Patcher output using add_nodes + add_edges (control-flow patch)
    let response = r#"{"add_nodes": [
        {"id": "n1", "step_type": "Loop", "name": "My Loop", "exit_condition": {
            "left": {"node": "done", "field": "result"},
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
        None,
        None::<&NoExecutor>,
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
    // Graph-format planner output (control-flow plan for empty workflow)
    let response = r#"{"nodes": [
        {"id": "n1", "step_type": "Tool", "tool_name": "focus_window", "arguments": {"app_name": "Calculator"}, "name": "Focus"},
        {"id": "n2", "step_type": "Loop", "name": "Loop", "exit_condition": {
            "left": {"node": "done", "field": "result"},
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
        None,
        None::<&NoExecutor>,
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

// ── Validation retry tests ─────────────────────────────────────

#[tokio::test]
async fn test_assistant_retry_succeeds_on_second_attempt() {
    let (_id, workflow) = single_node_workflow(NodeType::Click(ClickParams::default()), "Click");

    // First response: If node with only IfTrue edge (missing IfFalse → validation fails)
    let invalid_response = r#"{"add_nodes": [
        {"id": "n1", "step_type": "If", "name": "Check", "condition": {
            "left": {"node": "x", "field": "result"},
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
        None,
        None::<&NoExecutor>,
    )
    .await
    .unwrap();

    // Should have called LLM twice (initial + 1 retry)
    assert_eq!(mock.call_count(), 2);
    assert!(result.patch.is_some());
}

#[tokio::test]
async fn test_assistant_repair_callback_is_invoked() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (_id, workflow) = single_node_workflow(NodeType::Click(ClickParams::default()), "Click");

    let invalid_response = r#"{"add_nodes": [
        {"id": "n1", "step_type": "If", "name": "Check", "condition": {
            "left": {"node": "x", "field": "result"},
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
        None,
        None::<&NoExecutor>,
    )
    .await
    .unwrap();

    assert_eq!(repair_count.load(Ordering::SeqCst), 1);
    assert_eq!(mock.call_count(), 2);
    assert!(result.patch.is_some());
}

#[tokio::test]
async fn test_assistant_retry_exhausted_returns_last_patch() {
    let (_id, workflow) = single_node_workflow(NodeType::Click(ClickParams::default()), "Click");

    // Always returns invalid patch (If with missing IfFalse)
    let invalid_response = r#"{"add_nodes": [
        {"id": "n1", "step_type": "If", "name": "Check", "condition": {
            "left": {"node": "x", "field": "result"},
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
        None,
        None::<&NoExecutor>,
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
    let (_id, workflow) = single_node_workflow(NodeType::Click(ClickParams::default()), "Click");

    // Invalid patch, but max_repair_attempts = 0 skips validation entirely
    let invalid_response = r#"{"add_nodes": [
        {"id": "n1", "step_type": "If", "name": "Check", "condition": {
            "left": {"node": "x", "field": "result"},
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
        None,
        None::<&NoExecutor>,
    )
    .await
    .unwrap();

    // Only 1 LLM call, validation skipped
    assert_eq!(mock.call_count(), 1);
    assert!(result.patch.is_some());
}

#[tokio::test]
async fn test_assistant_validate_only_no_retry_when_max_is_one() {
    let (_id, workflow) = single_node_workflow(NodeType::Click(ClickParams::default()), "Click");

    // Invalid patch — max_repair_attempts = 1 means validate but no retry
    let invalid_response = r#"{"add_nodes": [
        {"id": "n1", "step_type": "If", "name": "Check", "condition": {
            "left": {"node": "x", "field": "result"},
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
        None,
        None::<&NoExecutor>,
    )
    .await
    .unwrap();

    // Only 1 LLM call — validated but no retry
    assert_eq!(mock.call_count(), 1);
    assert!(result.patch.is_some());
}

#[tokio::test]
async fn test_assistant_valid_patch_no_retry_needed() {
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
        None,
        None::<&NoExecutor>,
    )
    .await
    .unwrap();

    // Only 1 LLM call, validation passed first time
    assert_eq!(mock.call_count(), 1);
    assert!(result.patch.is_some());
    assert_eq!(result.patch.unwrap().added_nodes.len(), 1);
}
