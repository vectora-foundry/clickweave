use super::*;

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
        ..Default::default()
    };
    let runner = StateRunner::new("Click".to_string(), config)
        .with_events(event_tx)
        .with_approval(approval_tx)
        .with_permissions(policy);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Click".to_string(),
            workflow,
            mcp_tools,
            None,
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
        ..Default::default()
    };
    let runner = StateRunner::new("Click".to_string(), config)
        .with_events(event_tx)
        .with_approval(approval_tx)
        .with_permissions(policy);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Click".to_string(),
            workflow,
            mcp_tools,
            None,
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
        ..Default::default()
    };
    let runner = StateRunner::new("Delete".to_string(), config)
        .with_events(event_tx)
        .with_approval(approval_tx)
        .with_permissions(policy);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let _state = runner
        .run(
            &agent_llm,
            &mcp,
            "Delete".to_string(),
            workflow,
            mcp_tools,
            None,
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
        consecutive_destructive_cap: 3,
        ..Default::default()
    };
    let runner = StateRunner::new("Cleanup".to_string(), config)
        .with_events(event_tx)
        .with_approval(approval_tx)
        .with_permissions(policy);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Cleanup".to_string(),
            workflow,
            mcp_tools,
            None,
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
    // tool names. `runner` was consumed by `.run(...)`, which dropped the
    // event channel sender on completion — the drain loop terminates when
    // `try_recv` reports Empty.
    let mut found = false;
    while let Ok(event) = event_rx.try_recv() {
        let Some(event) = event.into_event() else {
            continue;
        };
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
        consecutive_destructive_cap: 3,
        ..Default::default()
    };
    let runner = StateRunner::new("Mix".to_string(), config)
        .with_events(event_tx)
        .with_approval(approval_tx)
        .with_permissions(policy);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Mix".to_string(),
            workflow,
            mcp_tools,
            None,
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
        consecutive_destructive_cap: 0,
        ..Default::default()
    };
    let runner = StateRunner::new("Many".to_string(), config)
        .with_events(event_tx)
        .with_approval(approval_tx)
        .with_permissions(policy);
    let workflow = crate::agent::trace_graph::AgentTraceGraph::new();
    let mcp_tools = mcp.tools_as_openai();

    let state = runner
        .run(
            &agent_llm,
            &mcp,
            "Many".to_string(),
            workflow,
            mcp_tools,
            None,
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
