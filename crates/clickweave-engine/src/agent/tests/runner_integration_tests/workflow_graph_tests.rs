use super::super::super::test_stubs::{ScriptedLlm, StaticMcp, llm_reply_tool};
use crate::agent::runner::StateRunner;
use crate::agent::trace_graph::AgentTraceGraph;
use crate::agent::types::{AgentConfig, RunnerOutput, TerminalReason};
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
fn drain_events(rx: &mut mpsc::Receiver<RunnerOutput>) -> Vec<RunnerOutput> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

/// A successful live-path tool call accumulates one node in `state.trace_graph`
/// stamped with the runner's `run_id` as `source_run_id`. No edges are
/// produced when the anchor slot is empty and this is the first node.
#[tokio::test]
async fn successful_tool_call_adds_node_to_trace_graph_with_source_run_id() {
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

    let _events = drain_events(&mut event_rx);
    assert_eq!(
        state.trace_graph.nodes.len(),
        1,
        "one live tool call → one trace node"
    );
    assert_eq!(
        state.trace_graph.nodes[0].source_run_id,
        Some(run_id),
        "every trace node must carry the runner's run_id as source_run_id"
    );
    // No edge — anchor_node_id is None and this is the first node.
    assert!(
        state.trace_graph.edges.is_empty(),
        "first node without an anchor must not produce an edge"
    );
}

/// Two successful tool calls produce two trace nodes and one edge connecting
/// the first to the second.
#[tokio::test]
async fn second_tool_call_adds_edge_in_trace_graph_connecting_to_first_node() {
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

    let _events = drain_events(&mut event_rx);
    assert_eq!(
        state.trace_graph.nodes.len(),
        2,
        "two live tool calls → two trace nodes"
    );
    assert_eq!(
        state.trace_graph.edges.len(),
        1,
        "two nodes, no anchor → one edge"
    );
    assert_eq!(
        state.trace_graph.edges[0].from,
        state.trace_graph.nodes[0].id
    );
    assert_eq!(state.trace_graph.edges[0].to, state.trace_graph.nodes[1].id);
}

/// Observation-only tools (here `cdp_find_elements`) execute but must not
/// produce a trace graph node.
#[tokio::test]
async fn observation_tool_does_not_add_node_to_trace_graph() {
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

    let _events = drain_events(&mut event_rx);
    assert!(
        state.trace_graph.nodes.is_empty(),
        "observation tools must not produce trace nodes"
    );
    assert!(state.trace_graph.edges.is_empty());
}

/// A caller-provided `anchor_node_id` seeds `state.last_node_id`, so the
/// first live node chains from the anchor via an edge in the trace graph.
#[tokio::test]
async fn anchor_node_id_chains_first_new_node_in_trace_graph() {
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

    let _events = drain_events(&mut event_rx);
    assert_eq!(state.trace_graph.nodes.len(), 1);
    assert_eq!(state.trace_graph.edges.len(), 1);
    assert_eq!(
        state.trace_graph.edges[0].from, anchor,
        "first edge must chain from the anchor"
    );
    assert_eq!(state.trace_graph.edges[0].to, state.trace_graph.nodes[0].id);
}

/// `build_workflow = false` opts out of trace-graph accumulation even on a
/// successful tool call. No nodes, no edges.
#[tokio::test]
async fn build_workflow_false_suppresses_trace_graph_accumulation() {
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

    let _events = drain_events(&mut event_rx);
    assert!(
        state.trace_graph.nodes.is_empty(),
        "build_workflow=false must suppress trace node accumulation"
    );
    assert!(
        matches!(
            state.terminal_reason,
            Some(TerminalReason::Completed { .. })
        ),
        "run still completes normally, {:?}",
        state.terminal_reason,
    );
}
