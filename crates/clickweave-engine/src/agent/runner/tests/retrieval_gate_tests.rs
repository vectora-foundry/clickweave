use super::*;
use crate::agent::episodic::{EpisodeScope, EpisodicContext, SqliteEpisodicStore};
use crate::agent::phase::Phase;
use tempfile::TempDir;

fn enabled_runner_with_store() -> (StateRunner, TempDir) {
    let dir = TempDir::new().unwrap();
    let wl_path = dir.path().join("episodic.sqlite");
    let ctx = EpisodicContext {
        enabled: true,
        workflow_local_path: wl_path.clone(),
        global_path: None,
        project_id: "gate-test-workflow".into(),
    };
    let runner = StateRunner::new_with_episodic("goal".to_string(), AgentConfig::default(), ctx);
    // Sanity: store opened.
    assert!(
        runner.episodic_store.is_some(),
        "test setup expects an episodic store",
    );
    // The `wl_path` is referenced indirectly through the runner's
    // store; pre-open one to confirm SQLite WAL mode took.
    let _verify = SqliteEpisodicStore::new(&wl_path, EpisodeScope::WorkflowLocal).unwrap();
    (runner, dir)
}

#[tokio::test]
async fn run_start_retrieval_consumes_gate_on_first_call() {
    let (mut r, _dir) = enabled_runner_with_store();
    assert!(!r.episodic_run_start_retrieved);

    // First call: run-start trigger fires (zero hits, but the
    // gate-consumed semantic still applies).
    let hits = r.try_retrieve_episodic(Phase::Exploring).await;
    assert!(
        hits.is_empty(),
        "fresh store has no episodes yet — retrieval should be empty",
    );
    assert!(
        r.episodic_run_start_retrieved,
        "first call must mark the run-start slot consumed regardless of hit count",
    );

    // Second call with no Recovering transition: must skip
    // entirely. Previously `step_index == 0` would have re-fired
    // RunStart on policy-deny early-continue paths.
    // Force `step_index` back to 0 to prove the gate (not the
    // counter) is what blocks re-fire.
    r.step_index = 0;
    let hits2 = r.try_retrieve_episodic(Phase::Exploring).await;
    assert!(
        hits2.is_empty(),
        "second call without Recovering transition must be a no-op",
    );
}

#[tokio::test]
async fn recovering_entry_still_fires_after_run_start_consumed() {
    let (mut r, _dir) = enabled_runner_with_store();

    // Consume the run-start slot.
    let _ = r.try_retrieve_episodic(Phase::Exploring).await;
    assert!(r.episodic_run_start_retrieved);

    // Transition into Recovering. Retrieval should fire on the
    // edge (returns empty here because no episodes exist yet, but
    // the call should still execute the trigger branch — verified
    // by the side effect of capturing a `recovering_snapshot`).
    r.task_state.phase = Phase::Recovering;
    let _ = r.try_retrieve_episodic(Phase::Exploring).await;
    assert!(
        r.recovering_snapshot.is_some(),
        "Recovering entry must capture a snapshot for the eventual write",
    );
}

#[tokio::test]
async fn advance_recorded_step_index_increments_counter() {
    let mut r = StateRunner::new_for_test("g".to_string());
    assert_eq!(r.step_index, 0);
    r.advance_recorded_step_index();
    assert_eq!(r.step_index, 1);
    r.advance_recorded_step_index();
    assert_eq!(r.step_index, 2);
}

#[tokio::test]
async fn record_policy_deny_failure_sets_stable_kind() {
    // Policy-deny branches funnel through this helper, and the snapshot derived from
    // `last_failed_*` populates `FailureSignature` on the
    // eventual write. The `error_kind` must be the stable
    // snake_case `policy_denied`, not a free-form string.
    let mut r = StateRunner::new_for_test("g".to_string());
    assert!(r.last_failed_tool_name.is_none());
    assert!(r.last_failed_error_kind.is_none());

    r.record_policy_deny_failure("cdp_click");
    assert_eq!(r.last_failed_tool_name.as_deref(), Some("cdp_click"));
    assert_eq!(
        r.last_failed_error_kind.as_deref(),
        Some("policy_denied"),
        "policy-deny error_kind must be the stable snake_case string used by both branches",
    );
}

#[tokio::test]
async fn clear_last_failure_tracking_drops_both_fields() {
    let mut r = StateRunner::new_for_test("g".to_string());
    r.record_policy_deny_failure("ax_click");
    r.clear_last_failure_tracking();
    assert!(
        r.last_failed_tool_name.is_none(),
        "tool_name must be cleared after success",
    );
    assert!(
        r.last_failed_error_kind.is_none(),
        "error_kind must be cleared after success",
    );
}

#[tokio::test]
async fn run_turn_no_longer_advances_step_index_directly() {
    // Under the new ownership rule, `run_turn` does not bump the
    // counter — that's the helper's job, called by sites that push
    // an `AgentStep`. `agent_done` is terminal with no step push,
    // so `step_index` must stay 0 after the turn.
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct EmptyExec(Mutex<Vec<Result<String, String>>>);
    #[async_trait]
    impl ToolExecutor for EmptyExec {
        async fn call_tool(&self, _: &str, _: &serde_json::Value) -> Result<String, String> {
            let mut q = self.0.lock().unwrap();
            q.pop().unwrap_or_else(|| Err("no result".into()))
        }
    }

    let mut r = StateRunner::new_for_test("g".to_string());
    let exec = EmptyExec(Mutex::new(vec![]));
    let done = AgentTurn {
        mutations: vec![],
        action: AgentAction::AgentDone {
            summary: "done".into(),
        },
    };
    let _ = r.run_turn(&done, &exec).await;
    assert_eq!(
        r.step_index, 0,
        "run_turn must not advance step_index — only `advance_recorded_step_index` does",
    );
}
