//! Spec 2 end-to-end test: drive `StateRunner::run` with the existing
//! Spec 1 test harness (`ScriptedLlm`, `StaticMcp`, `CapturingLlm`) and
//! verify that:
//!   1. A run that recovers from a tool failure persists exactly one
//!      episode in the workflow-local SQLite store (Task 3.5).
//!   2. A subsequent run with the same goal + state surfaces the
//!      retrieved recovery in the LLM's first user-turn message.
//!   3. Setting `AgentConfig::episodic_enabled = false` disables both
//!      writes and retrievals (kill switch — Task 3.6).
//!   4. Setting `EpisodicContext::disabled()` disables both even when
//!      the config flag is on (trace-gate — Task 3.6).
//!
//! These tests intentionally co-locate inside the engine crate so the
//! `pub(crate)` `Mcp` trait, the `pub mod test_stubs` doubles, and the
//! crate-private `StateRunner` builder seam are all reachable without
//! threading a `test-stubs` feature through cargo invocation.

use std::path::PathBuf;

use tempfile::TempDir;
use tokio::sync::mpsc;

use crate::agent::episodic::{EpisodeScope, EpisodicContext, SqliteEpisodicStore};
use crate::agent::runner::StateRunner;
use crate::agent::test_stubs::{
    CapturingLlm, ScriptedLlm, StaticMcp, llm_reply_tool, llm_reply_tool_with_id,
};
use crate::agent::types::{AgentConfig, AgentEvent};
use crate::executor::Mcp;
use clickweave_core::Workflow;

/// Build an MCP that advertises one CDP element so the world-model
/// signature is non-trivial across both runs in the test, plus the
/// `cdp_click` and `ax_click` tools the recovery script uses.
fn build_mcp_with_recovery_tools() -> StaticMcp {
    let body = r#"{"page_url":"about:blank","source":"cdp","matches":[{"uid":"1_0","role":"button","label":"Submit","tag":"button","disabled":false,"parent_role":null,"parent_name":null}]}"#;
    StaticMcp::with_tools(&["cdp_find_elements", "cdp_click", "ax_click"])
        .with_reply("cdp_find_elements", body)
        .with_error("cdp_click", "element not found")
        .with_reply("ax_click", "clicked")
}

/// Episodic-aware context pointing at a fresh tempdir SQLite path.
fn enabled_ctx(workflow_local: PathBuf) -> EpisodicContext {
    EpisodicContext {
        enabled: true,
        workflow_local_path: workflow_local,
        global_path: None,
        workflow_hash: "test-workflow-uuid".into(),
    }
}

/// Build a config with episodic on (the default) and a tight max-steps
/// budget. We rely on the scripted LLM falling back to `agent_done`
/// when its queue drains, so even if a turn takes an unexpected path
/// the run still terminates within the cap.
fn config_with_steps(max_steps: usize) -> AgentConfig {
    AgentConfig {
        max_steps,
        ..AgentConfig::default()
    }
}

fn config_with_episodic_disabled(max_steps: usize) -> AgentConfig {
    AgentConfig {
        max_steps,
        episodic_enabled: false,
        ..AgentConfig::default()
    }
}

