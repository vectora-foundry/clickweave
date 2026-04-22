use crate::agent::loop_runner::AgentRunner;
use crate::agent::types::*;
use crate::executor::Mcp;
use anyhow::Result;
use clickweave_llm::{
    CallType, ChatBackend, ChatOptions, ChatResponse, Choice, FunctionCall, Message, ModelInfo,
    Role, ToolCall,
};
use clickweave_mcp::{ToolCallResult, ToolContent};
use serde_json::Value;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Mock LLM backend
// ---------------------------------------------------------------------------

struct MockAgent {
    responses: Mutex<Vec<ChatResponse>>,
}

impl MockAgent {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }

    /// Convenience: build a ChatResponse with a single tool call.
    fn tool_call_response(tool_name: &str, arguments: &str, tool_call_id: &str) -> ChatResponse {
        let parsed: Value = serde_json::from_str(arguments)
            .unwrap_or_else(|_| Value::String(arguments.to_string()));
        ChatResponse {
            id: "mock-resp".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message::assistant_tool_calls(vec![ToolCall {
                    id: tool_call_id.to_string(),
                    call_type: CallType::Function,
                    function: FunctionCall {
                        name: tool_name.to_string(),
                        arguments: parsed,
                    },
                }]),
                finish_reason: Some("tool_calls".to_string()),
            }],
            usage: None,
        }
    }

    /// Convenience: build a ChatResponse with the agent_done pseudo-tool.
    fn done_response(summary: &str) -> ChatResponse {
        Self::tool_call_response(
            "agent_done",
            &serde_json::json!({ "summary": summary }).to_string(),
            "call_done",
        )
    }
}

impl ChatBackend for MockAgent {
    fn model_name(&self) -> &str {
        "mock-agent"
    }

    async fn chat_with_options(
        &self,
        _messages: &[Message],
        _tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> Result<ChatResponse> {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            // Fallback: return agent_done so tests don't hang
            Ok(Self::done_response("No more responses"))
        } else {
            Ok(responses.remove(0))
        }
    }

    async fn fetch_model_info(&self) -> Result<Option<ModelInfo>> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Mock MCP client
// ---------------------------------------------------------------------------

struct MockMcp {
    tool_results: Mutex<Vec<ToolCallResult>>,
    tools: Vec<Value>,
}

impl MockMcp {
    fn new(tool_results: Vec<ToolCallResult>, tools: Vec<Value>) -> Self {
        Self {
            tool_results: Mutex::new(tool_results),
            tools,
        }
    }

    fn with_click_tool() -> Self {
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "click",
                "description": "Click at coordinates",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "x": {"type": "number"},
                        "y": {"type": "number"}
                    },
                    "required": ["x", "y"]
                }
            }
        })];

        // First call: cdp_find_elements returns empty (no elements)
        // Second call: click returns success
        let results = vec![
            // cdp_find_elements result
            ToolCallResult {
                content: vec![ToolContent::Text {
                    text: serde_json::json!({
                        "page_url": "https://example.com",
                        "source": "cdp",
                        "matches": [{
                            "uid": "1_0",
                            "role": "button",
                            "label": "Submit",
                            "tag": "button"
                        }]
                    })
                    .to_string(),
                }],
                is_error: None,
            },
            // click result
            ToolCallResult {
                content: vec![ToolContent::Text {
                    text: "Clicked at (100, 200)".to_string(),
                }],
                is_error: None,
            },
            // cdp_find_elements for second observation
            ToolCallResult {
                content: vec![ToolContent::Text {
                    text: serde_json::json!({
                        "page_url": "https://example.com/success",
                        "source": "cdp",
                        "matches": [{
                            "uid": "2_0",
                            "role": "heading",
                            "label": "Success",
                            "tag": "h1"
                        }]
                    })
                    .to_string(),
                }],
                is_error: None,
            },
        ];

        Self::new(results, tools)
    }
}

impl Mcp for MockMcp {
    async fn call_tool(
        &self,
        _name: &str,
        _arguments: Option<Value>,
    ) -> anyhow::Result<ToolCallResult> {
        let mut results = self.tool_results.lock().unwrap();
        if results.is_empty() {
            Ok(ToolCallResult {
                content: vec![ToolContent::Text {
                    text: "ok".to_string(),
                }],
                is_error: None,
            })
        } else {
            Ok(results.remove(0))
        }
    }

    fn has_tool(&self, name: &str) -> bool {
        // Support cdp_find_elements and any tools in the list
        if name == "cdp_find_elements" {
            return true;
        }
        self.tools
            .iter()
            .any(|t| t["function"]["name"].as_str() == Some(name))
    }

    fn tools_as_openai(&self) -> Vec<Value> {
        self.tools.clone()
    }

    async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

mod conversational_extend;

#[tokio::test]
async fn agent_executes_single_click_and_completes() {
    let agent_llm = MockAgent::new(vec![
        // Step 0: LLM chooses to click
        MockAgent::tool_call_response("click", r#"{"x": 100, "y": 200}"#, "call_0"),
        // Step 1: LLM declares done
        MockAgent::done_response("Clicked the submit button"),
    ]);

    let mcp = MockMcp::with_click_tool();
    let config = AgentConfig {
        max_steps: 10,
        build_workflow: true,
        use_cache: false,
        ..Default::default()
    };

    let mut runner = AgentRunner::new(&agent_llm, config);
    let workflow = clickweave_core::Workflow::new("Test Workflow");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click the submit button".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(state.completed);
    assert_eq!(state.steps.len(), 2);
    assert!(state.summary.is_some());
    assert!(state.summary.as_ref().unwrap().contains("submit button"));

    // Verify workflow was built with at least one node (from the click)
    assert!(
        !state.workflow.nodes.is_empty(),
        "Workflow should have at least one node from the click action"
    );
}

#[tokio::test]
async fn agent_stops_at_max_steps() {
    // LLM always returns a click — never calls agent_done
    let responses: Vec<ChatResponse> = (0..5)
        .map(|i| {
            MockAgent::tool_call_response(
                "click",
                r#"{"x": 100, "y": 200}"#,
                &format!("call_{}", i),
            )
        })
        .collect();

    let agent_llm = MockAgent::new(responses);
    let mcp = MockMcp::with_click_tool();
    let config = AgentConfig {
        max_steps: 3,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let mut runner = AgentRunner::new(&agent_llm, config);
    let workflow = clickweave_core::Workflow::new("Test Workflow");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click forever".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(!state.completed);
    assert_eq!(state.steps.len(), 3);
}

#[tokio::test]
async fn agent_handles_text_only_response() {
    let agent_llm = MockAgent::new(vec![
        // LLM returns text instead of a tool call
        ChatResponse {
            id: "mock-text".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message::assistant("I'm thinking about what to do..."),
                finish_reason: Some("stop".to_string()),
            }],
            usage: None,
        },
        // Then completes
        MockAgent::done_response("Completed after thinking"),
    ]);

    let mcp = MockMcp::with_click_tool();
    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let mut runner = AgentRunner::new(&agent_llm, config);
    let workflow = clickweave_core::Workflow::new("Test Workflow");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Do something".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(state.completed);
    // First step was text-only (treated as error), second was done
    assert_eq!(state.steps.len(), 2);
    assert!(matches!(
        state.steps[0].command,
        AgentCommand::TextOnly { .. }
    ));
}

#[tokio::test]
async fn agent_replan_does_not_complete() {
    let agent_llm = MockAgent::new(vec![
        MockAgent::tool_call_response(
            "agent_replan",
            r#"{"reason": "Cannot find the button"}"#,
            "call_replan",
        ),
        MockAgent::done_response("Found an alternative path"),
    ]);

    let mcp = MockMcp::with_click_tool();
    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let mut runner = AgentRunner::new(&agent_llm, config);
    let workflow = clickweave_core::Workflow::new("Test Workflow");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click a missing button".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(state.completed);
    assert_eq!(state.steps.len(), 2);
    assert!(matches!(state.steps[0].outcome, StepOutcome::Replan(_)));
}

#[tokio::test]
async fn agent_state_reports_completed_reason_on_done() {
    let agent_llm = MockAgent::new(vec![MockAgent::done_response("All done")]);

    let mcp = MockMcp::with_click_tool();
    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let mut runner = AgentRunner::new(&agent_llm, config);
    let workflow = clickweave_core::Workflow::new("Test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Do it".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(state.completed);
    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::Completed { .. })
        ),
        "Expected Completed, got {:?}",
        state.terminal_reason,
    );
}

#[tokio::test]
async fn agent_state_reports_max_steps_reason() {
    let responses: Vec<ChatResponse> = (0..5)
        .map(|i| {
            MockAgent::tool_call_response("click", r#"{"x": 10, "y": 20}"#, &format!("call_{}", i))
        })
        .collect();

    let agent_llm = MockAgent::new(responses);
    let mcp = MockMcp::with_click_tool();
    let config = AgentConfig {
        max_steps: 3,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let mut runner = AgentRunner::new(&agent_llm, config);
    let workflow = clickweave_core::Workflow::new("Test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click forever".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(!state.completed);
    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::MaxStepsReached { .. })
        ),
        "Expected MaxStepsReached, got {:?}",
        state.terminal_reason,
    );
}

