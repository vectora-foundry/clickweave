use super::*;

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
        ..Default::default()
    };

    let runner = StateRunner::new("Click the submit button".to_string(), config);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Click the submit button".to_string(),
            workflow,
            mcp_tools,
            None,
        )
        .await
        .unwrap();

    assert!(state.completed);
    // StateRunner records one `AgentStep` per dispatched tool call. The
    // terminal `agent_done` turn is surfaced through
    // `TerminalReason::Completed` + `state.completed + state.summary`,
    // not as a separate `AgentStep` (legacy AgentRunner recorded a
    // `Done` step too; the spine-era surface drops that redundancy per
    // the Task 3a.7 mapping table).
    assert_eq!(state.steps.len(), 1);
    assert!(state.summary.is_some());
    assert!(state.summary.as_ref().unwrap().contains("submit button"));

    // Verify workflow was built with at least one node (from the click)
    assert!(
        !state.trace_graph.nodes.is_empty(),
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
        ..Default::default()
    };

    let runner = StateRunner::new("Click forever".to_string(), config);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Click forever".to_string(),
            workflow,
            mcp_tools,
            None,
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
        ..Default::default()
    };

    let runner = StateRunner::new("Do something".to_string(), config);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Do something".to_string(),
            workflow,
            mcp_tools,
            None,
        )
        .await
        .unwrap();

    assert!(state.completed);
    // Legacy AgentRunner recorded a `TextOnly` step for the first turn
    // and a `Done` step for the second. StateRunner's `parse_agent_turn`
    // treats text-only responses as a forgiveness-Replan (re-observe
    // next turn) and the terminal `agent_done` is surfaced through
    // `TerminalReason::Completed`; neither turn lands in
    // `AgentState.steps`. The "nearest StateRunner equivalent" for the
    // original assertion is therefore `state.completed == true` with an
    // empty steps vector.
    assert_eq!(state.steps.len(), 0);
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
        ..Default::default()
    };

    let runner = StateRunner::new("Click a missing button".to_string(), config);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Click a missing button".to_string(),
            workflow,
            mcp_tools,
            None,
        )
        .await
        .unwrap();

    assert!(state.completed);
    // Legacy AgentRunner recorded a `Replan` step for `agent_replan`
    // and a `Done` step for `agent_done`. StateRunner drops both from
    // `AgentState.steps` — `agent_replan` re-observes next turn
    // without a step, and `agent_done` is surfaced through
    // `TerminalReason::Completed`. The "nearest StateRunner
    // equivalent" is an empty `steps` vec with `state.completed`.
    assert_eq!(state.steps.len(), 0);
}

#[tokio::test]
async fn agent_state_reports_completed_reason_on_done() {
    let agent_llm = MockAgent::new(vec![MockAgent::done_response("All done")]);

    let mcp = MockMcp::with_click_tool();
    let config = AgentConfig {
        max_steps: 5,
        build_workflow: false,
        ..Default::default()
    };

    let runner = StateRunner::new("Do it".to_string(), config);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Do it".to_string(),
            workflow,
            mcp_tools,
            None,
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
        ..Default::default()
    };

    let runner = StateRunner::new("Click forever".to_string(), config);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Click forever".to_string(),
            workflow,
            mcp_tools,
            None,
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
        consecutive_destructive_cap: 0,
        allow_focus_window: true,
        ..AgentConfig::default()
    };

    let runner = StateRunner::new("Click it".to_string(), config);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Click it".to_string(),
            workflow,
            mcp_tools,
            None,
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
        consecutive_destructive_cap: 0,
        allow_focus_window: true,
        ..AgentConfig::default()
    };

    let runner = StateRunner::new("Click it".to_string(), config);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Click it".to_string(),
            workflow,
            mcp_tools,
            None,
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

    let runner = StateRunner::new("Click it".to_string(), config)
        .with_events(event_tx)
        .with_approval(approval_tx);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Click it".to_string(),
            workflow,
            mcp_tools,
            None,
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

    let runner = StateRunner::new("Click it".to_string(), config)
        .with_events(event_tx)
        .with_approval(approval_tx);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Click it".to_string(),
            workflow,
            mcp_tools,
            None,
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
        ..Default::default()
    };

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(64);
    // Create an approval channel and immediately drop the receiver.
    // This means the sender's `send()` will fail.
    let (approval_tx, _dropped_rx) = tokio::sync::mpsc::channel(1);
    drop(_dropped_rx);

    let runner = StateRunner::new("Click it".to_string(), config)
        .with_events(event_tx)
        .with_approval(approval_tx);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Click it".to_string(),
            workflow,
            mcp_tools,
            None,
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