/// Drain everything currently in the `event_rx` channel so the
/// `EpisodicWriter`'s consumer task can produce its emissions before
/// we observe the SQLite store.
async fn drain_events(rx: &mut mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Ok(Some(ev)) =
        tokio::time::timeout(std::time::Duration::from_millis(20), rx.recv()).await
    {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn recovery_written_and_retrieved_across_runs() {
    let dir = TempDir::new().unwrap();
    let wl_path = dir.path().join("episodic.sqlite");

    // -----------------------------
    // Run 1: failure -> recovery -> done. Should persist one episode.
    // -----------------------------
    let ctx_run1 = enabled_ctx(wl_path.clone());
    let cfg_run1 = config_with_steps(8);

    // Scripted turns:
    //   turn 0: cdp_click  → MCP returns is_error=true (first failure).
    //   turn 1: ax_click   → MCP returns "clicked" success — clears
    //                        consecutive_errors, fires the
    //                        Recovering -> Executing transition that
    //                        the runner persists as a RecoverySucceeded
    //                        boundary AND queues for the episodic write.
    //   turn 2: agent_done.
    let llm_run1 = ScriptedLlm::new(vec![
        llm_reply_tool_with_id("cdp_click", serde_json::json!({"uid": "1_0"}), "tc-1"),
        llm_reply_tool_with_id("ax_click", serde_json::json!({"uid": "a1"}), "tc-2"),
        llm_reply_tool_with_id(
            "agent_done",
            serde_json::json!({"summary": "recovered"}),
            "tc-3",
        ),
    ]);
    let mcp_run1 = build_mcp_with_recovery_tools();
    let tools_run1 = mcp_run1.tools_as_openai();

    let (event_tx_run1, mut event_rx_run1) = mpsc::channel::<AgentEvent>(64);
    let runner_run1 = StateRunner::new_with_episodic("login".to_string(), cfg_run1, ctx_run1)
        .with_run_id(uuid::Uuid::new_v4())
        .with_events(event_tx_run1)
        .with_episodic_writer();

    let _state = runner_run1
        .run(
            &llm_run1,
            &mcp_run1,
            "login".to_string(),
            Workflow::default(),
            tools_run1,
            None,
        )
        .await
        .expect("run 1 ok");

    // Give the writer's consumer task a chance to land its insert.
    let events_run1 = drain_events(&mut event_rx_run1).await;

    // ASSERT 1: exactly one episode landed in the workflow-local store.
    let store = SqliteEpisodicStore::new(&wl_path, EpisodeScope::WorkflowLocal).unwrap();
    let row_count = store.row_count_for_tests().unwrap();
    assert_eq!(
        row_count,
        1,
        "one recovery should have been persisted; got {row_count}, events: {:?}",
        events_run1
            .iter()
            .map(event_kind)
            .collect::<Vec<&'static str>>(),
    );

    // ASSERT 1b: the writer emitted at least one EpisodeWritten event.
    let writes = events_run1
        .iter()
        .filter(|e| matches!(e, AgentEvent::EpisodeWritten { .. }))
        .count();
    assert!(
        writes >= 1,
        "EpisodeWritten event should fire at least once; saw kinds {:?}",
        events_run1
            .iter()
            .map(event_kind)
            .collect::<Vec<&'static str>>(),
    );

    // -----------------------------
    // Run 2: Same goal + same MCP element fixture. RunStart trigger
    // should fire and the LLM's first user turn should carry a
    // <retrieved_recoveries> block from run 1's episode.
    // -----------------------------
    let ctx_run2 = enabled_ctx(wl_path.clone());
    let cfg_run2 = config_with_steps(4);
    let llm_run2 = CapturingLlm::new(vec![llm_reply_tool_with_id(
        "agent_done",
        serde_json::json!({"summary": "instant done"}),
        "tc-r2",
    )]);
    let mcp_run2 = build_mcp_with_recovery_tools();
    let tools_run2 = mcp_run2.tools_as_openai();

    let runner_run2 = StateRunner::new_with_episodic("login".to_string(), cfg_run2, ctx_run2)
        .with_run_id(uuid::Uuid::new_v4());

    let _state2 = runner_run2
        .run(
            &llm_run2,
            &mcp_run2,
            "login".to_string(),
            Workflow::default(),
            tools_run2,
            None,
        )
        .await
        .expect("run 2 ok");

    // ASSERT 2: the very first user-turn message rendered on run 2
    // contains the retrieved-recoveries block — proving the run-start
    // retrieval trigger fired AND the splice in
    // `prompt::build_user_turn_message` works.
    assert!(
        llm_run2.call_count() >= 1,
        "LLM should have been consulted at least once on run 2"
    );
    let messages = llm_run2.messages_at(0);
    let user_turn_text = messages
        .iter()
        .filter_map(|m| m.content_text())
        .collect::<Vec<&str>>()
        .join("\n");
    assert!(
        user_turn_text.contains("<retrieved_recoveries>"),
        "run 2's first prompt should carry a <retrieved_recoveries> block; got:\n{}",
        user_turn_text
    );
}

#[tokio::test]
async fn episodic_disabled_via_config_skips_store_open_and_retrieval() {
    let dir = TempDir::new().unwrap();
    let wl_path = dir.path().join("episodic.sqlite");

    let ctx = enabled_ctx(wl_path.clone());
    let cfg = config_with_episodic_disabled(8);

    let llm = ScriptedLlm::new(vec![
        llm_reply_tool_with_id("cdp_click", serde_json::json!({"uid": "1_0"}), "tc-1"),
        llm_reply_tool_with_id("ax_click", serde_json::json!({"uid": "a1"}), "tc-2"),
        llm_reply_tool_with_id(
            "agent_done",
            serde_json::json!({"summary": "no episodic"}),
            "tc-3",
        ),
    ]);
    let mcp = build_mcp_with_recovery_tools();
    let tools = mcp.tools_as_openai();

    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(64);
    let runner = StateRunner::new_with_episodic("login".to_string(), cfg, ctx)
        .with_run_id(uuid::Uuid::new_v4())
        .with_events(event_tx)
        .with_episodic_writer();

    let _state = runner
        .run(
            &llm,
            &mcp,
            "login".to_string(),
            Workflow::default(),
            tools,
            None,
        )
        .await
        .expect("run ok with episodic disabled");

    // Drain so an in-flight insert (if any) had time to fire.
    let events = drain_events(&mut event_rx).await;

    // ASSERT 1: SQLite file never created — kill switch blocks store open.
    assert!(
        !wl_path.exists(),
        "episodic.sqlite must not be created when episodic_enabled=false"
    );

    // ASSERT 2: the writer never fires an EpisodeWritten event.
    let writes = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::EpisodeWritten { .. }))
        .count();
    assert_eq!(
        writes, 0,
        "no EpisodeWritten events when episodic_enabled=false"
    );
}

