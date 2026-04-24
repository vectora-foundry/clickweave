#![allow(dead_code)] // Phase 1: module wired to its own tests only; runtime consumers land in later phases.

use serde::Serialize;

/// Harness-inferred phase of the agent run. Never authored by the LLM (D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// No active subgoal; the agent is deciding what to pursue.
    Exploring,
    /// Active subgoal and recent steps are succeeding.
    Executing,
    /// Consecutive errors or an `agent_replan` this step.
    Recovering,
}

/// Inputs to `infer`. All signals are harness-tracked — no LLM-authored fields.
#[derive(Debug, Clone, Copy)]
pub struct PhaseSignals {
    pub stack_depth: usize,
    pub consecutive_errors: usize,
    pub last_replan_step: Option<usize>,
    pub current_step: usize,
}

/// Pure phase derivation (D5). Precedence: Recovering > Executing > Exploring.
pub fn infer(s: &PhaseSignals) -> Phase {
    if s.consecutive_errors >= 1 {
        return Phase::Recovering;
    }
    if let Some(replan) = s.last_replan_step
        && replan == s.current_step
    {
        return Phase::Recovering;
    }
    if s.stack_depth == 0 {
        return Phase::Exploring;
    }
    Phase::Executing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exploring_when_stack_empty_and_no_errors() {
        let p = infer(&PhaseSignals {
            stack_depth: 0,
            consecutive_errors: 0,
            last_replan_step: None,
            current_step: 5,
        });
        assert_eq!(p, Phase::Exploring);
    }

    #[test]
    fn executing_when_stack_nonempty_and_no_errors() {
        let p = infer(&PhaseSignals {
            stack_depth: 2,
            consecutive_errors: 0,
            last_replan_step: None,
            current_step: 5,
        });
        assert_eq!(p, Phase::Executing);
    }

    #[test]
    fn recovering_when_consecutive_errors_ge_one_regardless_of_stack() {
        for stack_depth in [0, 1, 3] {
            let p = infer(&PhaseSignals {
                stack_depth,
                consecutive_errors: 1,
                last_replan_step: None,
                current_step: 5,
            });
            assert_eq!(p, Phase::Recovering, "stack_depth={}", stack_depth);
        }
    }

    #[test]
    fn recovering_when_replan_fired_this_step() {
        let p = infer(&PhaseSignals {
            stack_depth: 2,
            consecutive_errors: 0,
            last_replan_step: Some(7),
            current_step: 7,
        });
        assert_eq!(p, Phase::Recovering);
    }

    #[test]
    fn not_recovering_when_replan_was_an_earlier_step() {
        let p = infer(&PhaseSignals {
            stack_depth: 2,
            consecutive_errors: 0,
            last_replan_step: Some(3),
            current_step: 7,
        });
        assert_eq!(p, Phase::Executing);
    }

    #[test]
    fn recovering_has_priority_over_exploring() {
        // Empty stack + consecutive error → Recovering (not Exploring).
        let p = infer(&PhaseSignals {
            stack_depth: 0,
            consecutive_errors: 2,
            last_replan_step: None,
            current_step: 5,
        });
        assert_eq!(p, Phase::Recovering);
    }
}

#[cfg(all(test, feature = "specta"))]
mod specta_derive_tests {
    //! D17: `Phase` is part of the Tauri `agent://*` event payload surface
    //! and must derive `specta::Type` so the bindings exporter picks it up.
    use super::*;
    use specta::{Generics, Type, TypeCollection};

    #[test]
    fn phase_derives_specta_type() {
        let _: specta::DataType = Phase::inline(&mut TypeCollection::default(), Generics::NONE);
    }
}
