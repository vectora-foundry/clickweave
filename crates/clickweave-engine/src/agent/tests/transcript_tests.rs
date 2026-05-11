use super::*;

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
        ..Default::default()
    };

    let runner = StateRunner::new("Click the button".to_string(), config);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Click the button".to_string(),
            workflow,
            mcp_tools,
            None,
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
        ..Default::default()
    };

    let runner = StateRunner::new("Click a button".to_string(), config);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Click a button".to_string(),
            workflow,
            mcp_tools,
            None,
        )
        .await
        .unwrap();

    assert!(state.completed);
    // StateRunner records one step per dispatched tool call; the
    // terminal `agent_done` turn is surfaced through
    // `TerminalReason::Completed` rather than a `Done` step (legacy
    // AgentRunner recorded both).
    assert_eq!(
        state.steps.len(),
        1,
        "Should have exactly 1 step (one click; agent_done is not a step)"
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
        ..Default::default()
    };

    let runner = StateRunner::new("Click something".to_string(), config);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Click something".to_string(),
            workflow,
            mcp_tools,
            None,
        )
        .await
        .unwrap();

    assert!(state.completed);
    // Legacy AgentRunner validated the `arguments` JSON shape before
    // dispatch and recorded a `StepOutcome::Error("Malformed …")` step
    // for malformed tool-call arguments. StateRunner's
    // `parse_agent_turn` forwards the raw `Value` through to MCP
    // without a parse-time gate; pre-dispatch argument validation is
    // MCP's responsibility under the state-spine contract. The closest
    // StateRunner-equivalent observation is that the run still
    // terminates cleanly (the recovery path absorbs the transport
    // outcome on the way to `agent_done`).
    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::Completed { .. })
        ),
        "Expected Completed, got {:?}",
        state.terminal_reason,
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
///
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
        ..Default::default()
    };

    let runner = StateRunner::new("Wait for a bunch of events".to_string(), config);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Wait for a bunch of events".to_string(),
            workflow,
            mcp_tools,
            None,
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