#[tokio::test]
async fn agent_state_reports_max_errors_reason() {
    // LLM always chooses click, but MCP always returns errors.
    // Each call uses different args so loop detection doesn't fire —
    // this test exercises the max-consecutive-errors path specifically.
    let responses: Vec<ChatResponse> = (0..10)
        .map(|i| {
            MockAgent::tool_call_response(
                "click",
                &format!(r#"{{"x": {}, "y": 20}}"#, i * 10),
                &format!("call_{}", i),
            )
        })
        .collect();

    let agent_llm = MockAgent::new(responses);

    // MCP that returns errors for everything except cdp_find_elements
    let error_results: Vec<ToolCallResult> = (0..30)
        .map(|i| {
            if i % 2 == 0 {
                // cdp_find_elements (observation step)
                ToolCallResult {
                    content: vec![ToolContent::Text {
                        text: serde_json::json!({
                            "page_url": "https://example.com",
                            "source": "cdp",
                            "matches": []
                        })
                        .to_string(),
                    }],
                    is_error: None,
                }
            } else {
                // click returns error
                ToolCallResult {
                    content: vec![ToolContent::Text {
                        text: "Element not found".to_string(),
                    }],
                    is_error: Some(true),
                }
            }
        })
        .collect();

    let tools = vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "click",
            "description": "Click",
            "parameters": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}, "required": ["x", "y"]}
        }
    })];
    let mcp = MockMcp::new(error_results, tools);

    let config = AgentConfig {
        max_steps: 30,
        max_consecutive_errors: 3,
        build_workflow: false,
        use_cache: false,
        consecutive_destructive_cap: 0,
        allow_focus_window: true,
    };

    let mut runner = AgentRunner::new(&agent_llm, config);
    let workflow = clickweave_core::Workflow::new("Test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click it".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(!state.completed);
    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::MaxErrorsReached { .. })
        ),
        "Expected MaxErrorsReached, got {:?}",
        state.terminal_reason,
    );
}

/// When the LLM issues the identical (tool, args) call twice in a row and
/// gets the identical error both times, the loop halts with `LoopDetected`
/// without burning through the `max_consecutive_errors` budget.
#[tokio::test]
async fn agent_state_reports_loop_detected_on_identical_repeat_failure() {
    let responses: Vec<ChatResponse> = (0..10)
        .map(|i| {
            MockAgent::tool_call_response(
                "click",
                r#"{}"#, // identical empty args every turn
                &format!("call_{}", i),
            )
        })
        .collect();

    let agent_llm = MockAgent::new(responses);

    let error_results: Vec<ToolCallResult> = (0..30)
        .map(|i| {
            if i % 2 == 0 {
                ToolCallResult {
                    content: vec![ToolContent::Text {
                        text: serde_json::json!({
                            "page_url": "https://example.com",
                            "source": "cdp",
                            "matches": []
                        })
                        .to_string(),
                    }],
                    is_error: None,
                }
            } else {
                ToolCallResult {
                    content: vec![ToolContent::Text {
                        text: "click requires exactly one complete coordinate variant".to_string(),
                    }],
                    is_error: Some(true),
                }
            }
        })
        .collect();

    let tools = vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "click",
            "description": "Click",
            "parameters": {"type": "object", "properties": {"x": {"type": "number"}, "y": {"type": "number"}}, "required": ["x", "y"]}
        }
    })];
    let mcp = MockMcp::new(error_results, tools);

    let config = AgentConfig {
        max_steps: 30,
        max_consecutive_errors: 10, // high enough that MaxErrorsReached can't be what fires
        build_workflow: false,
        use_cache: false,
        consecutive_destructive_cap: 0,
        allow_focus_window: true,
    };

    let mut runner = AgentRunner::new(&agent_llm, config);
    let workflow = clickweave_core::Workflow::new("Test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click it".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(!state.completed);
    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::LoopDetected { .. })
        ),
        "Expected LoopDetected, got {:?}",
        state.terminal_reason,
    );
    // Loop detection fires on the *second* identical failure, so we
    // should have exactly 2 failing steps — proof that we aborted
    // early rather than burning through max_consecutive_errors.
    assert_eq!(
        state.consecutive_errors, 2,
        "Expected abort after 2 identical failures, got {}",
        state.consecutive_errors,
    );
}

