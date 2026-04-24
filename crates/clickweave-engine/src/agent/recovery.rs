// Task 3a.1: recovery strategy stays live through `AgentRunner` in the
// legacy integration tests; Task 3a.6 re-wires it against `StateRunner::run`.
#![allow(dead_code)]

/// Action the agent should take after encountering an error.
///
/// The agent loop always re-observes the page on the next iteration,
/// so there is no distinct "retry same action" path. Recovery is
/// either "continue the loop" or "abort".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Continue the loop — re-observe and let the LLM choose.
    Continue,
    /// Abort the agent run — too many consecutive errors.
    Abort,
}

/// Determine whether the agent should continue or abort based on
/// consecutive error count and the configured maximum.
pub fn recovery_strategy(
    consecutive_errors: usize,
    max_consecutive_errors: usize,
) -> RecoveryAction {
    if consecutive_errors >= max_consecutive_errors {
        RecoveryAction::Abort
    } else {
        RecoveryAction::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_max_continues() {
        assert_eq!(recovery_strategy(1, 3), RecoveryAction::Continue);
        assert_eq!(recovery_strategy(2, 3), RecoveryAction::Continue);
    }

    #[test]
    fn at_max_aborts() {
        assert_eq!(recovery_strategy(3, 3), RecoveryAction::Abort);
    }

    #[test]
    fn above_max_aborts() {
        assert_eq!(recovery_strategy(5, 3), RecoveryAction::Abort);
    }

    #[test]
    fn zero_errors_continues() {
        assert_eq!(recovery_strategy(0, 3), RecoveryAction::Continue);
    }

    #[test]
    fn single_max_aborts_on_first() {
        assert_eq!(recovery_strategy(1, 1), RecoveryAction::Abort);
    }
}
