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
/// updates when `refresh_tools` is called. The server's "true" tool set
/// grows after `cdp_connect` (the extras become available), but the mock
/// will keep returning the stale snapshot until refreshed — matching what
/// the production client does.
struct ShiftingToolsMcp {
    results: Mutex<Vec<ToolCallResult>>,
    base_tools: Vec<Value>,
    extra_tools: Vec<Value>,
    /// Server-side visibility: flips to true on `cdp_connect`.
    cdp_connected: std::sync::atomic::AtomicBool,
    /// Client-side cached snapshot of tools; only updated by `refresh_tools`.
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

    async fn refresh_tools(&self) -> anyhow::Result<()> {
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
    //   (refresh_tools reloads the client cache after connect)
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

    async fn refresh_tools(&self) -> anyhow::Result<()> {
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
        .run("Open settings".to_string(), workflow, &mcp, None, mcp_tools)
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
        .run("Open settings".to_string(), workflow, &mcp, None, mcp_tools)
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
        .run("Do it".to_string(), workflow, &mcp, None, mcp_tools)
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
        .run("Do it".to_string(), workflow, &mcp, None, mcp_tools)
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
// Cross-task / cross-process coordination tests
//
// These exercise the full `AgentChannels` contract end-to-end: a harness
// task plays the role the Tauri forwarders play in production, and the
// assertions verify the engine responds to realistic sequences of events
// (stop-during-approval, cache replay through the approval gate, empty-
// elements native paths, tool-mapping misses, cross-run event draining).
// ---------------------------------------------------------------------------

mod coordination;