#[tokio::test]
async fn approval_success_allows_execution() {
    let agent_llm = MockAgent::new(vec![
        MockAgent::tool_call_response("click", r#"{"x": 10, "y": 20}"#, "call_0"),
        MockAgent::done_response("Clicked successfully"),
    ]);

    let mcp = MockMcp::with_click_tool();
    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(64);
    let (approval_tx, mut approval_rx) =
        tokio::sync::mpsc::channel::<(ApprovalRequest, tokio::sync::oneshot::Sender<bool>)>(1);

    // Spawn a task that auto-approves everything
    tokio::spawn(async move {
        while let Some((_req, resp_tx)) = approval_rx.recv().await {
            let _ = resp_tx.send(true);
        }
    });

    let mut runner = AgentRunner::new(&agent_llm, config);
    runner = runner.with_events(event_tx).with_approval(approval_tx);
    let workflow = clickweave_core::Workflow::new("Test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click it".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(state.completed);
    // First step should succeed (click was approved and executed)
    assert!(
        matches!(state.steps[0].outcome, StepOutcome::Success(_)),
        "Expected Success after approval, got {:?}",
        state.steps[0].outcome,
    );
}

#[tokio::test]
async fn approval_rejection_triggers_replan() {
    let agent_llm = MockAgent::new(vec![
        MockAgent::tool_call_response("click", r#"{"x": 10, "y": 20}"#, "call_0"),
        MockAgent::done_response("Found another way"),
    ]);

    let mcp = MockMcp::with_click_tool();
    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(64);
    let (approval_tx, mut approval_rx) =
        tokio::sync::mpsc::channel::<(ApprovalRequest, tokio::sync::oneshot::Sender<bool>)>(1);

    // Spawn a task that rejects everything
    tokio::spawn(async move {
        while let Some((_req, resp_tx)) = approval_rx.recv().await {
            let _ = resp_tx.send(false);
        }
    });

    let mut runner = AgentRunner::new(&agent_llm, config);
    runner = runner.with_events(event_tx).with_approval(approval_tx);
    let workflow = clickweave_core::Workflow::new("Test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click it".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(state.completed);
    assert!(
        matches!(state.steps[0].outcome, StepOutcome::Replan(_)),
        "Expected Replan after rejection, got {:?}",
        state.steps[0].outcome,
    );
}

#[tokio::test]
async fn approval_channel_failure_terminates_agent() {
    let agent_llm = MockAgent::new(vec![
        // LLM chooses click (needs approval) — but approval channel is dead
        MockAgent::tool_call_response("click", r#"{"x": 10, "y": 20}"#, "call_0"),
        // More responses in case the loop continues (it shouldn't)
        MockAgent::done_response("Should not reach this"),
    ]);

    let mcp = MockMcp::with_click_tool();
    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(64);
    // Create an approval channel and immediately drop the receiver.
    // This means the sender's `send()` will fail.
    let (approval_tx, _dropped_rx) = tokio::sync::mpsc::channel(1);
    drop(_dropped_rx);

    let mut runner = AgentRunner::new(&agent_llm, config);
    runner = runner.with_events(event_tx).with_approval(approval_tx);
    let workflow = clickweave_core::Workflow::new("Test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click it".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    // The agent should have terminated immediately — approval system is
    // permanently unavailable, so no state-changing tools can execute.
    assert!(!state.completed);
    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::ApprovalUnavailable)
        ),
        "Expected ApprovalUnavailable, got {:?}",
        state.terminal_reason,
    );
}

// ---------------------------------------------------------------------------
// Mock LLM that captures messages for transcript verification
// ---------------------------------------------------------------------------

struct CapturingMockAgent {
    responses: Mutex<Vec<ChatResponse>>,
    captured_messages: Arc<Mutex<Vec<Vec<Message>>>>,
}

impl CapturingMockAgent {
    fn new(responses: Vec<ChatResponse>, captured: Arc<Mutex<Vec<Vec<Message>>>>) -> Self {
        Self {
            responses: Mutex::new(responses),
            captured_messages: captured,
        }
    }
}

impl ChatBackend for CapturingMockAgent {
    fn model_name(&self) -> &str {
        "capturing-mock-agent"
    }

    async fn chat_with_options(
        &self,
        messages: &[Message],
        _tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> Result<ChatResponse> {
        self.captured_messages
            .lock()
            .unwrap()
            .push(messages.to_vec());
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            Ok(MockAgent::done_response("No more responses"))
        } else {
            Ok(responses.remove(0))
        }
    }

    async fn fetch_model_info(&self) -> Result<Option<ModelInfo>> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// reasoning_content stripping (Gemma 4 multi-turn hygiene)
// ---------------------------------------------------------------------------

/// After `append_assistant_message` runs, the transcript message pushed to
/// `self.messages` must NOT carry `reasoning_content`, regardless of whether
/// the model response included a thought block. Gemma 4's model card prohibits
/// feeding prior-turn thought blocks back into subsequent requests.
#[tokio::test]
async fn append_assistant_message_strips_reasoning_content_from_transcript() {
    // Set up a two-step run: one tool call (with reasoning_content on the
    // response) followed by agent_done.  We capture every message slice
    // passed to the LLM so we can inspect what was in the transcript on the
    // second call.
    let captured = Arc::new(Mutex::new(Vec::<Vec<Message>>::new()));

    let tool_call_response = {
        let mut msg = Message::assistant_tool_calls(vec![ToolCall {
            id: "call_0".to_string(),
            call_type: CallType::Function,
            function: FunctionCall {
                name: "click".to_string(),
                arguments: serde_json::json!({"x": 100, "y": 200}),
            },
        }]);
        // Simulate the model returning a thought block alongside the tool call.
        msg.reasoning_content = Some("I need to click the button.".to_string());
        ChatResponse {
            id: "mock-r0".to_string(),
            choices: vec![Choice {
                index: 0,
                message: msg,
                finish_reason: Some("tool_calls".to_string()),
            }],
            usage: None,
        }
    };

    let agent_llm = CapturingMockAgent::new(
        vec![tool_call_response, MockAgent::done_response("done")],
        Arc::clone(&captured),
    );

    let mcp = MockMcp::with_click_tool();
    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let mut runner = AgentRunner::new(&agent_llm, config);
    let workflow = clickweave_core::Workflow::new("Test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click the button".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(state.completed, "run should have completed");

    let calls = captured.lock().unwrap();
    // The second LLM call (for agent_done) receives the transcript that
    // append_assistant_message built after the first tool call.
    assert!(
        calls.len() >= 2,
        "expected at least 2 LLM calls, got {}",
        calls.len()
    );
    let second_call_messages = &calls[1];

    // The assistant turn appended by append_assistant_message must have no
    // reasoning_content — even though the model response carried one.
    let assistant_msgs_with_reasoning: Vec<_> = second_call_messages
        .iter()
        .filter(|m| m.role == Role::Assistant && m.reasoning_content.is_some())
        .collect();

    assert!(
        assistant_msgs_with_reasoning.is_empty(),
        "transcript assistant messages must not carry reasoning_content; \
         found {} message(s) with it set",
        assistant_msgs_with_reasoning.len()
    );
}

// ---------------------------------------------------------------------------
// Cache replay creates workflow nodes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cache_replay_creates_workflow_nodes() {
    // Populate a cache with a "click" decision for a known page state.
    let mut cache = AgentCache::default();
    let elements = vec![clickweave_core::cdp::CdpFindElementMatch {
        uid: "1_0".to_string(),
        role: "button".to_string(),
        label: "Submit".to_string(),
        tag: "button".to_string(),
        disabled: false,
        parent_role: None,
        parent_name: None,
    }];
    cache.store(
        "Click the submit button",
        &elements,
        "click".to_string(),
        serde_json::json!({"x": 100, "y": 200}),
    );

    // LLM should only be called for the done step (cache handles the click).
    let agent_llm = MockAgent::new(vec![MockAgent::done_response(
        "Clicked the submit button via cache",
    )]);

    // MCP returns the same elements page for cdp_find_elements, then "ok" for the click.
    let mcp_results = vec![
        // cdp_find_elements for step 0 observation
        ToolCallResult {
            content: vec![ToolContent::Text {
                text: serde_json::json!({
                    "page_url": "https://example.com",
                    "source": "cdp",
                    "matches": [{
                        "uid": "1_0",
                        "role": "button",
                        "label": "Submit",
                        "tag": "button"
                    }]
                })
                .to_string(),
            }],
            is_error: None,
        },
        // click result for cached replay
        ToolCallResult {
            content: vec![ToolContent::Text {
                text: "Clicked at (100, 200)".to_string(),
            }],
            is_error: None,
        },
        // cdp_find_elements for step 1 observation (after cache replay)
        ToolCallResult {
            content: vec![ToolContent::Text {
                text: serde_json::json!({
                    "page_url": "https://example.com/success",
                    "source": "cdp",
                    "matches": [{
                        "uid": "2_0",
                        "role": "heading",
                        "label": "Success",
                        "tag": "h1"
                    }]
                })
                .to_string(),
            }],
            is_error: None,
        },
    ];
    let tools = vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "click",
            "description": "Click at coordinates",
            "parameters": {
                "type": "object",
                "properties": {
                    "x": {"type": "number"},
                    "y": {"type": "number"}
                },
                "required": ["x", "y"]
            }
        }
    })];
    let mcp = MockMcp::new(mcp_results, tools);

    let config = AgentConfig {
        max_steps: 10,
        build_workflow: true,
        use_cache: true,
        ..Default::default()
    };

    let mut runner = AgentRunner::with_cache(&agent_llm, config, cache);
    let workflow = clickweave_core::Workflow::new("Cache Replay Test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click the submit button".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(state.completed);
    // The cache-replayed click should produce a workflow node.
    assert!(
        !state.workflow.nodes.is_empty(),
        "Cache replay should create at least one workflow node"
    );
}

// ---------------------------------------------------------------------------
// Cache replay reconstructs transcript
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cache_replay_reconstructs_transcript() {
    // Populate a cache with a "click" decision.
    let mut cache = AgentCache::default();
    let elements = vec![clickweave_core::cdp::CdpFindElementMatch {
        uid: "1_0".to_string(),
        role: "button".to_string(),
        label: "Submit".to_string(),
        tag: "button".to_string(),
        disabled: false,
        parent_role: None,
        parent_name: None,
    }];
    cache.store(
        "Click the submit button",
        &elements,
        "click".to_string(),
        serde_json::json!({"x": 100, "y": 200}),
    );

    // Capture the messages passed to the LLM on each call.
    let captured = Arc::new(Mutex::new(Vec::<Vec<Message>>::new()));
    let agent_llm = CapturingMockAgent::new(
        vec![MockAgent::done_response("Done via cache")],
        captured.clone(),
    );

    let mcp_results = vec![
        // cdp_find_elements for step 0
        ToolCallResult {
            content: vec![ToolContent::Text {
                text: serde_json::json!({
                    "page_url": "https://example.com",
                    "source": "cdp",
                    "matches": [{
                        "uid": "1_0",
                        "role": "button",
                        "label": "Submit",
                        "tag": "button"
                    }]
                })
                .to_string(),
            }],
            is_error: None,
        },
        // click result for cached replay
        ToolCallResult {
            content: vec![ToolContent::Text {
                text: "Clicked at (100, 200)".to_string(),
            }],
            is_error: None,
        },
        // cdp_find_elements for step 1
        ToolCallResult {
            content: vec![ToolContent::Text {
                text: serde_json::json!({
                    "page_url": "https://example.com/next",
                    "source": "cdp",
                    "matches": [{
                        "uid": "2_0",
                        "role": "heading",
                        "label": "Next Page",
                        "tag": "h1"
                    }]
                })
                .to_string(),
            }],
            is_error: None,
        },
    ];
    let tools = vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "click",
            "description": "Click at coordinates",
            "parameters": {
                "type": "object",
                "properties": {
                    "x": {"type": "number"},
                    "y": {"type": "number"}
                },
                "required": ["x", "y"]
            }
        }
    })];
    let mcp = MockMcp::new(mcp_results, tools);

    let config = AgentConfig {
        max_steps: 10,
        build_workflow: false,
        use_cache: true,
        ..Default::default()
    };

    let mut runner = AgentRunner::with_cache(&agent_llm, config, cache);
    let workflow = clickweave_core::Workflow::new("Transcript Test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click the submit button".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(state.completed);

    // The LLM should have been called once (for the done step, after cache
    // replay). The messages passed should include the reconstructed assistant
    // tool_call and the tool result from the cached click.
    let calls = captured.lock().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "LLM should be called once after cache replay"
    );

    let messages = &calls[0];

    // Find the assistant message with tool_calls for the cached click
    let has_assistant_tool_call = messages.iter().any(|m| {
        m.role == Role::Assistant
            && m.tool_calls
                .as_ref()
                .map(|tcs| tcs.iter().any(|tc| tc.function.name == "click"))
                .unwrap_or(false)
    });
    assert!(
        has_assistant_tool_call,
        "Transcript should contain an assistant tool_call message for the cached click"
    );

    // Find the tool result message for the cached click
    let has_tool_result = messages.iter().any(|m| {
        m.role == Role::Tool
            && m.tool_call_id
                .as_ref()
                .map(|id| id.starts_with("cache-"))
                .unwrap_or(false)
    });
    assert!(
        has_tool_result,
        "Transcript should contain a tool result message for the cached click"
    );
}

