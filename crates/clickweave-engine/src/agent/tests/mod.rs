use crate::agent::loop_runner::AgentRunner;
use crate::agent::types::*;
use crate::executor::Mcp;
use anyhow::Result;
use clickweave_llm::{
    ChatBackend, ChatOptions, ChatResponse, Choice, FunctionCall, Message, ModelInfo, ToolCall,
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
        ChatResponse {
            id: "mock-resp".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message::assistant_tool_calls(vec![ToolCall {
                    id: tool_call_id.to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: tool_name.to_string(),
                        arguments: arguments.to_string(),
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
            &serde_json::json!({"summary": summary}).to_string(),
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

    async fn refresh_tools(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
        .run("Click forever".to_string(), workflow, &mcp, None, mcp_tools)
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
        .run("Do something".to_string(), workflow, &mcp, None, mcp_tools)
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
        .run("Do it".to_string(), workflow, &mcp, None, mcp_tools)
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
        .run("Click forever".to_string(), workflow, &mcp, None, mcp_tools)
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
    // LLM always chooses click, but MCP always returns errors
    let responses: Vec<ChatResponse> = (0..10)
        .map(|i| {
            MockAgent::tool_call_response("click", r#"{"x": 10, "y": 20}"#, &format!("call_{}", i))
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
    };

    let mut runner = AgentRunner::new(&agent_llm, config);
    let workflow = clickweave_core::Workflow::new("Test");
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run("Click it".to_string(), workflow, &mcp, None, mcp_tools)
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
        .run("Click it".to_string(), workflow, &mcp, None, mcp_tools)
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
        .run("Click it".to_string(), workflow, &mcp, None, mcp_tools)
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
        .run("Click it".to_string(), workflow, &mcp, None, mcp_tools)
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
        m.role == "assistant"
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
        m.role == "tool"
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
                        call_type: "function".to_string(),
                        function: FunctionCall {
                            name: "click".to_string(),
                            arguments: r#"{"x": 10, "y": 20}"#.to_string(),
                        },
                    },
                    ToolCall {
                        id: "call_second".to_string(),
                        call_type: "function".to_string(),
                        function: FunctionCall {
                            name: "click".to_string(),
                            arguments: r#"{"x": 300, "y": 400}"#.to_string(),
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
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "click".to_string(),
                        arguments: "not valid json{{{".to_string(),
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
            m.role == "tool"
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
// Cross-task / cross-process coordination tests
//
// These exercise the full `AgentChannels` contract end-to-end: a harness
// task plays the role the Tauri forwarders play in production, and the
// assertions verify the engine responds to realistic sequences of events
// (stop-during-approval, cache replay through the approval gate, empty-
// elements native paths, tool-mapping misses, cross-run event draining).
// ---------------------------------------------------------------------------

mod coordination;
