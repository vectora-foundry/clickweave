//! Integration test: two sequential agent runs linked by anchor_node_id.
//!
//! Asserts:
//! - Run 1's nodes all carry Run 1's `source_run_id`.
//! - Run 2's nodes all carry Run 2's `source_run_id`.
//! - Run 2's first emitted edge connects from the anchor node id to
//!   Run 2's first new node.

use super::{MockAgent, MockMcp};
use crate::agent::StateRunner;
use crate::agent::types::AgentConfig;
use crate::executor::Mcp;
use crate::agent::trace_graph::AgentTraceGraph;
use uuid::Uuid;

#[tokio::test]
async fn sequential_runs_chain_via_anchor() {
    // Run 1: click then agent_done. Workflow-building on so we get a node.
    let llm1 = MockAgent::new(vec![
        MockAgent::tool_call_response("click", r#"{"x": 100, "y": 200}"#, "call_0"),
        MockAgent::done_response("clicked submit"),
    ]);
    let mcp1 = MockMcp::with_click_tool();

    let run_id_1 = Uuid::new_v4();
    let config = AgentConfig {
        build_workflow: true,
        ..Default::default()
    };

    let runner1 = StateRunner::new("send test".to_string(), config.clone()).with_run_id(run_id_1);
    let mcp_tools = mcp1.tools_as_openai();
    let state1 = runner1
        .run(
            &llm1,
            &mcp1,
            "send test".to_string(),
            AgentTraceGraph::new(),
            mcp_tools.clone(),
            None,
        )
        .await
        .expect("run 1 succeeds");

    assert!(!state1.trace_graph.nodes.is_empty(), "run 1 built no nodes");
    for node in &state1.trace_graph.nodes {
        assert_eq!(
            node.source_run_id,
            Some(run_id_1),
            "run 1 nodes must all carry run_id_1"
        );
    }
    let last_id_1 = state1.trace_graph.nodes.last().unwrap().id;

    // Run 2: anchor seeded to run 1's last node; one click + done.
    let llm2 = MockAgent::new(vec![
        MockAgent::tool_call_response("click", r#"{"x": 300, "y": 400}"#, "call_r2_0"),
        MockAgent::done_response("clicked reply"),
    ]);
    let mcp2 = MockMcp::with_click_tool();

    let run_id_2 = Uuid::new_v4();
    let prior = vec![crate::agent::PriorTurn {
        goal: "send test".to_string(),
        summary: "clicked submit".to_string(),
        run_id: run_id_1,
    }];

    let runner2 = StateRunner::new("wait for reply".to_string(), config).with_run_id(run_id_2);
    // D18: caller composes prior-turn log + variant context into the
    // goal string. Tests exercise the same seam.
    let goal2 = crate::agent::build_goal_block("wait for reply", &prior, None, 1000);
    let state2 = runner2
        .run(
            &llm2,
            &mcp2,
            goal2,
            AgentTraceGraph::new(),
            mcp2.tools_as_openai(),
            Some(last_id_1),
        )
        .await
        .expect("run 2 succeeds");

    assert!(!state2.trace_graph.nodes.is_empty(), "run 2 built no nodes");
    for node in &state2.trace_graph.nodes {
        assert_eq!(
            node.source_run_id,
            Some(run_id_2),
            "run 2 nodes must all carry run_id_2"
        );
    }

    // Run 2's first new node must be connected via an edge from the
    // anchor node (which is not in state2.trace_graph.nodes — the current
    // engine builds a fresh Workflow per run). The anchor shows up
    // solely as the `from` endpoint of the first edge.
    let first_new = state2.trace_graph.nodes.first().expect("run 2 has nodes");
    assert!(
        state2
            .trace_graph
            .edges
            .iter()
            .any(|e| e.from == last_id_1 && e.to == first_new.id),
        "run 2's first node must connect to the anchor node"
    );
}