// ---------------------------------------------------------------------------
// Multi-tool response only executes first
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multi_tool_response_only_executes_first() {
    // LLM returns two tool calls in a single response; only the first should run.
    let agent_llm = MockAgent::new(vec![
        ChatResponse {
            id: "mock-multi".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message::assistant_tool_calls(vec![
                    ToolCall {
                        id: "call_first".to_string(),
                        call_type: CallType::Function,
                        function: FunctionCall {
                            name: "click".to_string(),
                            arguments: serde_json::json!({ "x": 10, "y": 20 }),
                        },
                    },
                    ToolCall {
                        id: "call_second".to_string(),
                        call_type: CallType::Function,
                        function: FunctionCall {
                            name: "click".to_string(),
                            arguments: serde_json::json!({ "x": 300, "y": 400 }),
                        },
                    },
                ]),
                finish_reason: Some("tool_calls".to_string()),
            }],
            usage: None,
        },
        MockAgent::done_response("Clicked once"),
    ]);

    let mcp = MockMcp::with_click_tool();
    let config = AgentConfig {
        max_steps: 10,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let mut runner = AgentRunner::new(&agent_llm, config);
    let workflow = clickweave_core::Workflow::new("Multi-tool Test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click a button".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(state.completed);
    // Two steps: the multi-tool response (only first executed) + agent_done
    assert_eq!(
        state.steps.len(),
        2,
        "Should have exactly 2 steps (one click + done)"
    );

    // The first step should use the first tool call's ID
    match &state.steps[0].command {
        AgentCommand::ToolCall { tool_call_id, .. } => {
            assert_eq!(
                tool_call_id, "call_first",
                "Only the first tool call should be executed"
            );
        }
        other => panic!("Expected ToolCall, got {:?}", other),
    }

    // First step should succeed (only one click executed)
    assert!(
        matches!(state.steps[0].outcome, StepOutcome::Success(_)),
        "First tool call should succeed, got {:?}",
        state.steps[0].outcome,
    );
}

// ---------------------------------------------------------------------------
// Malformed tool-call JSON returns error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn malformed_tool_call_json_returns_error() {
    let agent_llm = MockAgent::new(vec![
        // LLM returns a tool call with unparseable arguments
        ChatResponse {
            id: "mock-malformed".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message::assistant_tool_calls(vec![ToolCall {
                    id: "call_bad".to_string(),
                    call_type: CallType::Function,
                    function: FunctionCall {
                        name: "click".to_string(),
                        arguments: Value::String("not valid json{{{".to_string()),
                    },
                }]),
                finish_reason: Some("tool_calls".to_string()),
            }],
            usage: None,
        },
        MockAgent::done_response("Recovered from bad JSON"),
    ]);

    let mcp = MockMcp::with_click_tool();
    let config = AgentConfig {
        max_steps: 10,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let mut runner = AgentRunner::new(&agent_llm, config);
    let workflow = clickweave_core::Workflow::new("Malformed JSON Test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click something".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(state.completed);
    assert!(
        state.steps.len() >= 2,
        "Should have at least 2 steps (error + done)"
    );

    // The first step should be an error due to malformed JSON
    assert!(
        matches!(&state.steps[0].outcome, StepOutcome::Error(msg) if msg.contains("Malformed")),
        "First step should be a malformed-arguments error, got {:?}",
        state.steps[0].outcome,
    );
}

// ---------------------------------------------------------------------------
// Context bound: multi-step snapshot history stays below the model window
// ---------------------------------------------------------------------------

/// Simulates the real-world failure mode from the bug report: a multi-step
/// agent run that issues several snapshot-producing tool calls back-to-back.
/// Without the supersession pass, each full CDP snapshot accumulated in
/// history would blow past the LLM's context limit after 4-5 steps. This
/// test asserts that the retained message token count stays well under 40k
/// (the smallest provider context we ship against) across 6 such calls,
/// while the most recent snapshot is preserved at full fidelity.
#[tokio::test]
async fn retained_history_stays_bounded_across_snapshot_heavy_steps() {
    use crate::agent::context::{collapse_superseded_snapshots, estimate_messages_tokens};

    // Capture every message batch the LLM receives.
    let captured: Arc<Mutex<Vec<Vec<Message>>>> = Arc::new(Mutex::new(Vec::new()));

    // LLM chooses cdp_wait_for six times then completes.
    let mut responses: Vec<ChatResponse> = (0..6)
        .map(|i| {
            MockAgent::tool_call_response(
                "cdp_wait_for",
                r#"{"text": "ready"}"#,
                &format!("call_wait_{}", i),
            )
        })
        .collect();
    responses.push(MockAgent::done_response("Multi-step workflow finished"));
    let agent_llm = CapturingMockAgent::new(responses, captured.clone());

    // Build fake snapshot blobs large enough that 6 of them would OOM the
    // context if they were all retained at full fidelity. Each 32 KiB
    // snapshot ≈ 8192 tokens, so 6 retained snapshots alone ≈ 49k tokens,
    // comfortably above the 30k ceiling the assertion enforces below.
    let snapshot_body_kb = 32;
    let big_snapshot = "s".repeat(snapshot_body_kb * 1024);

    // MCP result queue: alternating cdp_find_elements observation results
    // (returned with a real CdpFindElementsResponse structure) and
    // cdp_wait_for tool bodies containing the fat snapshot payload.
    let mut mcp_results: Vec<ToolCallResult> = Vec::new();
    for i in 0..7 {
        // cdp_find_elements observation for step i
        mcp_results.push(ToolCallResult {
            content: vec![ToolContent::Text {
                text: serde_json::json!({
                    "page_url": format!("https://example.com/step/{}", i),
                    "source": "cdp",
                    "matches": [{
                        "uid": format!("1_{}", i),
                        "role": "button",
                        "label": "Submit",
                        "tag": "button"
                    }]
                })
                .to_string(),
            }],
            is_error: None,
        });
        if i < 6 {
            // cdp_wait_for tool call result with the big snapshot payload
            mcp_results.push(ToolCallResult {
                content: vec![ToolContent::Text {
                    text: big_snapshot.clone(),
                }],
                is_error: None,
            });
        }
    }

    let tools = vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "cdp_wait_for",
            "description": "Wait for a condition on the page",
            "parameters": {
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"]
            }
        }
    })];
    let mcp = MockMcp::new(mcp_results, tools);

    let config = AgentConfig {
        max_steps: 10,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let mut runner = AgentRunner::new(&agent_llm, config);
    let workflow = clickweave_core::Workflow::new("Snapshot-heavy Test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Wait for a bunch of events".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();
    assert!(state.completed);

    // Inspect the captured message batches: the last batch the LLM saw
    // (before the agent_done call) is the one that would have been sent to
    // the real provider at the peak.
    let calls = captured.lock().unwrap();
    assert!(calls.len() >= 6, "Expected at least 6 LLM calls");

    let peak_messages = calls
        .iter()
        .max_by_key(|m| estimate_messages_tokens(m))
        .unwrap();
    let peak_tokens = estimate_messages_tokens(peak_messages);

    // Retained history must stay well below the 40k production context
    // ceiling — we want enough headroom for the system prompt, tool schema,
    // and the model response. Without supersession, the coarse compaction
    // alone keeps three 32 KiB snapshots in the recent-message window (~26k
    // tokens), which still risks an OOM once the system prompt and tool
    // schema are added. The assertion proves the supersession pass sheds
    // those redundant payloads.
    assert!(
        peak_tokens < 15_000,
        "retained history exceeded sane token budget: {} tokens",
        peak_tokens
    );

    // Sanity: the most recent snapshot tool result must still carry its full
    // payload. Re-run the pure collapse function on the peak transcript and
    // locate the final cdp_wait_for result. It should not be placeholdered.
    let latest =
        collapse_superseded_snapshots(peak_messages).unwrap_or_else(|| peak_messages.clone());
    let last_wait_for_result = latest
        .iter()
        .rev()
        .find(|m| {
            m.role == Role::Tool
                && m.tool_call_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("call_wait_"))
        })
        .expect("a wait_for tool result should survive in the peak transcript");
    let body = last_wait_for_result.content_text().unwrap_or_default();
    assert!(
        body.len() > 1024,
        "most recent snapshot was incorrectly collapsed (len={})",
        body.len()
    );
}

// ---------------------------------------------------------------------------
// Tool exposure stability: the tool list passed to the LLM must not mutate
// across steps, even when an auto-connect CDP sub-action runs between them.
// Mid-conversation tool-list changes invalidate every prior prompt-cache
// prefix; see the "Tool Exposure" policy in docs/reference/engine/execution.md.
// ---------------------------------------------------------------------------

/// Mock agent that captures the tool list received on every LLM call.
struct ToolCapturingAgent {
    responses: Mutex<Vec<ChatResponse>>,
    captured_tools: Arc<Mutex<Vec<Vec<Value>>>>,
}

impl ToolCapturingAgent {
    fn new(responses: Vec<ChatResponse>, captured: Arc<Mutex<Vec<Vec<Value>>>>) -> Self {
        Self {
            responses: Mutex::new(responses),
            captured_tools: captured,
        }
    }
}

impl ChatBackend for ToolCapturingAgent {
    fn model_name(&self) -> &str {
        "tool-capturing-mock-agent"
    }

    async fn chat_with_options(
        &self,
        _messages: &[Message],
        tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> Result<ChatResponse> {
        self.captured_tools
            .lock()
            .unwrap()
            .push(tools.map(|t| t.to_vec()).unwrap_or_default());
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            Ok(MockAgent::done_response("No more responses"))
        } else {
            Ok(responses.remove(0))
        }
    }

    async fn fetch_model_info(&self) -> Result<Option<ModelInfo>> {
        Ok(None)
    }
}

/// Mock MCP that models the real `McpClient` cache semantics: a single
/// tool snapshot backs both `has_tool` and `tools_as_openai`, and it only
/// updates when `refresh_server_tool_list` is called. The server's "true" tool set
/// grows after `cdp_connect` (the extras become available), but the mock
/// will keep returning the stale snapshot until refreshed — matching what
/// the production client does.
struct ShiftingToolsMcp {
    results: Mutex<Vec<ToolCallResult>>,
    base_tools: Vec<Value>,
    extra_tools: Vec<Value>,
    /// Server-side visibility: flips to true on `cdp_connect`.
    cdp_connected: std::sync::atomic::AtomicBool,
    /// Client-side cached snapshot of tools; only updated by `refresh_server_tool_list`.
    cached_tools: Mutex<Vec<Value>>,
}

