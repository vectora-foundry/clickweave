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

    // Rebuild messages: system prompt + summary + recent step messages
    let mut compacted = Vec::new();

    // Keep the system message (always first)
    if let Some(system_msg) = messages.first() {
        if system_msg.role == "system" {
            compacted.push(system_msg.clone());
        }
    }

    // Add compact summary as a user message
    compacted.push(Message::user(summary));

    // Each tool step contributes 3 messages: user observation + assistant tool-call + tool result.
    // Cache-replayed steps also contribute 2 messages (tool-call + tool-result).
    // Use 3 as the multiplier to avoid discarding context prematurely.
    let messages_per_step = 3;
    let recent_message_count = keep_recent * messages_per_step;
    let skip = messages.len().saturating_sub(recent_message_count);
    for msg in messages.iter().skip(skip) {
        // Don't duplicate the system message
        if msg.role != "system" {
            compacted.push(msg.clone());
        }
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
}
