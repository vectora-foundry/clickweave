//! Pure helpers for rendering prior-turn context into the agent's goal
//! message. Injection is inline in the goal string (not a separate
//! message slot) so `context::compact_step_summaries` — which assumes
//! `messages[1]` is the goal — stays correct across compaction.
//!
//! Budget: the entire rendered log is capped at ~1000 tokens. If the
//! verbose log exceeds budget, older turns collapse to one-liners.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::context::estimate_tokens;

/// A prior-turn record passed from the UI on every new `run_agent`
/// request. Summary may be redacted by the caller when some of the
/// turn's nodes were deleted (see `AssistantSlice::clearConversation`
/// + `useNodeChangeHandler` redaction rule).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorTurn {
    pub goal: String,
    pub summary: String,
    pub run_id: Uuid,
}

/// Build the text that will be inlined above the user's current goal.
/// When empty (no prior turns), returns an empty string so the goal
/// message is unchanged.
pub fn render_prior_turn_log(turns: &[PriorTurn], budget_tokens: usize) -> String {
    if turns.is_empty() {
        return String::new();
    }
    let verbose = render_verbose(turns);
    if estimate_tokens(&verbose) <= budget_tokens {
        return verbose;
    }
    render_truncated(turns, budget_tokens)
}

fn render_verbose(turns: &[PriorTurn]) -> String {
    let mut out = String::from("Previous conversation:\n");
    for (i, t) in turns.iter().enumerate() {
        out.push_str(&format!(
            "- Turn {}: User asked {:?} -> Assistant: {:?}\n",
            i + 1,
            t.goal.trim(),
            t.summary.trim(),
        ));
    }
    out
}

/// Keep the most recent turns verbose; collapse older ones to
/// `Turn N: "<goal>" -> completed.` one-liners until the whole log
/// fits inside the budget. The newest turn always stays verbose (even
/// if that overshoots the budget slightly — losing the current
/// conversational context is worse than a few hundred extra tokens).
fn render_truncated(turns: &[PriorTurn], budget_tokens: usize) -> String {
    // verbose_from indexes the first turn rendered verbosely. Start with
    // 0 (all verbose) and walk it forward until the rendered log fits —
    // but never past `turns.len() - 1` so the newest turn is always verbose.
    let last_idx = turns.len().saturating_sub(1);
    let mut verbose_from: usize = 0;
    loop {
        let rendered = compose(turns, verbose_from);
        if estimate_tokens(&rendered) <= budget_tokens {
            return rendered;
        }
        if verbose_from >= last_idx {
            // Everything older than the newest turn is already collapsed.
            return rendered;
        }
        verbose_from += 1;
    }
}

fn compose(turns: &[PriorTurn], verbose_from: usize) -> String {
    let mut out = String::from("Previous conversation:\n");
    for (i, t) in turns.iter().enumerate() {
        if i < verbose_from {
            out.push_str(&format!(
                "- Turn {}: {:?} -> completed.\n",
                i + 1,
                t.goal.trim(),
            ));
        } else {
            out.push_str(&format!(
                "- Turn {}: User asked {:?} -> Assistant: {:?}\n",
                i + 1,
                t.goal.trim(),
                t.summary.trim(),
            ));
        }
    }
    out
}

/// Build the final user goal message string, inlining the prior-turn
/// log above `current_goal` when non-empty. Returns the raw goal
/// string (matching the shape passed to `prompt::goal_message`).
pub fn build_goal_with_prior_turns(
    current_goal: &str,
    turns: &[PriorTurn],
    budget_tokens: usize,
) -> String {
    let log = render_prior_turn_log(turns, budget_tokens);
    if log.is_empty() {
        return current_goal.to_string();
    }
    format!("{}\nCurrent goal: {}", log, current_goal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(goal: &str, summary: &str) -> PriorTurn {
        PriorTurn {
            goal: goal.to_string(),
            summary: summary.to_string(),
            run_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn empty_turns_returns_empty_string() {
        assert!(render_prior_turn_log(&[], 1000).is_empty());
    }

    #[test]
    fn verbose_log_when_under_budget() {
        let turns = vec![t("send test to v", "sent the message")];
        let log = render_prior_turn_log(&turns, 1000);
        assert!(log.contains("Previous conversation"));
        assert!(log.contains("Turn 1"));
        assert!(log.contains("send test to v"));
        assert!(log.contains("sent the message"));
    }

    #[test]
    fn older_turns_collapse_when_over_budget() {
        // Large summaries force truncation.
        let big = "x".repeat(4000);
        let turns = vec![
            t("goal one", &big),
            t("goal two", &big),
            t("goal three", "short"),
        ];
        let log = render_prior_turn_log(&turns, 200);
        // Newest turn must remain verbose.
        assert!(log.contains("goal three"));
        assert!(log.contains("short"));
        // Older turn summaries must be collapsed (no big string).
        assert!(
            !log.contains(&big),
            "older turns should collapse to one-liners under budget"
        );
    }

    #[test]
    fn build_goal_includes_log_and_current_goal() {
        let turns = vec![t("prior", "done")];
        let composed = build_goal_with_prior_turns("new goal here", &turns, 1000);
        assert!(composed.contains("Previous conversation"));
        assert!(composed.contains("new goal here"));
    }

    #[test]
    fn build_goal_with_empty_turns_is_identity() {
        let composed = build_goal_with_prior_turns("just the goal", &[], 1000);
        assert_eq!(composed, "just the goal");
    }
}