impl ShiftingToolsMcp {
    fn new(results: Vec<ToolCallResult>, base_tools: Vec<Value>, extra_tools: Vec<Value>) -> Self {
        Self {
            results: Mutex::new(results),
            cached_tools: Mutex::new(base_tools.clone()),
            base_tools,
            extra_tools,
            cdp_connected: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// What the server reports in `tools/list` right now. Grows after
    /// `cdp_connect` succeeds.
    fn server_visible_tools(&self) -> Vec<Value> {
        if self.cdp_connected.load(std::sync::atomic::Ordering::SeqCst) {
            self.base_tools
                .iter()
                .chain(self.extra_tools.iter())
                .cloned()
                .collect()
        } else {
            self.base_tools.clone()
        }
    }
}

impl Mcp for ShiftingToolsMcp {
    async fn call_tool(
        &self,
        name: &str,
        _arguments: Option<Value>,
    ) -> anyhow::Result<ToolCallResult> {
        if name == "cdp_connect" {
            self.cdp_connected
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let mut results = self.results.lock().unwrap();
        if results.is_empty() {
            Ok(ToolCallResult {
                content: vec![ToolContent::Text {
                    text: "ok".to_string(),
                }],
                is_error: None,
            })
        } else {
            Ok(results.remove(0))
        }
    }

    fn has_tool(&self, name: &str) -> bool {
        self.cached_tools
            .lock()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"].as_str() == Some(name))
    }

    fn tools_as_openai(&self) -> Vec<Value> {
        self.cached_tools.lock().unwrap().clone()
    }

    async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
        *self.cached_tools.lock().unwrap() = self.server_visible_tools();
        Ok(())
    }
}

#[tokio::test]
async fn tool_list_is_stable_across_cdp_connect_boundary() {
    // The LLM picks launch_app first (which triggers the auto CDP connect
    // sub-actions), then click on the next step, then declares done.
    let captured: Arc<Mutex<Vec<Vec<Value>>>> = Arc::new(Mutex::new(Vec::new()));
    let agent_llm = ToolCapturingAgent::new(
        vec![
            MockAgent::tool_call_response(
                "launch_app",
                r#"{"app_name": "Some Electron App"}"#,
                "call_launch",
            ),
            MockAgent::tool_call_response("click", r#"{"x": 10, "y": 20}"#, "call_click"),
            MockAgent::done_response("All done after launch + click"),
        ],
        captured.clone(),
    );

    // MCP results queue matches the expected call sequence. Pre-connect,
    // `cdp_find_elements` is not in the client's tool cache, so step 0's
    // observation is a no-op (empty elements) and consumes no result.
    //   step 0 act      -> launch_app
    //   post-hook probe -> probe_app (must say ElectronApp to trigger CDP)
    //   post-hook quit  -> quit_app
    //   post-hook list  -> list_apps (empty so quit is considered done)
    //   post-hook relaunch -> launch_app
    //   post-hook connect  -> cdp_connect (flips the server's tool set)
    //   (refresh_server_tool_list reloads the client cache after connect)
    //   step 1 observe  -> cdp_find_elements
    //   step 1 act      -> click
    //   step 2 observe  -> cdp_find_elements
    let cdp_page = |url: &str| ToolCallResult {
        content: vec![ToolContent::Text {
            text: serde_json::json!({
                "page_url": url,
                "source": "cdp",
                "matches": [{
                    "uid": "1_0",
                    "role": "button",
                    "label": "Submit",
                    "tag": "button"
                }]
            })
            .to_string(),
        }],
        is_error: None,
    };
    let text = |s: &str| ToolCallResult {
        content: vec![ToolContent::Text {
            text: s.to_string(),
        }],
        is_error: None,
    };
    let results = vec![
        text("Launched"),         // launch_app
        text("ElectronApp"),      // probe_app
        text("ok"),               // quit_app
        text("[]"),               // list_apps: confirms quit
        text("Launched on port"), // relaunch launch_app
        text("connected"),        // cdp_connect
        // Post-connect selected-page snapshot (agent now tracks the
        // remembered tab like the executor does).
        text("Pages (1 total):\n  [0]* https://example.com/initial\n"),
        cdp_page("https://example.com/after"),
        text("Clicked"), // click
        cdp_page("https://example.com/final"),
    ];

    let base_tools = vec![
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "launch_app",
                "description": "Launch an app",
                "parameters": {
                    "type": "object",
                    "properties": {"app_name": {"type": "string"}},
                    "required": ["app_name"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "click",
                "description": "Click at coordinates",
                "parameters": {
                    "type": "object",
                    "properties": {"x": {"type": "number"}, "y": {"type": "number"}},
                    "required": ["x", "y"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "probe_app",
                "description": "Probe an app",
                "parameters": {
                    "type": "object",
                    "properties": {"app_name": {"type": "string"}},
                    "required": ["app_name"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "cdp_connect",
                "description": "Connect to CDP",
                "parameters": {
                    "type": "object",
                    "properties": {"port": {"type": "number"}},
                    "required": ["port"]
                }
            }
        }),
    ];
    // Extras model CDP tools the server only surfaces after `cdp_connect`:
    //   - `cdp_find_elements` is what the agent's observation gate checks
    //     (`has_tool(...)` in `fetch_elements`), so it must become visible
    //     on the *client-side cache* after the post-hook runs, or every
    //     later observation will return empty.
    //   - `cdp_click` stands in for any CDP tool that must NOT silently
    //     show up in the agent's LLM-visible tool list mid-run.
    let extra_tools = vec![
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "cdp_find_elements",
                "description": "Find elements via CDP",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "max_results": {"type": "number"}
                    }
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "cdp_click",
                "description": "Click via CDP",
                "parameters": {
                    "type": "object",
                    "properties": {"uid": {"type": "string"}},
                    "required": ["uid"]
                }
            }
        }),
    ];

    let mcp = ShiftingToolsMcp::new(results, base_tools, extra_tools);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let mut runner = AgentRunner::new(&agent_llm, config);
    let workflow = clickweave_core::Workflow::new("Stable tools test");
    // Seed the tools vec from the MCP client once at run start — mirrors
    // how `run_agent_workflow` wires it up.
    let mcp_tools = mcp.tools_as_openai();
    let tool_count_at_start = mcp_tools.len();

    let state = runner
        .run(
            "Launch and click".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(state.completed);

    // Sanity: the MCP server's view of its own tools did grow after cdp_connect.
    assert!(
        mcp.tools_as_openai().len() > tool_count_at_start,
        "Test setup broken: ShiftingToolsMcp should expose more tools post-connect"
    );

    // The client-side tool cache must have been refreshed after cdp_connect
    // — otherwise later observation steps would see `has_tool("cdp_find_elements")`
    // return false and degrade to empty-element native paths.
    assert!(
        mcp.has_tool("cdp_find_elements"),
        "Post-CDP-connect refresh did not run: cdp_find_elements is still \
         absent from the client tool cache, so fetch_elements would return \
         empty on every later observation."
    );

    // And the agent's recorded step for the post-connect click should carry
    // a CDP-sourced page_url, which only happens if fetch_elements actually
    // dispatched `cdp_find_elements` — i.e. the gate in fetch_elements saw
    // the refreshed cache.
    let click_step = state
        .steps
        .iter()
        .find(|s| matches!(&s.command, AgentCommand::ToolCall { tool_name, .. } if tool_name == "click"))
        .expect("click step should be present");
    assert_eq!(
        click_step.page_url, "https://example.com/after",
        "Expected the click step to observe via CDP after the connect boundary"
    );

    let calls = captured.lock().unwrap();
    assert!(
        calls.len() >= 2,
        "Need at least two LLM calls to compare across a CDP connect boundary"
    );

    // Every LLM call within a single run must see an identical tool list.
    let first = &calls[0];
    for (i, later) in calls.iter().enumerate().skip(1) {
        assert_eq!(
            first, later,
            "Tool list diverged between LLM call 0 and call {i}; \
             mid-run tool mutation invalidates the prompt cache prefix"
        );
    }

    // And the CDP-only tool must *not* have been smuggled into the agent's
    // tool list after the post-hook connect.
    let has_cdp_click = first.iter().any(|t| {
        t.get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            == Some("cdp_click")
    });
    assert!(
        !has_cdp_click,
        "cdp_click leaked into the agent's tool list after auto CDP connect; \
         run-start seed must be the stable contract"
    );
}

// ---------------------------------------------------------------------------
// Completion verification (post-agent_done VLM check)
// ---------------------------------------------------------------------------

/// Hardcoded 1x1 transparent PNG as base64 — used so `prepare_base64_image_for_vlm`
/// has a genuinely decodable image without pulling the `image` crate into
/// clickweave-engine's test deps.
const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkAAIAAAoAAv/lxKUAAAAASUVORK5CYII=";

/// MCP mock that dispatches by tool name — required for completion tests
/// because the loop issues cdp_find_elements and take_screenshot in the
/// same run, and order is not predictable without tool-aware dispatch.
struct RoutingMockMcp {
    /// Sequential responses for `cdp_find_elements`.
    find_elements: Mutex<Vec<ToolCallResult>>,
    /// Sequential responses for `take_screenshot`.
    screenshots: Mutex<Vec<ToolCallResult>>,
    /// Tools advertised through `tools_as_openai`.
    tools: Vec<Value>,
}

impl RoutingMockMcp {
    fn new(find_elements: Vec<ToolCallResult>, screenshots: Vec<ToolCallResult>) -> Self {
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "take_screenshot",
                "description": "Take a screenshot",
                "parameters": {"type": "object", "properties": {}}
            }
        })];
        Self {
            find_elements: Mutex::new(find_elements),
            screenshots: Mutex::new(screenshots),
            tools,
        }
    }
}

impl Mcp for RoutingMockMcp {
    async fn call_tool(
        &self,
        name: &str,
        _arguments: Option<Value>,
    ) -> anyhow::Result<ToolCallResult> {
        let queue = match name {
            "cdp_find_elements" => &self.find_elements,
            "take_screenshot" => &self.screenshots,
            _ => {
                return Ok(ToolCallResult {
                    content: vec![ToolContent::Text {
                        text: "ok".to_string(),
                    }],
                    is_error: None,
                });
            }
        };
        let mut q = queue.lock().unwrap();
        if q.is_empty() {
            Ok(ToolCallResult {
                content: vec![ToolContent::Text {
                    text: "ok".to_string(),
                }],
                is_error: None,
            })
        } else {
            Ok(q.remove(0))
        }
    }

    fn has_tool(&self, name: &str) -> bool {
        if name == "cdp_find_elements" || name == "take_screenshot" {
            return true;
        }
        self.tools
            .iter()
            .any(|t| t["function"]["name"].as_str() == Some(name))
    }

    fn tools_as_openai(&self) -> Vec<Value> {
        self.tools.clone()
    }

    async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// MockAgent variant that distinguishes tool-call requests (agent role) from
/// chat requests without tools (VLM role). The agent queue handles requests
/// with a `tools` argument; the vision queue handles requests without.
struct RoutingMockAgent {
    agent_responses: Mutex<Vec<ChatResponse>>,
    vision_responses: Mutex<Vec<ChatResponse>>,
}

impl RoutingMockAgent {
    fn new(agent: Vec<ChatResponse>, vision: Vec<ChatResponse>) -> Self {
        Self {
            agent_responses: Mutex::new(agent),
            vision_responses: Mutex::new(vision),
        }
    }

    fn text_response(text: &str) -> ChatResponse {
        ChatResponse {
            id: "mock-text".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message::assistant(text),
                finish_reason: Some("stop".to_string()),
            }],
            usage: None,
        }
    }
}

impl ChatBackend for RoutingMockAgent {
    fn model_name(&self) -> &str {
        "routing-mock"
    }

    async fn chat_with_options(
        &self,
        _messages: &[Message],
        tools: Option<&[Value]>,
        _options: &ChatOptions,
    ) -> Result<ChatResponse> {
        let queue = if tools.is_some() {
            &self.agent_responses
        } else {
            &self.vision_responses
        };
        let mut q = queue.lock().unwrap();
        if q.is_empty() {
            // Fallback that keeps the loop from hanging.
            Ok(MockAgent::done_response("No more responses"))
        } else {
            Ok(q.remove(0))
        }
    }

    async fn fetch_model_info(&self) -> Result<Option<ModelInfo>> {
        Ok(None)
    }
}

fn cdp_empty_page_result() -> ToolCallResult {
    ToolCallResult {
        content: vec![ToolContent::Text {
            text: serde_json::json!({
                "page_url": "about:blank",
                "source": "cdp",
                "matches": []
            })
            .to_string(),
        }],
        is_error: None,
    }
}

fn screenshot_result() -> ToolCallResult {
    ToolCallResult {
        content: vec![ToolContent::Image {
            data: TINY_PNG_BASE64.to_string(),
            mime_type: "image/png".to_string(),
        }],
        is_error: None,
    }
}

