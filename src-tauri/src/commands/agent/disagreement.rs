use super::*;

pub(super) async fn await_disagreement_resolution(
    app: &tauri::AppHandle,
    cancel_token: &CancellationToken,
    storage: &Arc<Mutex<clickweave_core::storage::RunStorage>>,
    run_id: &str,
    agent_summary: String,
    vlm_reasoning: String,
) -> Option<TerminalReason> {
    let (tx, rx) = tokio::sync::oneshot::channel::<DisagreementResolutionAction>();
    {
        let handle = app.state::<Mutex<AgentHandle>>();
        let mut guard = handle.lock().unwrap();
        guard.pending_disagreement_tx = Some(tx);
    }

    // Wait for the operator's decision, racing the run's cancellation
    // token so `stop_agent` during the adjudication window unblocks.
    //
    // `biased;` is load-bearing: without it, `tokio::select!` can pick
    // the cancel branch even when the resolver oneshot already carries
    // the operator's `Confirm`, which would silently overwrite the
    // user's decision with a `DisagreementCancelled` terminal record.
    // The resolver branch must always win when its channel is ready;
    // the cancel branch is the pure fallback for the adjudication-
    // window stop case (force_stop has no sender to consume because
    // `resolve_completion_disagreement` was never called).
    let action = tokio::select! {
        biased;
        res = rx => res.ok(),
        _ = cancel_token.cancelled() => {
            // Clear any stale sender the force_stop path did not consume
            // (theoretically impossible because force_stop always takes
            // it, but defensive is cheap here).
            let handle = app.state::<Mutex<AgentHandle>>();
            let mut guard = handle.lock().unwrap();
            guard.pending_disagreement_tx = None;
            Some(DisagreementResolutionAction::Cancel)
        }
    };

    let action = action?;

    // Persist the resolution to the durable run trace before any
    // terminal-emit side-effects. The Tauri event forwarder has already
    // exited by this point (the event_tx handle was dropped when the
    // engine returned), so we append directly via RunStorage.
    let resolved_event = AgentEvent::CompletionDisagreementResolved {
        action,
        agent_summary: agent_summary.clone(),
        vlm_reasoning: vlm_reasoning.clone(),
    };
    let _ = storage.lock().unwrap().append_agent_event(&resolved_event);
    // Also surface the decision as a lightweight Tauri event so UIs
    // outside the assistant panel (logs drawer, telemetry) observe the
    // resolution. This is in addition to the definitive `agent://complete`
    // / `agent://stopped` emission the caller performs next.
    let _ = app.emit(
        "agent://completion_disagreement_resolved",
        serde_json::json!({
            "run_id": run_id,
            "action": match action {
                DisagreementResolutionAction::Confirm => "confirm",
                DisagreementResolutionAction::Cancel => "cancel",
            },
        }),
    );

    Some(match action {
        DisagreementResolutionAction::Confirm => {
            TerminalReason::DisagreementConfirmed { agent_summary }
        }
        DisagreementResolutionAction::Cancel => TerminalReason::DisagreementCancelled {
            agent_summary,
            vlm_reasoning,
        },
    })
}
