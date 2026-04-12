use super::prompt::summarize_steps;
use super::types::AgentStep;
use clickweave_llm::Message;

/// Rough token estimate: ~4 characters per token for English text.
const CHARS_PER_TOKEN: usize = 4;

/// Estimate the number of tokens in a string.
pub fn estimate_tokens(text: &str) -> usize {
    // Rough approximation: 1 token ≈ 4 characters
    text.len().div_ceil(CHARS_PER_TOKEN)
}

/// Estimate the total token count across a list of messages.
pub fn estimate_messages_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|m| {
            let content_len = m.content_text().map_or(0, |t| t.len());
            let tool_calls_len = m.tool_calls.as_ref().map_or(0, |tcs| {
                tcs.iter()
                    .map(|tc| tc.function.name.len() + tc.function.arguments.len())
                    .sum()
            });
            (content_len + tool_calls_len).div_ceil(CHARS_PER_TOKEN)
        })
        .sum()
}

/// Compact old step details into a summary when the context window is getting full.
///
/// Replaces individual step messages with a compact summary of the oldest steps,
/// keeping the most recent `keep_recent` steps in full detail.
///
/// Returns `None` if no compaction is needed (messages are within budget).
pub fn compact_step_summaries(
    messages: &[Message],
    steps: &[AgentStep],
    token_budget: usize,
    keep_recent: usize,
) -> Option<Vec<Message>> {
    let current_tokens = estimate_messages_tokens(messages);
    if current_tokens <= token_budget {
        return None;
    }

    if steps.len() <= keep_recent {
        // Not enough steps to compact
        return None;
    }

    // Split steps into old (to summarize) and recent (to keep)
    let split_at = steps.len().saturating_sub(keep_recent);
    let old_steps = &steps[..split_at];

    // Build a compact summary of old steps
    let summary = summarize_steps(old_steps);

    // Rebuild messages: system prompt + goal + summary + recent step messages
    let mut compacted = Vec::new();

    // Keep the system message (always first)
    if let Some(system_msg) = messages.first() {
        if system_msg.role == "system" {
            compacted.push(system_msg.clone());
        }
    }

    // Keep the goal message (second message — user-controlled goal text
    // that must survive compaction to keep the LLM on-task).
    if let Some(goal_msg) = messages.get(1) {
        if goal_msg.role == "user" {
            compacted.push(goal_msg.clone());
        }
    }

    // Add compact summary as a user message
    compacted.push(Message::user(summary));

    // LLM steps contribute 3 messages (user observation + assistant tool-call + tool result).
    // Cache-replayed steps contribute 2 (tool-call + tool-result).
    // Use 3 (the maximum across step types) to avoid discarding context prematurely.
    let messages_per_step = 3;
    let recent_message_count = keep_recent * messages_per_step;
    // Start copying from at least index 3 to skip the system message,
    // goal message, and any previously injected summary that were already
    // prepended above. This prevents repeated compaction from accumulating
    // stale summaries. Index 3 is safe because compaction only runs when
    // steps.len() > keep_recent, guaranteeing enough step messages exist.
    let skip = messages.len().saturating_sub(recent_message_count).max(3);
    for msg in messages.iter().skip(skip) {
        compacted.push(msg.clone());
    }

    Some(compacted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::{AgentCommand, AgentStep, StepOutcome};
    use clickweave_core::cdp::CdpFindElementMatch;

    #[test]
    fn estimate_tokens_basic() {
        // 12 characters → 3 tokens
        assert_eq!(estimate_tokens("hello world!"), 3);
    }

    #[test]
    fn estimate_tokens_empty() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_tokens_single_char() {
        assert_eq!(estimate_tokens("a"), 1);
    }

    #[test]
    fn estimate_messages_tokens_sums_content() {
        let messages = vec![
            Message::system("You are a helper."), // 18 chars → 5 tokens
            Message::user("Do something."),       // 13 chars → 4 tokens
        ];
        let total = estimate_messages_tokens(&messages);
        assert!(total > 0);
        assert_eq!(total, 5 + 4);
    }

    fn make_step(index: usize) -> AgentStep {
        AgentStep {
            index,
            elements: vec![CdpFindElementMatch {
                uid: format!("1_{}", index),
                role: "button".to_string(),
                label: "Click me".to_string(),
                tag: "button".to_string(),
                disabled: false,
                parent_role: None,
                parent_name: None,
            }],
            command: AgentCommand::ToolCall {
                tool_name: "click".to_string(),
                arguments: serde_json::json!({"uid": format!("1_{}", index)}),
                tool_call_id: format!("call_{}", index),
            },
            outcome: StepOutcome::Success("Clicked".to_string()),
            page_url: "https://example.com".to_string(),
        }
    }

    #[test]
    fn compact_returns_none_within_budget() {
        let messages = vec![
            Message::system("System prompt"),
            Message::user("Step 0"),
            Message::assistant("Action 0"),
        ];
        let steps = vec![make_step(0)];

        let result = compact_step_summaries(&messages, &steps, 100_000, 2);
        assert!(result.is_none());
    }

    #[test]
    fn compact_returns_none_when_few_steps() {
        let messages = vec![
            Message::system("System prompt"),
            Message::user("Step 0"),
            Message::assistant("Action 0"),
        ];
        let steps = vec![make_step(0)];

        // Budget is tiny but only 1 step which is <= keep_recent
        let result = compact_step_summaries(&messages, &steps, 1, 2);
        assert!(result.is_none());
    }

    #[test]
    fn compact_produces_shorter_messages() {
        // Create enough messages to exceed a small token budget
        let mut messages = vec![Message::system("System prompt")];
        let mut steps = Vec::new();
        for i in 0..10 {
            messages.push(Message::user(format!(
                "Observation step {} with a lot of element details and page info repeated",
                i
            )));
            messages.push(Message::assistant(format!("Action for step {}", i)));
            steps.push(make_step(i));
        }

        // Set a tiny budget to force compaction
        let result = compact_step_summaries(&messages, &steps, 10, 2);
        assert!(result.is_some());
        let compacted = result.unwrap();

        // Compacted should have fewer messages than original
        assert!(compacted.len() < messages.len());

        // Should start with system message
        assert_eq!(compacted[0].role, "system");

        // Should contain a summary message
        let has_summary = compacted.iter().any(|m| {
            m.content_text()
                .map_or(false, |t| t.contains("Previous Steps Summary"))
        });
        assert!(has_summary);
    }

    #[test]
    fn compact_preserves_goal_message() {
        // Simulate: [system, goal, obs0, asst0, tool0, obs1, asst1, tool1, ..., obs9, asst9, tool9]
        let mut messages = vec![
            Message::system("System prompt"),
            Message::user("## Goal\nOpen the calculator app"),
        ];
        let mut steps = Vec::new();
        for i in 0..10 {
            messages.push(Message::user(format!("Observation {}", i)));
            messages.push(Message::assistant(format!("Action {}", i)));
            messages.push(Message::tool_result(&format!("call_{}", i), "ok"));
            steps.push(make_step(i));
        }

        let result = compact_step_summaries(&messages, &steps, 10, 3);
        assert!(result.is_some());
        let compacted = result.unwrap();

        // Goal must survive compaction
        assert!(
            compacted.iter().any(|m| m
                .content_text()
                .map_or(false, |t| t.contains("Open the calculator app"))),
            "Goal message was dropped during compaction"
        );
    }

    #[test]
    fn compact_repeated_does_not_duplicate_goal_or_summary() {
        let mut messages = vec![
            Message::system("System prompt"),
            Message::user("## Goal\nDo the thing"),
        ];
        let mut steps = Vec::new();
        for i in 0..10 {
            messages.push(Message::user(format!("Observation {}", i)));
            messages.push(Message::assistant(format!("Action {}", i)));
            messages.push(Message::tool_result(&format!("call_{}", i), "ok"));
            steps.push(make_step(i));
        }

        // First compaction
        let first = compact_step_summaries(&messages, &steps, 10, 3).unwrap();

        // Second compaction on already-compacted transcript
        let second = compact_step_summaries(&first, &steps, 10, 3).unwrap();

        // Count goal messages — should be exactly 1
        let goal_count = second
            .iter()
            .filter(|m| {
                m.content_text()
                    .map_or(false, |t| t.contains("Do the thing"))
            })
            .count();
        assert_eq!(goal_count, 1, "Goal duplicated after repeated compaction");

        // Count summary messages — should be exactly 1
        let summary_count = second
            .iter()
            .filter(|m| {
                m.content_text()
                    .map_or(false, |t| t.contains("Previous Steps Summary"))
            })
            .count();
        assert_eq!(
            summary_count, 1,
            "Summary duplicated after repeated compaction"
        );
    }
}