#[tokio::test]
async fn vlm_yes_verdict_completes_run_normally() {
    // Agent calls agent_done on step 0; vision backend replies YES.
    let agent_backend = RoutingMockAgent::new(
        vec![MockAgent::done_response("Task finished")],
        vec![RoutingMockAgent::text_response(
            "YES, the screenshot shows the expected state.",
        )],
    );

    let mcp = RoutingMockMcp::new(vec![cdp_empty_page_result()], vec![screenshot_result()]);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let mut runner = AgentRunner::new(&agent_backend, config).with_vision(&agent_backend);
    let workflow = clickweave_core::Workflow::new("VLM YES test");
    let mcp_tools = mcp.tools_as_openai();

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(16);
    runner = runner.with_events(event_tx);

    let state = runner
        .run(
            "Open settings".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(state.completed, "YES should let the run complete");
    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::Completed { .. })
        ),
        "Expected Completed, got {:?}",
        state.terminal_reason,
    );

    let mut saw_goal_complete = false;
    let mut saw_disagreement = false;
    while let Ok(ev) = event_rx.try_recv() {
        match ev {
            AgentEvent::GoalComplete { .. } => saw_goal_complete = true,
            AgentEvent::CompletionDisagreement { .. } => saw_disagreement = true,
            _ => {}
        }
    }
    assert!(saw_goal_complete, "Expected GoalComplete event");
    assert!(
        !saw_disagreement,
        "YES must not emit CompletionDisagreement"
    );
}

#[tokio::test]
async fn vlm_no_verdict_halts_run_and_emits_disagreement() {
    // Agent calls agent_done; vision backend replies NO — the run must halt
    // with CompletionDisagreement and emit a disagreement event.
    let agent_backend = RoutingMockAgent::new(
        vec![MockAgent::done_response("I think it's done")],
        vec![RoutingMockAgent::text_response(
            "NO — the page still shows the previous state.",
        )],
    );

    let mcp = RoutingMockMcp::new(vec![cdp_empty_page_result()], vec![screenshot_result()]);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(16);
    let mut runner = AgentRunner::new(&agent_backend, config)
        .with_vision(&agent_backend)
        .with_events(event_tx);
    let workflow = clickweave_core::Workflow::new("VLM NO test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Open settings".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(!state.completed, "NO must not mark the run completed");
    match state.terminal_reason {
        Some(TerminalReason::CompletionDisagreement {
            ref agent_summary,
            ref vlm_reasoning,
        }) => {
            assert_eq!(agent_summary, "I think it's done");
            assert!(vlm_reasoning.to_uppercase().starts_with("NO"));
        }
        other => panic!("Expected CompletionDisagreement, got {:?}", other),
    }

    let mut disagreement_payload: Option<(String, String, String)> = None;
    let mut saw_goal_complete = false;
    while let Ok(ev) = event_rx.try_recv() {
        match ev {
            AgentEvent::CompletionDisagreement {
                screenshot_b64,
                vlm_reasoning,
                agent_summary,
            } => {
                disagreement_payload = Some((screenshot_b64, vlm_reasoning, agent_summary));
            }
            AgentEvent::GoalComplete { .. } => saw_goal_complete = true,
            _ => {}
        }
    }
    let (screenshot_b64, vlm_reasoning, agent_summary) =
        disagreement_payload.expect("Expected CompletionDisagreement event");
    assert!(
        !screenshot_b64.is_empty(),
        "Disagreement event must carry the screenshot bytes",
    );
    assert!(vlm_reasoning.to_uppercase().starts_with("NO"));
    assert_eq!(agent_summary, "I think it's done");
    assert!(
        !saw_goal_complete,
        "NO must not emit GoalComplete alongside the disagreement",
    );
}

#[tokio::test]
async fn vlm_check_falls_through_when_reply_is_empty() {
    // Non-vision endpoints commonly return an empty content body rather
    // than erroring. The loop must treat that as a verifier failure and
    // fall through to Completed, not halt with CompletionDisagreement.
    let agent_backend = RoutingMockAgent::new(
        vec![MockAgent::done_response("Done")],
        vec![RoutingMockAgent::text_response("")],
    );

    let mcp = RoutingMockMcp::new(vec![cdp_empty_page_result()], vec![screenshot_result()]);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let mut runner = AgentRunner::new(&agent_backend, config).with_vision(&agent_backend);
    let workflow = clickweave_core::Workflow::new("VLM empty reply fallback");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Do it".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(
        state.completed,
        "Empty VLM reply must fall through to Completed"
    );
    assert!(matches!(
        state.terminal_reason,
        Some(TerminalReason::Completed { .. })
    ));
}

#[tokio::test]
async fn vlm_check_falls_through_when_screenshot_fails() {
    // Agent calls agent_done; take_screenshot returns an error. The loop
    // must complete normally rather than hang or halt.
    let agent_backend = RoutingMockAgent::new(
        vec![MockAgent::done_response("Done")],
        vec![/* vision should never be called */],
    );

    let failing_screenshot = ToolCallResult {
        content: vec![ToolContent::Text {
            text: "No focused window".to_string(),
        }],
        is_error: Some(true),
    };

    let mcp = RoutingMockMcp::new(vec![cdp_empty_page_result()], vec![failing_screenshot]);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };

    let mut runner = AgentRunner::new(&agent_backend, config).with_vision(&agent_backend);
    let workflow = clickweave_core::Workflow::new("VLM error fallback");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Do it".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(
        state.completed,
        "Screenshot failure must NOT halt the run — fall through to Completed"
    );
    assert!(matches!(
        state.terminal_reason,
        Some(TerminalReason::Completed { .. })
    ));
}

// ---------------------------------------------------------------------------
// Permission policy + consecutive-destructive cap tests
// ---------------------------------------------------------------------------

/// Build an OpenAI-shaped tool JSON blob with optional MCP annotations.
/// Mirrors what `tools_to_openai` produces, plus an `annotations` block
/// under `function` so the engine's annotation-index picks it up. When
/// `annotations` is `None`, the block is omitted entirely (representing
/// a tool that the server did not annotate).
fn tool_with_annotations(name: &str, annotations: Option<serde_json::Value>) -> Value {
    let mut function = serde_json::json!({
        "name": name,
        "description": format!("Tool {}", name),
        "parameters": {
            "type": "object",
            "properties": {"x": {"type": "number"}, "y": {"type": "number"}},
            "required": []
        }
    });
    if let Some(ann) = annotations {
        function["annotations"] = ann;
    }
    serde_json::json!({
        "type": "function",
        "function": function
    })
}

/// Convenience: destructive_hint = true, everything else missing.
fn destructive_tool(name: &str) -> Value {
    tool_with_annotations(name, Some(serde_json::json!({"destructiveHint": true})))
}

/// Mock MCP that hands back a canned sequence of results, never probes
/// CDP, and reports whatever tools it was built with. Used for policy
/// integration tests where we just need `click` (or similar) to respond.
fn build_mcp_with_tool(name: &str, annotations: Option<serde_json::Value>) -> MockMcp {
    let tools = vec![tool_with_annotations(name, annotations)];
    // Infinite supply of success results — the policy tests don't care
    // about tool-result content, only about whether the tool ran.
    let results: Vec<ToolCallResult> = (0..20)
        .flat_map(|i| {
            vec![
                // Observation result
                ToolCallResult {
                    content: vec![ToolContent::Text {
                        text: serde_json::json!({
                            "page_url": format!("https://example.com/{}", i),
                            "source": "cdp",
                            "matches": [{
                                "uid": format!("{}_0", i),
                                "role": "button",
                                "label": "Do it",
                                "tag": "button"
                            }]
                        })
                        .to_string(),
                    }],
                    is_error: None,
                },
                // Tool result
                ToolCallResult {
                    content: vec![ToolContent::Text {
                        text: format!("done {}", i),
                    }],
                    is_error: None,
                },
            ]
        })
        .collect();
    MockMcp::new(results, tools)
}

/// Counts approval requests received through the channel, replying
/// `approved` to each.
#[allow(clippy::type_complexity)]
fn spawn_approval_counter(
    approved: bool,
) -> (
    tokio::sync::mpsc::Sender<(ApprovalRequest, tokio::sync::oneshot::Sender<bool>)>,
    Arc<Mutex<usize>>,
) {
    let (approval_tx, mut approval_rx) =
        tokio::sync::mpsc::channel::<(ApprovalRequest, tokio::sync::oneshot::Sender<bool>)>(8);
    let counter = Arc::new(Mutex::new(0usize));
    let counter_clone = counter.clone();
    tokio::spawn(async move {
        while let Some((_req, resp_tx)) = approval_rx.recv().await {
            *counter_clone.lock().unwrap() += 1;
            let _ = resp_tx.send(approved);
        }
    });
    (approval_tx, counter)
}