#[tokio::test]
async fn episodic_ctx_disabled_overrides_config_enabled() {
    let dir = TempDir::new().unwrap();
    let wl_path = dir.path().join("episodic.sqlite");

    // Config wants episodic on, but context says off — context wins
    // (D34 trace-gate semantics).
    let ctx = EpisodicContext::disabled();
    let cfg = AgentConfig::default(); // episodic_enabled = true

    let llm = ScriptedLlm::new(vec![
        llm_reply_tool_with_id("cdp_click", serde_json::json!({"uid": "1_0"}), "tc-1"),
        llm_reply_tool_with_id("ax_click", serde_json::json!({"uid": "a1"}), "tc-2"),
        llm_reply_tool_with_id(
            "agent_done",
            serde_json::json!({"summary": "ctx disabled"}),
            "tc-3",
        ),
    ]);
    let mcp = build_mcp_with_recovery_tools();
    let tools = mcp.tools_as_openai();

    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(64);
    let runner = StateRunner::new_with_episodic("login".to_string(), cfg, ctx)
        .with_run_id(uuid::Uuid::new_v4())
        .with_events(event_tx)
        .with_episodic_writer();

    let _state = runner
        .run(
            &llm,
            &mcp,
            "login".to_string(),
            Workflow::default(),
            tools,
            None,
        )
        .await
        .expect("run ok with ctx disabled");

    let events = drain_events(&mut event_rx).await;

    // ASSERT 1: SQLite path never instantiated (the disabled context
    // carries an empty PathBuf), so nothing on disk.
    assert!(
        !wl_path.exists(),
        "episodic.sqlite must not be created when ctx is disabled"
    );

    // ASSERT 2: no episodic events fire.
    let writes = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::EpisodeWritten { .. }))
        .count();
    assert_eq!(writes, 0, "no EpisodeWritten events when ctx is disabled");
}

#[tokio::test]
async fn run_with_no_recovery_writes_nothing() {
    // Regression guard: a clean run without any tool failures must
    // never persist an episode. The episodic write fires only on the
    // exact Recovering -> Executing transition; runs that stay in
    // Exploring/Executing throughout produce zero rows.
    let dir = TempDir::new().unwrap();
    let wl_path = dir.path().join("episodic.sqlite");
    let ctx = enabled_ctx(wl_path.clone());

    let mcp = StaticMcp::with_tools(&["cdp_find_elements", "cdp_click"])
        .with_reply(
            "cdp_find_elements",
            r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
        )
        .with_reply("cdp_click", "clicked");
    let tools = mcp.tools_as_openai();

    let llm = ScriptedLlm::new(vec![
        llm_reply_tool("cdp_click", serde_json::json!({"uid": "1_0"})),
        llm_reply_tool("agent_done", serde_json::json!({"summary": "clean run"})),
    ]);

    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(64);
    let runner = StateRunner::new_with_episodic("login".to_string(), config_with_steps(4), ctx)
        .with_run_id(uuid::Uuid::new_v4())
        .with_events(event_tx)
        .with_episodic_writer();

    let _state = runner
        .run(
            &llm,
            &mcp,
            "login".to_string(),
            Workflow::default(),
            tools,
            None,
        )
        .await
        .expect("clean run ok");

    let events = drain_events(&mut event_rx).await;

    // The store file may exist (it was opened at runner-construction
    // time) but it should hold zero rows — there was no recovery.
    if wl_path.exists() {
        let store = SqliteEpisodicStore::new(&wl_path, EpisodeScope::WorkflowLocal).unwrap();
        assert_eq!(
            store.row_count_for_tests().unwrap(),
            0,
            "no episodes should be written for a clean (no-recovery) run"
        );
    }
    let writes = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::EpisodeWritten { .. }))
        .count();
    assert_eq!(
        writes, 0,
        "no EpisodeWritten events for a clean (no-recovery) run"
    );
}

fn event_kind(e: &AgentEvent) -> &'static str {
    match e {
        AgentEvent::StepCompleted { .. } => "step_completed",
        AgentEvent::NodeAdded { .. } => "node_added",
        AgentEvent::EdgeAdded { .. } => "edge_added",
        AgentEvent::GoalComplete { .. } => "goal_complete",
        AgentEvent::Error { .. } => "error",
        AgentEvent::Warning { .. } => "warning",
        AgentEvent::CdpConnected { .. } => "cdp_connected",
        AgentEvent::StepFailed { .. } => "step_failed",
        AgentEvent::SubAction { .. } => "sub_action",
        AgentEvent::CompletionDisagreement { .. } => "completion_disagreement",
        AgentEvent::ConsecutiveDestructiveCapHit { .. } => "consecutive_destructive_cap_hit",
        AgentEvent::CompletionDisagreementResolved { .. } => "completion_disagreement_resolved",
        AgentEvent::TaskStateChanged { .. } => "task_state_changed",
        AgentEvent::WorldModelChanged { .. } => "world_model_changed",
        AgentEvent::BoundaryRecordWritten { .. } => "boundary_record_written",
        AgentEvent::EpisodeWritten { .. } => "episode_written",
        AgentEvent::EpisodePromoted { .. } => "episode_promoted",
    }
}
