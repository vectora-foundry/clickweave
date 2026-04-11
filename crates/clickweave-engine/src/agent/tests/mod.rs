use crate::agent::loop_runner::AgentRunner;
use crate::agent::types::*;
use crate::executor::Mcp;
use anyhow::Result;
use clickweave_llm::{
    ChatBackend, ChatResponse, Choice, FunctionCall, Message, ModelInfo, ToolCall,
};
use clickweave_mcp::{ToolCallResult, ToolContent};
use serde_json::Value;
use std::sync::Mutex;

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

    async fn chat(
        &self,
        _messages: Vec<Message>,
        _tools: Option<Vec<Value>>,
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
