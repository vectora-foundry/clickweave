use super::*;

/// Regression: `AgentHandle::force_stop` must NOT drop the pending
/// approval sender silently. Dropping surfaces to the engine as
/// `Err(channel closed)` → `TerminalReason::ApprovalUnavailable`,
/// which the Tauri layer then emits as `agent://stopped { reason:
/// approval_unavailable }`. The fix sends `Ok(false)` explicitly so
/// the engine treats the stop as a rejection (`Replan`) and the
/// outer select races on `cancel_token.cancel()` to emit
/// `agent://stopped { reason: cancelled }`.
#[test]
fn force_stop_sends_rejection_through_pending_approval() {
    let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
    let mut handle = AgentHandle {
        cancel_token: Some(CancellationToken::new()),
        pending_approval_tx: Some(tx),
        ..Default::default()
    };

    let had_task = handle.force_stop();

    assert!(
        had_task,
        "force_stop should report true when cancel_token is installed"
    );
    // The receiver must see `Ok(false)` — not `Err` from a dropped sender.
    assert_eq!(
        rx.blocking_recv(),
        Ok(false),
        "force_stop must send explicit rejection, not drop the oneshot"
    );
}

/// `force_stop` must also cancel the CancellationToken so the outer
/// agent task observes the stop during the spawn window (before
/// `task_handle` is installed). The scenario: a user hits Stop while
/// MCP spawn is still in progress.
#[test]
fn force_stop_cancels_token_for_spawn_window_stop() {
    let token = CancellationToken::new();
    let mut handle = AgentHandle {
        cancel_token: Some(token.clone()),
        ..Default::default()
    };
    // Simulate the spawn window: no task_handle, no pending approval.
    // `force_stop` must still succeed — the token alone is sufficient
    // evidence that a run is in flight.

    let had_task = handle.force_stop();

    assert!(
        had_task,
        "force_stop must return true when a cancel_token is present \
             even without a task_handle (the spawn window)"
    );
    assert!(
        token.is_cancelled(),
        "The CancellationToken must be cancelled so the spawning \
             task sees the stop before it finishes MCP bring-up"
    );
}

/// `force_stop` must return false when no run is active, so the
/// Tauri command can return a validation error instead of silently
/// succeeding.
#[test]
fn force_stop_returns_false_when_no_run_active() {
    let mut handle = AgentHandle::default();
    let had_task = handle.force_stop();
    assert!(
        !had_task,
        "force_stop must return false when no run is active"
    );
}

/// When a VLM completion disagreement is pending, `force_stop` must
/// resolve the oneshot as `Cancel` — not drop it. Dropping would
/// surface as a receiver error in the Tauri task, leaving the run
/// without a truthful terminal record (variant index + events.jsonl
/// entry both missing).
#[test]
fn force_stop_resolves_pending_disagreement_as_cancel() {
    let (tx, rx) = tokio::sync::oneshot::channel::<DisagreementResolutionAction>();
    let mut handle = AgentHandle {
        cancel_token: Some(CancellationToken::new()),
        pending_disagreement_tx: Some(tx),
        ..Default::default()
    };

    let had_task = handle.force_stop();

    assert!(
        had_task,
        "force_stop must report true when a pending disagreement is installed"
    );
    assert_eq!(
        rx.blocking_recv(),
        Ok(DisagreementResolutionAction::Cancel),
        "force_stop must send explicit Cancel through the disagreement channel, \
             not drop the oneshot (drops cause ambiguous `unknown` terminal records)"
    );
}

/// Regression: even though `resolve_completion_disagreement` now
/// holds the AgentHandle lock across `tx.send(...)`, both branches
/// of the `await_disagreement_resolution` select can still be ready
/// at the same time — the loop's own cancellation path (e.g., a
/// workflow-level cancel or shutdown) can cancel the token
/// independently of `force_stop`, so a Confirm already sitting in
/// the oneshot can race a tripped token. Without `biased;`,
/// `tokio::select!` may pick the cancel branch and silently
/// overwrite the confirm with a DisagreementCancelled terminal
/// record. This test asserts the biased-select policy preserves
/// the operator's decision.
#[tokio::test]
async fn biased_select_preserves_confirm_when_token_also_cancelled() {
    let (tx, rx) = tokio::sync::oneshot::channel::<DisagreementResolutionAction>();
    let token = CancellationToken::new();

    // Arrange: both branches are ready simultaneously — the confirm
    // has been sent and the cancel-token has been tripped.
    tx.send(DisagreementResolutionAction::Confirm).unwrap();
    token.cancel();

    let action = tokio::select! {
        biased;
        res = rx => res.ok(),
        _ = token.cancelled() => Some(DisagreementResolutionAction::Cancel),
    };

    assert_eq!(
        action,
        Some(DisagreementResolutionAction::Confirm),
        "biased select must prefer the resolver oneshot over a \
             cancelled token so the operator's Confirm is never overwritten"
    );
}

/// Regression: `resolve_completion_disagreement` must hold the
/// `AgentHandle` lock across `tx.send(...)`. If the lock were
/// released after `.take()` but before `.send()`, a concurrent
/// `force_stop` could cancel the run's CancellationToken in the
/// gap — and then the select race in `await_disagreement_resolution`
/// would take the cancel branch before the confirm ever arrived,
/// silently overwriting the operator's decision. This test
/// simulates the interleaving: after the resolver's critical
/// section completes (ordered by the AgentHandle mutex), a
/// subsequent `force_stop` must find no pending sender and the
/// receiver must already hold the Confirm. Asserting this
/// invariant documents that the lock-hold-across-send policy is
/// load-bearing, not incidental.
#[test]
fn resolver_critical_section_closes_confirm_vs_force_stop_window() {
    let (tx, rx) = tokio::sync::oneshot::channel::<DisagreementResolutionAction>();
    let token = CancellationToken::new();
    let handle_mutex = Mutex::new(AgentHandle {
        cancel_token: Some(token.clone()),
        pending_disagreement_tx: Some(tx),
        ..Default::default()
    });

    // Simulate the resolver's critical section — `.take()` the sender
    // and send on it while still holding the lock. This mirrors the
    // real command.
    {
        let mut guard = handle_mutex.lock().unwrap();
        let tx = guard
            .pending_disagreement_tx
            .take()
            .expect("pending_disagreement_tx should be installed");
        tx.send(DisagreementResolutionAction::Confirm).unwrap();
    }

    // A later `force_stop` then observes no sender to consume (so
    // it cannot overwrite the confirm) and only cancels the token.
    let had_task = {
        let mut guard = handle_mutex.lock().unwrap();
        guard.force_stop()
    };

    assert!(had_task, "force_stop should report true on active run");
    assert!(token.is_cancelled(), "force_stop must cancel the token");
    assert_eq!(
        rx.blocking_recv(),
        Ok(DisagreementResolutionAction::Confirm),
        "receiver must still see the operator's Confirm — force_stop \
             had no pending sender to overwrite it with Cancel"
    );
}
