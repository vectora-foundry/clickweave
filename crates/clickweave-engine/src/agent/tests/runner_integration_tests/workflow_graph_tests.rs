use super::super::super::test_stubs::{ScriptedLlm, StaticMcp, llm_reply_tool};
use crate::agent::runner::StateRunner;
use crate::agent::trace_graph::AgentTraceGraph;
use crate::agent::types::{AgentConfig, AgentEvent, RunnerOutput, TerminalReason};
use crate::executor::Mcp;
use tokio::sync::mpsc;

fn cfg_with_steps(steps: usize) -> AgentConfig {
    AgentConfig {
        max_steps: steps,
        ..AgentConfig::default()
    }
}

/// MCP fixture: advertises `cdp_find_elements` + `cdp_click`; the
/// `cdp_find_elements` reply contains exactly one element so the
/// fingerprint is stable across runs.
fn build_mcp_with_one_element() -> StaticMcp {
    let body = r#"{"page_url":"about:blank","source":"cdp","matches":[{"uid":"1_0","role":"button","label":"Submit","tag":"button","disabled":false,"parent_role":null,"parent_name":null}]}"#;
    StaticMcp::with_tools(&["cdp_find_elements", "cdp_click"])
        .with_reply("cdp_find_elements", body)
        .with_reply("cdp_click", "clicked")
}

/// Drain `event_rx` of every already-buffered event. Non-blocking.
fn drain_events(rx: &mut mpsc::Receiver<RunnerOutput>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let Some(event) = ev.into_event() {
            out.push(event);
        }
    }
    out
}

/// A successful live-path tool call emits `AgentEvent::NodeAdded` with the
/// runner's `run_id` stamped as `source_run_id`, and the workflow gains a
/// single node with no prior edge (the anchor slot is empty).
#[tokio::test]
async fn successful_tool_call_emits_node_added_event_with_source_run_id() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let mcp = build_mcp_with_one_element();
    let tools = mcp.tools_as_openai();

    let run_id = uuid::Uuid::new_v4();
    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(16);
    let runner = StateRunner::new("goal".to_string(), cfg_with_steps(5))
        .with_run_id(run_id)
        .with_events(event_tx);

    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools,
            None,
        )
        .await
        .expect("run ok");

    let events = drain_events(&mut event_rx);
    let node_events: Vec<_> = events
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::NodeAdded { node } => Some(node.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(node_events.len(), 1, "one live tool call → one NodeAdded");
    assert_eq!(
        node_events[0].source_run_id,
        Some(run_id),
        "every emitted node must carry the runner's run_id as source_run_id"
    );
    // No EdgeAdded — anchor_node_id is None and this is the first node.
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, AgentEvent::EdgeAdded { .. })),
        "first node without an anchor must not emit an EdgeAdded"
    );
    assert_eq!(state.trace_graph.nodes.len(), 1);
    assert!(state.trace_graph.edges.is_empty());
}

/// Two successful tool calls emit an `EdgeAdded` that connects the first
/// node to the second, and the workflow's edge vec is populated.
#[tokio::test]
async fn second_tool_call_emits_edge_added_connecting_to_first_node() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "2_0"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let mcp = build_mcp_with_one_element();
    let tools = mcp.tools_as_openai();

    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(32);
    let runner = StateRunner::new("goal".to_string(), cfg_with_steps(5)).with_events(event_tx);

    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools,
            None,
        )
        .await
        .expect("run ok");

    let events = drain_events(&mut event_rx);
    let nodes: Vec<_> = events
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::NodeAdded { node } => Some(node.as_ref().clone()),
            _ => None,
        })
        .collect();
    let edges: Vec<_> = events
        .iter()
        .filter_map(|ev| match ev {
            AgentEvent::EdgeAdded { edge } => Some(edge.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(nodes.len(), 2, "two live tool calls → two NodeAdded");
    assert_eq!(edges.len(), 1, "two nodes, no anchor → one EdgeAdded");
    assert_eq!(edges[0].from, nodes[0].id);
    assert_eq!(edges[0].to, nodes[1].id);
    assert_eq!(state.trace_graph.nodes.len(), 2);
    assert_eq!(state.trace_graph.edges.len(), 1);
}

/// Observation-only tools (here `cdp_find_elements`) execute but must not
/// produce a workflow node or emit `NodeAdded`.
#[tokio::test]
async fn observation_tool_does_not_emit_node() {
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool(
            "cdp_find_elements",
            serde_json::json!({"query": "", "max_results": 300}),
        ),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let mcp = build_mcp_with_one_element();
    let tools = mcp.tools_as_openai();

    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(16);
    let runner = StateRunner::new("goal".to_string(), cfg_with_steps(5)).with_events(event_tx);

    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools,
            None,
        )
        .await
        .expect("run ok");

    let events = drain_events(&mut event_rx);
    let node_count = events
        .iter()
        .filter(|ev| matches!(ev, AgentEvent::NodeAdded { .. }))
        .count();
    assert_eq!(
        node_count, 0,
        "observation tools must not produce workflow nodes"
    );
    assert!(state.trace_graph.nodes.is_empty());
    assert!(state.trace_graph.edges.is_empty());
}

/// A caller-provided `anchor_node_id` seeds `state.last_node_id`, so the
/// first live node chains from the anchor via `EdgeAdded`.
#[tokio::test]
async fn anchor_node_id_chains_first_new_node() {
    let anchor = uuid::Uuid::new_v4();
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let mcp = build_mcp_with_one_element();
    let tools = mcp.tools_as_openai();

    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(16);
    let runner = StateRunner::new("goal".to_string(), cfg_with_steps(5)).with_events(event_tx);

    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools,
            Some(anchor),
        )
        .await
        .expect("run ok");

    let events = drain_events(&mut event_rx);
    let first_node = events.iter().find_map(|ev| match ev {
        AgentEvent::NodeAdded { node } => Some(node.as_ref().clone()),
        _ => None,
    });
    let first_edge = events.iter().find_map(|ev| match ev {
        AgentEvent::EdgeAdded { edge } => Some(edge.clone()),
        _ => None,
    });
    let node = first_node.expect("one live node");
    let edge = first_edge.expect("anchor must produce a first edge");
    assert_eq!(edge.from, anchor, "first edge must chain from the anchor");
    assert_eq!(edge.to, node.id);
    assert_eq!(state.trace_graph.edges.len(), 1);
}

/// `build_workflow = false` opts out of workflow-graph emission even on a
/// successful tool call. No nodes, no edges, no events.
#[tokio::test]
async fn build_workflow_false_suppresses_node_emission() {
    let mut cfg = cfg_with_steps(5);
    cfg.build_workflow = false;
    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "ok"})),
    ]);
    let mcp = build_mcp_with_one_element();
    let tools = mcp.tools_as_openai();

    let (event_tx, mut event_rx) = mpsc::channel::<RunnerOutput>(16);
    let runner = StateRunner::new("goal".to_string(), cfg).with_events(event_tx);

    let state = runner
        .run(
            &llm,
            &mcp,
            "goal".to_string(),
            AgentTraceGraph::new(),
            tools,
            None,
        )
        .await
        .expect("run ok");

    let events = drain_events(&mut event_rx);
    assert!(
        !events
            .iter()
            .any(|ev| matches!(ev, AgentEvent::NodeAdded { .. })),
        "build_workflow=false must suppress NodeAdded"
    );
    assert!(state.trace_graph.nodes.is_empty());
    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::Completed { .. })
        ),
        "run still completes normally, {:?}",
        state.terminal_reason,
    );
}