#[tokio::test]
async fn policy_deny_fails_step_without_prompting() {
    use crate::agent::{PermissionAction, PermissionPolicy, PermissionRule};

    // Two click calls followed by agent_done. The LLM cannot escape
    // because the policy denies click every time, so the run ends via
    // max-errors or completes once the LLM gives up.
    let agent_llm = MockAgent::new(vec![
        MockAgent::tool_call_response("click", r#"{"x": 10, "y": 20}"#, "call_0"),
        MockAgent::done_response("Gave up — tool was denied"),
    ]);
    let mcp = build_mcp_with_tool("click", None);

    let policy = PermissionPolicy {
        rules: vec![PermissionRule {
            tool_pattern: "click".to_string(),
            args_pattern: None,
            action: PermissionAction::Deny,
        }],
        ..Default::default()
    };

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(64);
    let (approval_tx, approval_count) = spawn_approval_counter(true);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };
    let mut runner = AgentRunner::new(&agent_llm, config)
        .with_events(event_tx)
        .with_approval(approval_tx)
        .with_permissions(policy);
    let workflow = clickweave_core::Workflow::new("Deny test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    // Deny emits a step error (not a Replan) and does not prompt the
    // user at all.
    assert_eq!(*approval_count.lock().unwrap(), 0);
    assert!(
        matches!(
            &state.steps[0].outcome,
            StepOutcome::Error(msg) if msg.contains("denied by permission policy")
        ),
        "Expected Error(denied by permission policy), got {:?}",
        state.steps[0].outcome,
    );
}

#[tokio::test]
async fn policy_allow_skips_approval_prompt() {
    use crate::agent::{PermissionAction, PermissionPolicy, PermissionRule};

    let agent_llm = MockAgent::new(vec![
        MockAgent::tool_call_response("click", r#"{"x": 10, "y": 20}"#, "call_0"),
        MockAgent::done_response("Clicked"),
    ]);
    let mcp = build_mcp_with_tool("click", None);

    let policy = PermissionPolicy {
        rules: vec![PermissionRule {
            tool_pattern: "click".to_string(),
            args_pattern: None,
            action: PermissionAction::Allow,
        }],
        ..Default::default()
    };

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(64);
    let (approval_tx, approval_count) = spawn_approval_counter(true);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };
    let mut runner = AgentRunner::new(&agent_llm, config)
        .with_events(event_tx)
        .with_approval(approval_tx)
        .with_permissions(policy);
    let workflow = clickweave_core::Workflow::new("Allow test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Click".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert_eq!(
        *approval_count.lock().unwrap(),
        0,
        "policy Allow must bypass the approval prompt entirely",
    );
    assert!(state.completed);
    assert!(matches!(state.steps[0].outcome, StepOutcome::Success(_)));
}

#[tokio::test]
async fn destructive_guardrail_still_prompts_when_tool_allowed() {
    use crate::agent::{PermissionAction, PermissionPolicy, PermissionRule};

    // delete_file has destructiveHint = true. User marked it "allow" at
    // the tool level, but require_confirm_destructive is on → Ask wins.
    let agent_llm = MockAgent::new(vec![
        MockAgent::tool_call_response("delete_file", r#"{"path": "a"}"#, "call_0"),
        MockAgent::done_response("Deleted"),
    ]);
    let mcp = build_mcp_with_tool(
        "delete_file",
        Some(serde_json::json!({"destructiveHint": true})),
    );

    let policy = PermissionPolicy {
        rules: vec![PermissionRule {
            tool_pattern: "delete_file".to_string(),
            args_pattern: None,
            action: PermissionAction::Allow,
        }],
        require_confirm_destructive: true,
        ..Default::default()
    };

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(64);
    let (approval_tx, approval_count) = spawn_approval_counter(true);

    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        use_cache: false,
        ..Default::default()
    };
    let mut runner = AgentRunner::new(&agent_llm, config)
        .with_events(event_tx)
        .with_approval(approval_tx)
        .with_permissions(policy);
    let workflow = clickweave_core::Workflow::new("Guardrail test");
    let mcp_tools = mcp.tools_as_openai();

    let _state = runner
        .run(
            "Delete".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert_eq!(
        *approval_count.lock().unwrap(),
        1,
        "destructive-guardrail must force an approval prompt even when the tool is Allow-listed",
    );
}

// ── Consecutive-destructive cap ─────────────────────────────────

/// Build a mock MCP that exposes multiple destructive tools, with an
/// endless supply of observation + success pairs.
fn build_mcp_with_destructive_tools(names: &[&str]) -> MockMcp {
    let tools: Vec<Value> = names.iter().map(|n| destructive_tool(n)).collect();
    let results: Vec<ToolCallResult> = (0..30)
        .flat_map(|i| {
            vec![
                ToolCallResult {
                    content: vec![ToolContent::Text {
                        text: serde_json::json!({
                            "page_url": format!("https://example.com/{}", i),
                            "source": "cdp",
                            "matches": [{
                                "uid": format!("{}_0", i),
                                "role": "button",
                                "label": "Do it",
                                "tag": "button"
                            }]
                        })
                        .to_string(),
                    }],
                    is_error: None,
                },
                ToolCallResult {
                    content: vec![ToolContent::Text {
                        text: format!("ok {}", i),
                    }],
                    is_error: None,
                },
            ]
        })
        .collect();
    MockMcp::new(results, tools)
}

#[tokio::test]
async fn consecutive_destructive_cap_halts_after_three_calls() {
    use crate::agent::{PermissionAction, PermissionPolicy, PermissionRule};

    // Allow the tool so the approval prompt doesn't interfere. Three
    // destructive calls in a row must halt the run at cap=3.
    let agent_llm = MockAgent::new(vec![
        MockAgent::tool_call_response("delete_file", r#"{"id": 1}"#, "call_0"),
        MockAgent::tool_call_response("delete_file", r#"{"id": 2}"#, "call_1"),
        MockAgent::tool_call_response("delete_file", r#"{"id": 3}"#, "call_2"),
        // Should not be reached
        MockAgent::done_response("Unreachable"),
    ]);
    let mcp = build_mcp_with_destructive_tools(&["delete_file"]);

    let policy = PermissionPolicy {
        rules: vec![PermissionRule {
            tool_pattern: "delete_file".to_string(),
            args_pattern: None,
            action: PermissionAction::Allow,
        }],
        ..Default::default()
    };

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
    let (approval_tx, _) = spawn_approval_counter(true);

    let config = AgentConfig {
        max_steps: 10,
        build_workflow: false,
        use_cache: false,
        consecutive_destructive_cap: 3,
        ..Default::default()
    };
    let mut runner = AgentRunner::new(&agent_llm, config)
        .with_events(event_tx)
        .with_approval(approval_tx)
        .with_permissions(policy);
    let workflow = clickweave_core::Workflow::new("Cap test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Cleanup".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::ConsecutiveDestructiveCap { cap: 3, .. })
        ),
        "Expected ConsecutiveDestructiveCap terminal reason, got {:?}",
        state.terminal_reason,
    );
    // Exactly three successful destructive steps.
    let destructive_count = state
        .steps
        .iter()
        .filter(|s| matches!(&s.outcome, StepOutcome::Success(_)))
        .count();
    assert_eq!(destructive_count, 3);

    // Drain events and assert the cap-hit event fired with the expected
    // tool names.
    drop(runner);
    let mut found = false;
    while let Ok(event) = event_rx.try_recv() {
        if let AgentEvent::ConsecutiveDestructiveCapHit {
            recent_tool_names,
            cap,
        } = event
        {
            assert_eq!(cap, 3);
            assert_eq!(recent_tool_names, vec!["delete_file"; 3]);
            found = true;
        }
    }
    assert!(found, "ConsecutiveDestructiveCapHit event must be emitted");
}

#[tokio::test]
async fn non_destructive_tool_resets_consecutive_destructive_counter() {
    use crate::agent::{PermissionAction, PermissionPolicy, PermissionRule};

    // Sequence: destructive, destructive, NON-destructive, destructive,
    // destructive, done. Streak resets on the non-destructive call so the
    // cap (3) is never reached — the run completes normally.
    let agent_llm = MockAgent::new(vec![
        MockAgent::tool_call_response("delete_file", r#"{"id": 1}"#, "call_0"),
        MockAgent::tool_call_response("delete_file", r#"{"id": 2}"#, "call_1"),
        MockAgent::tool_call_response("benign_click", r#"{"x": 10}"#, "call_2"),
        MockAgent::tool_call_response("delete_file", r#"{"id": 3}"#, "call_3"),
        MockAgent::tool_call_response("delete_file", r#"{"id": 4}"#, "call_4"),
        MockAgent::done_response("Complete"),
    ]);

    // Build a mock with two tools: a destructive delete_file and a
    // benign non-destructive benign_click.
    let tools = vec![
        destructive_tool("delete_file"),
        tool_with_annotations(
            "benign_click",
            Some(serde_json::json!({"readOnlyHint": false, "destructiveHint": false})),
        ),
    ];
    let results: Vec<ToolCallResult> = (0..30)
        .flat_map(|i| {
            vec![
                ToolCallResult {
                    content: vec![ToolContent::Text {
                        text: serde_json::json!({
                            "page_url": format!("https://example.com/{}", i),
                            "source": "cdp",
                            "matches": [{
                                "uid": format!("{}_0", i),
                                "role": "button",
                                "label": "X",
                                "tag": "button"
                            }]
                        })
                        .to_string(),
                    }],
                    is_error: None,
                },
                ToolCallResult {
                    content: vec![ToolContent::Text {
                        text: format!("ok {}", i),
                    }],
                    is_error: None,
                },
            ]
        })
        .collect();
    let mcp = MockMcp::new(results, tools);

    let policy = PermissionPolicy {
        rules: vec![PermissionRule {
            tool_pattern: "*".to_string(),
            args_pattern: None,
            action: PermissionAction::Allow,
        }],
        ..Default::default()
    };

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(64);
    let (approval_tx, _) = spawn_approval_counter(true);

    let config = AgentConfig {
        max_steps: 10,
        build_workflow: false,
        use_cache: false,
        consecutive_destructive_cap: 3,
        ..Default::default()
    };
    let mut runner = AgentRunner::new(&agent_llm, config)
        .with_events(event_tx)
        .with_approval(approval_tx)
        .with_permissions(policy);
    let workflow = clickweave_core::Workflow::new("Cap reset test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Mix".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(
        state.completed,
        "non-destructive tool should reset the streak and let the run complete",
    );
    assert!(matches!(
        state.terminal_reason,
        Some(TerminalReason::Completed { .. })
    ));
}

#[tokio::test]
async fn consecutive_destructive_cap_of_zero_disables_feature() {
    use crate::agent::{PermissionAction, PermissionPolicy, PermissionRule};

    // Four destructive tools in a row; cap=0 must let them all through.
    let agent_llm = MockAgent::new(vec![
        MockAgent::tool_call_response("delete_file", r#"{"id": 1}"#, "call_0"),
        MockAgent::tool_call_response("delete_file", r#"{"id": 2}"#, "call_1"),
        MockAgent::tool_call_response("delete_file", r#"{"id": 3}"#, "call_2"),
        MockAgent::tool_call_response("delete_file", r#"{"id": 4}"#, "call_3"),
        MockAgent::done_response("Done"),
    ]);
    let mcp = build_mcp_with_destructive_tools(&["delete_file"]);

    let policy = PermissionPolicy {
        rules: vec![PermissionRule {
            tool_pattern: "*".to_string(),
            args_pattern: None,
            action: PermissionAction::Allow,
        }],
        ..Default::default()
    };

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(64);
    let (approval_tx, _) = spawn_approval_counter(true);

    let config = AgentConfig {
        max_steps: 10,
        build_workflow: false,
        use_cache: false,
        consecutive_destructive_cap: 0,
        ..Default::default()
    };
    let mut runner = AgentRunner::new(&agent_llm, config)
        .with_events(event_tx)
        .with_approval(approval_tx)
        .with_permissions(policy);
    let workflow = clickweave_core::Workflow::new("Cap-zero test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            "Many".to_string(),
            workflow,
            &mcp,
            None,
            mcp_tools,
            None,
            &[],
        )
        .await
        .unwrap();

    assert!(state.completed);
    assert!(matches!(
        state.terminal_reason,
        Some(TerminalReason::Completed { .. })
    ));
    // All four destructive calls ran — plus the agent_done step.
    let success_count = state
        .steps
        .iter()
        .filter(|s| matches!(&s.outcome, StepOutcome::Success(_)))
        .count();
    assert_eq!(success_count, 4, "all 4 destructive steps should run");
}

// ---------------------------------------------------------------------------
// Cross-task / cross-process coordination tests
//
// These exercise the full `AgentChannels` contract end-to-end: a harness
// task plays the role the Tauri forwarders play in production, and the
// assertions verify the engine responds to realistic sequences of events
// (stop-during-approval, cache replay through the approval gate, empty-
// elements native paths, tool-mapping misses, cross-run event draining).
// ---------------------------------------------------------------------------

mod coordination;

// ---------------------------------------------------------------------------
// CDP lifecycle parity tests
//
// These assert that the agent runner now maintains the same
// `cdp_selected_pages` bookkeeping the executor has long relied on —
// the correctness gap that motivated the `cdp_lifecycle` consolidation.
// Before the refactor, the agent observed a lost tab only on the next
// observation step, and any action dispatched in between was committed
// against the wrong tab.
// ---------------------------------------------------------------------------

mod cdp_lifecycle_parity {
    use super::*;

    /// A queue-backed MCP stub for tests that exercise `snapshot_selected_page_url`.
    /// Distinct from the module-level `MockMcp` because it records per-call
    /// arguments so parity assertions can verify the correct tool was invoked.
    struct RecordingMcp {
        results: Mutex<Vec<ToolCallResult>>,
        calls: Mutex<Vec<String>>,
    }

    impl RecordingMcp {
        fn new(results: Vec<ToolCallResult>) -> Self {
            Self {
                results: Mutex::new(results),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn took(&self) -> Vec<String> {
            std::mem::take(&mut *self.calls.lock().unwrap())
        }
    }

    impl Mcp for RecordingMcp {
        async fn call_tool(
            &self,
            name: &str,
            _arguments: Option<Value>,
        ) -> anyhow::Result<ToolCallResult> {
            self.calls.lock().unwrap().push(name.to_string());
            let mut q = self.results.lock().unwrap();
            if q.is_empty() {
                panic!("RecordingMcp: no queued response for '{}'", name);
            }
            Ok(q.remove(0))
        }

        fn has_tool(&self, _name: &str) -> bool {
            true
        }

        fn tools_as_openai(&self) -> Vec<Value> {
            Vec::new()
        }

        async fn refresh_server_tool_list(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn text_result(text: &str) -> ToolCallResult {
        ToolCallResult {
            content: vec![ToolContent::Text {
                text: text.to_string(),
            }],
            is_error: None,
        }
    }

    #[tokio::test]
    async fn agent_snapshot_remembers_selected_tab_matching_executor_behavior() {
        // Mirrors `executor::tests::cdp::snapshot_selected_page_url_remembers_current_selection`:
        // given a page list with a `*`-marked selected tab, the remembered
        // URL must land in the agent's own CdpState under the same
        // `(app_name, pid)` key shape the executor uses.
        let agent_llm = MockAgent::new(Vec::new());
        let mut runner = AgentRunner::new(&agent_llm, AgentConfig::default());
        let mcp = RecordingMcp::new(vec![text_result(
            "Pages (2 total):\n  [0] https://a.example.com/\n  [1]* https://b.example.com/foo\n",
        )]);

        runner
            .snapshot_selected_page_url_for_test("Chrome", 4242, &mcp)
            .await;

        let calls = mcp.took();
        assert_eq!(calls, vec!["cdp_list_pages".to_string()]);
        assert_eq!(
            runner
                .cdp_state()
                .selected_pages
                .get(&("Chrome".to_string(), 4242))
                .map(String::as_str),
            Some("https://b.example.com/foo"),
            "Agent CdpState must track the selected URL like the executor does",
        );
    }

    #[tokio::test]
    async fn agent_snapshot_is_silent_on_list_pages_error() {
        // Mirrors `executor::tests::cdp::snapshot_selected_page_url_is_silent_on_error`:
        // a failed `cdp_list_pages` must neither panic nor mutate state.
        let agent_llm = MockAgent::new(Vec::new());
        let mut runner = AgentRunner::new(&agent_llm, AgentConfig::default());
        let mcp = RecordingMcp::new(vec![ToolCallResult {
            content: vec![ToolContent::Text {
                text: "boom".to_string(),
            }],
            is_error: Some(true),
        }]);

        runner
            .snapshot_selected_page_url_for_test("Chrome", 4242, &mcp)
            .await;

        assert!(
            runner.cdp_state().selected_pages.is_empty(),
            "State must remain untouched when cdp_list_pages errors",
        );
    }

    #[tokio::test]
    async fn agent_cdp_state_upgrade_pid_migrates_placeholder_entry() {
        // The agent initially records pages against pid=0 because PID
        // resolution isn't reliable inline in the observe-act loop. When
        // the real PID later becomes known, the shared `CdpState`
        // upgrade path must migrate both the connection identity and the
        // remembered URL — the same behavior that keeps the executor's
        // focus_refresh test passing.
        let agent_llm = MockAgent::new(Vec::new());
        let mut runner = AgentRunner::new(&agent_llm, AgentConfig::default());
        let mcp = RecordingMcp::new(vec![text_result(
            "Pages (1 total):\n  [0]* https://example.com/\n",
        )]);

        runner
            .snapshot_selected_page_url_for_test("Chrome", 0, &mcp)
            .await;

        // Before upgrade: placeholder entry under pid=0.
        assert_eq!(
            runner
                .cdp_state()
                .selected_pages
                .get(&("Chrome".to_string(), 0))
                .map(String::as_str),
            Some("https://example.com/"),
        );

        // After upgrade: entry keyed by the real PID.
        // We reach into the runner's state through the test-only accessor.
        let _ = runner; // rebind as mutable below.
        let mut runner = AgentRunner::new(&agent_llm, AgentConfig::default());
        let mcp = RecordingMcp::new(vec![text_result(
            "Pages (1 total):\n  [0]* https://example.com/\n",
        )]);
        runner
            .snapshot_selected_page_url_for_test("Chrome", 0, &mcp)
            .await;

        // Simulate the runner learning the real PID and upgrading.
        // We call the same state method the executor calls via
        // `refresh_focused_pid`.
        // SAFETY: `cdp_state_mut` isn't exposed on the agent runner;
        // use the fact that `snapshot_selected_page_url_for_test` returns
        // via shared state we just inspected, plus the fact that
        // `CdpState::upgrade_pid` is a pure method we can call through
        // the struct's `pub(crate)` accessors below.
        // Reach in via the `cdp_state()` immutable accessor for inspection;
        // to mutate we go through a fresh helper that routes through the
        // real call path.
        // Use a minimal test-only code path: record, then invoke
        // `record_selected_page` under the upgraded PID and drop the old
        // key, which is exactly what `upgrade_pid` does.
        // Since the runner owns its CdpState privately, we exercise
        // upgrade_pid on a standalone instance below to close the loop.

        use crate::cdp_lifecycle::CdpState;
        let mut standalone = CdpState::new();
        standalone.connected_app = Some(("Chrome".to_string(), 0));
        standalone
            .selected_pages
            .insert(("Chrome".to_string(), 0), "https://example.com/".into());

        standalone.upgrade_pid("Chrome", 5150);

        assert_eq!(standalone.connected_app, Some(("Chrome".to_string(), 5150)));
        assert_eq!(
            standalone
                .selected_pages
                .get(&("Chrome".to_string(), 5150))
                .map(String::as_str),
            Some("https://example.com/"),
            "Agent-side upgrade_pid must migrate the remembered URL to the real PID, \
             mirroring executor::tests::focus_refresh parity.",
        );
        assert!(
            !standalone
                .selected_pages
                .contains_key(&("Chrome".to_string(), 0)),
            "Placeholder entry must be removed after upgrade",
        );
    }

    #[tokio::test]
    async fn agent_mark_app_quit_clears_state_like_executor() {
        // Mirrors the executor's `quit_app` handling in
        // `ai_step.rs` / `deterministic/mod.rs`: after a quit,
        // both the active connection and every remembered tab URL
        // for that app name must be gone.
        use crate::cdp_lifecycle::CdpState;
        let mut state = CdpState::new();
        state.connected_app = Some(("Slack".to_string(), 4242));
        state
            .selected_pages
            .insert(("Slack".to_string(), 4242), "slack-url".into());
        state
            .selected_pages
            .insert(("Safari".to_string(), 7), "safari-url".into());

        state.mark_app_quit("Slack");

        assert!(state.connected_app.is_none());
        assert!(
            !state.selected_pages.keys().any(|(name, _)| name == "Slack"),
            "Quit must drop every Slack entry regardless of PID",
        );
        assert_eq!(
            state
                .selected_pages
                .get(&("Safari".to_string(), 7))
                .map(String::as_str),
            Some("safari-url"),
            "Other apps must survive untouched",
        );
    }
}
