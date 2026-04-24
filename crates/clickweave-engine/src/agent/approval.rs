//! Approval-gate channel pair shared between `StateRunner` (state-spine
//! runner) and the legacy `AgentRunner`. Lives in its own module so the
//! state-spine runner can own it without a cyclic dep on `loop_runner`.
//!
//! Each approval request uses a fresh `tokio::sync::oneshot` channel to
//! avoid deadlocks — the runner sends an `ApprovalRequest` bundled with a
//! oneshot `Sender<bool>`, and the UI responds exactly once.

use tokio::sync::{mpsc, oneshot};

use crate::agent::types::ApprovalRequest;

/// Callback channel used to request approval from the UI before
/// executing a tool call. The runner sends an `ApprovalRequest` paired
/// with a oneshot reply channel; the UI replies exactly once with
/// `true` (approve) or `false` (reject).
pub struct ApprovalGate {
    pub request_tx: mpsc::Sender<(ApprovalRequest, oneshot::Sender<bool>)>,
}
