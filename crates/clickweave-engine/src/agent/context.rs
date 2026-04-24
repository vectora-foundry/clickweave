//! Transcript compaction for the state-spine runner.
//!
//! Rules (D12):
//! - `messages[0]` (system prompt) — never compacted.
//! - `messages[1]` (goal, with prior_turns + variant context inlined) — never compacted.
//! - Last `recent_n` assistant/tool pairs — preserved verbatim.
//! - Beyond `recent_n` — collapsed to a brief harness-authored line.
//! - Snapshot tool-result messages older than the current step are dropped.
//!
//! Continuity data lives in `WorldModel`; the transcript no longer carries it.
//!
//! Phase 2b: this module is dormant — nothing in the live runner imports it.
//! Wiring lands in Phase 3 (cutover), at which point the old `context.rs` is
//! deleted and this file is renamed `context.rs`.

#![allow(dead_code)] // Phase 2b: module is dormant; live consumers land in Phase 3 cutover.

use clickweave_llm::{Content, Message, Role};
use serde_json::Value;

/// Rough token estimate: ~4 characters per token for English text.
const CHARS_PER_TOKEN: usize = 4;

/// Prefix marking a tool-result body that has already been collapsed by
/// [`collapse_superseded_snapshots`]. Used to make the pass idempotent.
const SUPERSEDED_PREFIX: &str = "[superseded ";

/// Tools whose results embed a full page snapshot. Each successive call
/// returns a fresh view of the same page, so older payloads rarely help
/// planning and can be collapsed. `wait_for` is a legacy alias for
/// `cdp_wait_for` that some tool manifests still surface alongside the
/// prefixed form.
///
/// `take_screenshot` is deliberately excluded: its result body contains a
/// `screenshot_id` that `find_image` and the deterministic click/coordinate
/// flows can reference on a later turn. Collapsing an older screenshot
/// would erase that id from the only transcript copy the agent has.
pub(crate) const SNAPSHOT_PRODUCING_TOOLS: &[&str] = &[
    "cdp_take_dom_snapshot",
    "cdp_take_snapshot",
    "cdp_find_elements",
    "cdp_wait_for",
    "take_ax_snapshot",
    "wait_for",
];

fn make_superseded_placeholder(tool_name: &str) -> String {
    format!(
        "{}{} result — a newer snapshot of the same page was captured; \
         only the most recent snapshot is retained at full fidelity]",
        SUPERSEDED_PREFIX, tool_name
    )
}

/// Resolve the tool name a tool-result message refers to by scanning the
/// preceding assistant `tool_calls` for a matching id. Returns `None` if
/// `msg` is not a tool-result or the id cannot be resolved.
fn resolve_tool_name<'a>(messages: &'a [Message], msg: &Message) -> Option<&'a str> {
    if msg.role != Role::Tool {
        return None;
    }
    let call_id = msg.tool_call_id.as_deref()?;
    for prior in messages {
        if let Some(tool_calls) = &prior.tool_calls {
            for tc in tool_calls {
                if tc.id == call_id {
                    return Some(tc.function.name.as_str());
                }
            }
        }
    }
    None
}

/// Rough token estimate for a single string: ~4 characters per token.
///
/// Shared by context compaction and the prior-turn log renderer so the
/// heuristic stays consistent.
pub(crate) fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(CHARS_PER_TOKEN)
}

/// Estimate the total token count across a list of messages.
///
/// Ported from the legacy `context.rs` so callers outside the state-spine
/// compaction pipeline (the Phase 3a capability-gap test and any future
/// loop-detection / recovery code in `runner.rs`) can still reason about
/// transcript token pressure.
pub fn estimate_messages_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|m| {
            let content_len = m.content_text().map_or(0, |t| t.len());
            let tool_calls_len = m.tool_calls.as_ref().map_or(0, |tcs| {
                tcs.iter()
                    .map(|tc| tc.function.name.len() + json_value_len(&tc.function.arguments))
                    .sum()
            });
            (content_len + tool_calls_len).div_ceil(CHARS_PER_TOKEN)
        })
        .sum()
}

/// Approximate serialized length of a JSON `Value` without allocating the
/// string form. Used only for token-budget estimation, so exactness is not
/// required; avoid the full `to_string()` formatter on the hot agent path.
fn json_value_len(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(n) => n.to_string().len(),
        Value::String(s) => s.len() + 2,
        Value::Array(a) => a.iter().map(json_value_len).sum::<usize>() + a.len().max(1) + 1,
        Value::Object(map) => {
            map.iter()
                .map(|(k, v)| k.len() + 3 + json_value_len(v))
                .sum::<usize>()
                + map.len().max(1)
                + 1
        }
    }
}

/// Collapse snapshot-producing tool-result payloads that have been superseded
/// by a more recent snapshot-family call.
///
/// Ported from the legacy `context.rs`. The state-spine runner's primary
/// `compact` pipeline already drops stale snapshot bodies via the
/// recent-N / snapshot-family rules above, but this helper stays reachable
/// for the capability-gap test in `tests/mod.rs` and for any future
/// loop-detection / recovery code that wants an in-place supersession pass
/// without running the full compaction.
pub fn collapse_superseded_snapshots(messages: &[Message]) -> Option<Vec<Message>> {
    let mut latest_family_index: Option<usize> = None;
    for (idx, msg) in messages.iter().enumerate() {
        let Some(tool_name) = resolve_tool_name(messages, msg) else {
            continue;
        };
        if !SNAPSHOT_PRODUCING_TOOLS.contains(&tool_name) {
            continue;
        }
        latest_family_index = Some(idx);
    }

    let latest_family_index = latest_family_index?;

    let mut out = messages.to_vec();
    let mut changed = false;
    for (idx, msg) in out.iter_mut().enumerate() {
        let Some(tool_name) = resolve_tool_name(messages, msg) else {
            continue;
        };
        if !SNAPSHOT_PRODUCING_TOOLS.contains(&tool_name) {
            continue;
        }
        if idx == latest_family_index {
            continue;
        }
        if msg
            .content_text()
            .is_some_and(|t| t.starts_with(SUPERSEDED_PREFIX))
        {
            continue;
        }
        msg.content = Some(Content::Text(make_superseded_placeholder(tool_name)));
        changed = true;
    }

    if changed { Some(out) } else { None }
}

/// Tool names whose results are snapshot-family. Bodies older than the
/// current step get dropped entirely from the transcript.
const SNAPSHOT_TOOL_NAMES: &[&str] = &[
    "take_ax_snapshot",
    "take_screenshot",
    "cdp_take_dom_snapshot",
    "cdp_take_snapshot",
    "cdp_find_elements",
    "cdp_wait_for",
    "wait_for",
];

#[derive(Debug, Clone)]
pub struct CompactBudget {
    pub max_tokens: usize,
    pub recent_n: usize,
}

impl Default for CompactBudget {
    fn default() -> Self {
        Self {
            max_tokens: 100_000,
            recent_n: 6,
        }
    }
}

/// Compact a chat-history vector under the state-spine rules.
///
/// Invariants:
/// - `messages[0]` (system) and `messages[1]` (goal) are never modified.
/// - The last `budget.recent_n` assistant/tool pairs are preserved verbatim,
///   except that snapshot-family tool-result bodies older than the current
///   step are replaced by a short placeholder (body dropped).
/// - All pairs older than the recent-N window are collapsed to a single
///   brief assistant-authored summary line.
/// - If the total token estimate still exceeds `budget.max_tokens`, the
///   largest surviving body is truncated in-place until the estimate fits
///   or nothing is left to collapse.
pub fn compact(messages: Vec<Message>, budget: &CompactBudget) -> Vec<Message> {
    if messages.len() <= 2 {
        return messages;
    }

    let mut out = Vec::with_capacity(messages.len());
    out.push(messages[0].clone());
    out.push(messages[1].clone());

    // Pair up assistant + tool-result messages after messages[1].
    // Each "pair" is one assistant message followed by its tool-result(s).
    let tail = &messages[2..];
    let pairs = group_into_pairs(tail);

    let total_pairs = pairs.len();
    let recent_start = total_pairs.saturating_sub(budget.recent_n);
    // The "current step" is the last pair. Any snapshot-family tool-result
    // in an earlier pair is stale — the state block carries the fresh view
    // and the body should be dropped outright, independent of the recent-N
    // window and the token budget.
    let current_step_idx = total_pairs.saturating_sub(1);

    for (i, pair) in pairs.iter().enumerate() {
        let is_current_step = i == current_step_idx;
        if i >= recent_start {
            // Recent-N window: keep verbatim, but drop stale snapshot bodies.
            for m in pair {
                if !is_current_step && is_snapshot_tool_result(m) {
                    out.push(drop_snapshot_body(m));
                } else {
                    out.push(m.clone());
                }
            }
        } else {
            // Collapse: one brief summary line instead of the full pair.
            out.push(collapse_pair_to_brief(pair));
        }
    }

    // If we are still over budget, collapse more-recent pairs (after the
    // protected system + goal) until we fit.
    enforce_token_budget(out, budget)
}

/// Replace a snapshot-family tool-result body with a short placeholder,
/// preserving `role`, `tool_call_id`, and `name` so OpenAI tool-call
/// linkage stays intact.
fn drop_snapshot_body(m: &Message) -> Message {
    let placeholder = format!(
        "[{}: body dropped (older snapshot)]",
        m.name.as_deref().unwrap_or("snapshot")
    );
    Message {
        role: m.role,
        content: Some(Content::Text(placeholder)),
        reasoning_content: m.reasoning_content.clone(),
        tool_calls: m.tool_calls.clone(),
        tool_call_id: m.tool_call_id.clone(),
        name: m.name.clone(),
    }
}

fn group_into_pairs(messages: &[Message]) -> Vec<Vec<Message>> {
    let mut pairs: Vec<Vec<Message>> = Vec::new();
    let mut current: Vec<Message> = Vec::new();
    for m in messages {
        if m.role == Role::Assistant {
            if !current.is_empty() {
                pairs.push(std::mem::take(&mut current));
            }
            current.push(m.clone());
        } else {
            current.push(m.clone());
        }
    }
    if !current.is_empty() {
        pairs.push(current);
    }
    pairs
}

fn collapse_pair_to_brief(pair: &[Message]) -> Message {
    let asst = pair.iter().find(|m| m.role == Role::Assistant);
    let tool = pair.iter().find(|m| m.role == Role::Tool);
    let asst_kind = asst
        .and_then(|m| m.tool_calls.as_ref())
        .and_then(|tcs| tcs.first())
        .map(|tc| tc.function.name.clone())
        .unwrap_or_else(|| "text".to_string());
    let tool_kind = tool
        .and_then(|m| m.name.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let outcome = tool
        .and_then(|m| m.content_text().map(|t| truncate(t, 120)))
        .unwrap_or_default();
    Message {
        role: Role::Assistant,
        content: Some(Content::Text(format!(
            "[collapsed] action={} tool={} outcome={}",
            asst_kind, tool_kind, outcome
        ))),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

fn truncate(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    // Walk down to a UTF-8 char boundary so multibyte content
    // (tool outputs, UI labels) never panics. Matches the existing
    // `prompt.rs::truncate_summary` floor_char_boundary discipline.
    let mut boundary = cap;
    while boundary > 0 && !s.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}…", &s[..boundary])
}

fn is_snapshot_tool_result(m: &Message) -> bool {
    if m.role != Role::Tool {
        return false;
    }
    match m.name.as_deref() {
        Some(n) => SNAPSHOT_TOOL_NAMES.contains(&n),
        None => false,
    }
}

fn content_len(m: &Message) -> usize {
    m.content_text().map_or(0, |t| t.len())
}

fn enforce_token_budget(mut messages: Vec<Message>, budget: &CompactBudget) -> Vec<Message> {
    // Rough estimate: 4 characters per token (matches `context::estimate_tokens`).
    let est = |m: &Message| content_len(m) / 4 + 4;
    loop {
        let total: usize = messages.iter().map(est).sum();
        if total <= budget.max_tokens {
            return messages;
        }
        // Collapse the oldest non-system, non-goal body that still has a
        // meaningful payload (ignore bodies already collapsed by a prior
        // pass to guarantee progress).
        let collapse_idx = messages
            .iter()
            .enumerate()
            .skip(2)
            .find(|(_, m)| {
                let text = m.content_text().unwrap_or("");
                text.len() > 200
                    && !text.starts_with("[collapsed")
                    && !text.starts_with("[collapsed to fit budget]")
            })
            .map(|(i, _)| i);
        match collapse_idx {
            Some(i) => {
                let text = messages[i].content_text().unwrap_or("").to_string();
                let shortened = format!("[collapsed to fit budget] {}", truncate(&text, 80));
                messages[i].content = Some(Content::Text(shortened));
            }
            None => return messages, // cannot compact further
        }
    }
}

#[cfg(test)]
mod state_spine_compact_tests {
    use super::*;
    use clickweave_llm::{Content, FunctionCall, Message, Role, ToolCall};
    use serde_json::Value;

    fn msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: Some(Content::Text(content.to_string())),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    fn assistant_call(tool_name: &str, call_id: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: None,
            reasoning_content: None,
            tool_calls: Some(vec![ToolCall {
                id: call_id.to_string(),
                call_type: Default::default(),
                function: FunctionCall {
                    name: tool_name.to_string(),
                    arguments: Value::Object(Default::default()),
                },
            }]),
            tool_call_id: None,
            name: None,
        }
    }

    fn tool_result(name: &str, body: &str) -> Message {
        Message {
            role: Role::Tool,
            content: Some(Content::Text(body.to_string())),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("tc-1".to_string()),
            name: Some(name.to_string()),
        }
    }

    fn content_of(m: &Message) -> &str {
        m.content_text().unwrap_or("")
    }

    #[test]
    fn system_and_goal_never_compacted() {
        let messages = vec![
            msg(Role::System, "system prompt"),
            msg(Role::User, "goal text"),
            msg(Role::Assistant, "I will start."),
            tool_result("cdp_click", "ok"),
        ];
        let budget = CompactBudget {
            max_tokens: 16,
            recent_n: 1,
        };
        let out = compact(messages.clone(), &budget);
        assert_eq!(content_of(&out[0]), "system prompt");
        assert_eq!(content_of(&out[1]), "goal text");
    }

    #[test]
    fn drops_snapshot_tool_result_bodies_even_when_budget_is_huge() {
        // A snapshot tool-result older than the current step must be dropped,
        // independent of budget. The state-block in the current user turn
        // supersedes it.
        let long_body = "uid=a1g3 button\n".repeat(500);
        let messages = vec![
            msg(Role::System, "sys"),
            msg(Role::User, "goal"),
            msg(Role::Assistant, "take snapshot"),
            tool_result("take_ax_snapshot", &long_body),
            msg(Role::Assistant, "click"),
            tool_result("ax_click", "ok"),
        ];
        let budget = CompactBudget {
            max_tokens: 100_000,
            recent_n: 2,
        };
        let out = compact(messages, &budget);
        let has_full_ax_body = out
            .iter()
            .any(|m| m.name.as_deref() == Some("take_ax_snapshot") && content_of(m).len() > 200);
        assert!(
            !has_full_ax_body,
            "old snapshot tool-result bodies must be dropped, not merely collapsed"
        );
    }

    #[test]
    fn recent_n_pairs_preserved_verbatim_when_under_budget() {
        let messages = vec![
            msg(Role::System, "sys"),
            msg(Role::User, "goal"),
            msg(Role::Assistant, "a1"),
            tool_result("cdp_click", "r1"),
            msg(Role::Assistant, "a2"),
            tool_result("cdp_click", "r2"),
        ];
        let budget = CompactBudget {
            max_tokens: 10_000,
            recent_n: 2,
        };
        let out = compact(messages, &budget);
        assert!(out.iter().any(|m| content_of(m) == "a1"));
        assert!(out.iter().any(|m| content_of(m) == "a2"));
    }

    #[test]
    fn beyond_recent_n_pairs_collapse_to_brief_summaries() {
        let mut messages = vec![msg(Role::System, "sys"), msg(Role::User, "goal")];
        for i in 0..10 {
            messages.push(msg(Role::Assistant, &format!("a{}", i)));
            messages.push(tool_result("cdp_click", &format!("r{}", i)));
        }
        let budget = CompactBudget {
            max_tokens: 2_000,
            recent_n: 2,
        };
        let out = compact(messages, &budget);
        // Oldest pairs must be collapsed — we should not see the full
        // "a0" assistant content in the output.
        let has_a0 = out.iter().any(|m| content_of(m) == "a0");
        assert!(!has_a0, "oldest assistant pair must be collapsed");
        // But the most recent 2 pairs must be present verbatim.
        assert!(out.iter().any(|m| content_of(m) == "a8"));
        assert!(out.iter().any(|m| content_of(m) == "a9"));
    }

    #[test]
    fn cross_phase_snapshot_family_does_not_accumulate_bodies() {
        let long = "x".repeat(5_000);
        let messages = vec![
            msg(Role::System, "sys"),
            msg(Role::User, "goal"),
            msg(Role::Assistant, "ax"),
            tool_result("take_ax_snapshot", &long),
            msg(Role::Assistant, "dom"),
            tool_result("cdp_take_dom_snapshot", &long),
            msg(Role::Assistant, "find"),
            tool_result("cdp_find_elements", &long),
        ];
        let budget = CompactBudget {
            max_tokens: 500_000,
            recent_n: 3,
        };
        let out = compact(messages, &budget);
        let total: usize = out.iter().map(content_len).sum();
        assert!(
            total < 50_000,
            "no snapshot-family body should survive verbatim into the compacted output; got {} chars",
            total
        );
    }

    #[test]
    fn collapsed_summary_surfaces_action_and_tool_names() {
        // When a pair is collapsed beyond the recent-N window, the brief
        // summary line should name the assistant's tool_call and the tool
        // result it paired with, so the LLM retains the ordering even
        // without the full bodies.
        let messages = vec![
            msg(Role::System, "sys"),
            msg(Role::User, "goal"),
            assistant_call("cdp_click", "tc-collapse"),
            tool_result("cdp_click", "r-old"),
            msg(Role::Assistant, "a-recent"),
            tool_result("cdp_click", "r-recent"),
        ];
        let budget = CompactBudget {
            max_tokens: 10_000,
            recent_n: 1,
        };
        let out = compact(messages, &budget);
        let collapsed = out
            .iter()
            .find(|m| content_of(m).starts_with("[collapsed]"))
            .expect("should contain a collapsed summary");
        let text = content_of(collapsed);
        assert!(
            text.contains("cdp_click"),
            "collapsed summary should mention the tool name; got: {text}"
        );
    }

    #[test]
    fn truncate_is_utf8_boundary_safe() {
        // Crafted so a naive byte-slice would land mid-multibyte, which
        // would panic. The cap lands mid-ellipsis char → helper must walk
        // down to a char boundary before slicing.
        let s = "aa…bbb";
        let out = truncate(s, 3);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn enforce_token_budget_shrinks_oversized_bodies() {
        let huge = "y".repeat(20_000);
        let messages = vec![
            msg(Role::System, "sys"),
            msg(Role::User, "goal"),
            msg(Role::Assistant, &huge),
            tool_result("cdp_click", "ok"),
        ];
        let budget = CompactBudget {
            max_tokens: 100,
            recent_n: 5,
        };
        let out = compact(messages, &budget);
        let total_chars: usize = out.iter().map(content_len).sum();
        assert!(
            total_chars < 5_000,
            "enforce_token_budget should shrink oversized bodies; got {total_chars} chars"
        );
        // System + goal are still the first two.
        assert_eq!(content_of(&out[0]), "sys");
        assert_eq!(content_of(&out[1]), "goal");
    }

    #[test]
    fn short_histories_are_returned_unchanged() {
        let messages = vec![msg(Role::System, "sys"), msg(Role::User, "goal")];
        let budget = CompactBudget {
            max_tokens: 10,
            recent_n: 0,
        };
        let out = compact(messages.clone(), &budget);
        assert_eq!(out.len(), 2);
        assert_eq!(content_of(&out[0]), "sys");
        assert_eq!(content_of(&out[1]), "goal");
    }
}

#[cfg(test)]
mod legacy_context_tests {
    //! Tests for helpers ported from the pre-state-spine `context.rs`
    //! (`estimate_tokens`, `estimate_messages_tokens`,
    //! `collapse_superseded_snapshots`). These helpers are still reachable
    //! from `prior_turns.rs` and from capability-gap tests in
    //! `tests/mod.rs`, so they must stay covered after the Phase 3b
    //! rename.
    use super::*;
    use clickweave_llm::{CallType, FunctionCall, ToolCall};

    fn snapshot_pair(tool_name: &str, call_id: &str, body_kb: usize) -> (Message, Message) {
        let big_body = "x".repeat(body_kb * 1024);
        let assistant = Message::assistant_tool_calls(vec![ToolCall {
            id: call_id.to_string(),
            call_type: CallType::Function,
            function: FunctionCall {
                name: tool_name.to_string(),
                arguments: serde_json::json!({}),
            },
        }]);
        let result = Message::tool_result(call_id, big_body);
        (assistant, result)
    }

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

    #[test]
    fn collapse_returns_none_when_no_snapshot_tools() {
        let messages = vec![
            Message::system("System"),
            Message::user("Goal"),
            snapshot_pair("click", "call_0", 1).0,
            snapshot_pair("click", "call_0", 1).1,
        ];
        assert!(collapse_superseded_snapshots(&messages).is_none());
    }

    #[test]
    fn collapse_returns_none_with_single_snapshot() {
        let mut messages = vec![Message::system("System"), Message::user("Goal")];
        let (asst, result) = snapshot_pair("cdp_find_elements", "call_0", 4);
        messages.push(asst);
        messages.push(result);
        assert!(collapse_superseded_snapshots(&messages).is_none());
    }

    #[test]
    fn collapse_keeps_most_recent_snapshot_at_full_fidelity() {
        let mut messages = vec![Message::system("System"), Message::user("Goal")];
        for i in 0..4 {
            let (asst, result) = snapshot_pair("cdp_find_elements", &format!("call_{}", i), 4);
            messages.push(asst);
            messages.push(result);
        }

        let collapsed = collapse_superseded_snapshots(&messages)
            .expect("expected supersession to change the transcript");

        assert_eq!(collapsed.len(), messages.len());

        let tool_results: Vec<&Message> =
            collapsed.iter().filter(|m| m.role == Role::Tool).collect();
        assert_eq!(tool_results.len(), 4);

        for m in &tool_results[..3] {
            let text = m.content_text().expect("placeholder has text");
            assert!(
                text.starts_with("[superseded cdp_find_elements"),
                "older snapshot was not collapsed: {:?}",
                text,
            );
            assert!(m.tool_call_id.is_some(), "tool_call_id was stripped");
        }

        let latest = tool_results.last().unwrap();
        let latest_text = latest.content_text().unwrap();
        assert!(
            latest_text.len() > 1024,
            "most recent snapshot was collapsed ({}b)",
            latest_text.len()
        );
    }

    #[test]
    fn collapse_is_idempotent() {
        let mut messages = vec![Message::system("System"), Message::user("Goal")];
        for i in 0..3 {
            let (asst, result) = snapshot_pair("cdp_wait_for", &format!("call_{}", i), 4);
            messages.push(asst);
            messages.push(result);
        }

        let once = collapse_superseded_snapshots(&messages).expect("first pass rewrites");
        let twice = collapse_superseded_snapshots(&once);
        assert!(twice.is_none(), "second pass must be a no-op");
    }

    #[test]
    fn collapse_leaves_only_latest_snapshot_family_member() {
        let mut messages = vec![Message::system("System"), Message::user("Goal")];
        let specs = [
            ("cdp_find_elements", "a0"),
            ("cdp_take_dom_snapshot", "b0"),
            ("cdp_find_elements", "a1"),
            ("cdp_wait_for", "c0"),
            ("cdp_take_dom_snapshot", "b1"), // globally latest
        ];
        for (tool, id) in specs {
            let (asst, result) = snapshot_pair(tool, id, 2);
            messages.push(asst);
            messages.push(result);
        }

        let collapsed = collapse_superseded_snapshots(&messages)
            .expect("supersession should fire for multi-tool history");

        let collapsed_ids: Vec<String> = collapsed
            .iter()
            .filter(|m| m.role == Role::Tool)
            .filter(|m| {
                m.content_text()
                    .is_some_and(|t| t.starts_with("[superseded "))
            })
            .filter_map(|m| m.tool_call_id.clone())
            .collect();
        assert_eq!(
            collapsed_ids,
            vec![
                "a0".to_string(),
                "b0".to_string(),
                "a1".to_string(),
                "c0".to_string(),
            ],
        );

        let latest = collapsed
            .iter()
            .find(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some("b1"))
            .expect("b1 result must still be present");
        let body_len = latest.content_text().map(|t| t.len()).unwrap_or(0);
        assert!(
            body_len > 1024,
            "latest snapshot body was unexpectedly collapsed (len={})",
            body_len
        );
    }

    #[test]
    fn collapse_supersedes_cross_phase_cdp_then_ax_snapshots() {
        let mut messages = vec![Message::system("System"), Message::user("Goal")];

        let signal_dom = snapshot_pair("cdp_take_dom_snapshot", "signal_dom", 8);
        messages.push(signal_dom.0);
        messages.push(signal_dom.1);

        let signal_find = snapshot_pair("cdp_find_elements", "signal_find", 8);
        messages.push(signal_find.0);
        messages.push(signal_find.1);

        let calc_ax = snapshot_pair("take_ax_snapshot", "calc_ax", 8);
        messages.push(calc_ax.0);
        messages.push(calc_ax.1);

        let collapsed =
            collapse_superseded_snapshots(&messages).expect("cross-phase collapse must fire");

        for id in ["signal_dom", "signal_find"] {
            let m = collapsed
                .iter()
                .find(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some(id))
                .unwrap_or_else(|| panic!("tool-result {id} missing"));
            let text = m.content_text().unwrap();
            assert!(
                text.starts_with(SUPERSEDED_PREFIX),
                "{id} should be collapsed, got: {text:.60}"
            );
        }

        let calc = collapsed
            .iter()
            .find(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some("calc_ax"))
            .expect("calc_ax result missing");
        assert!(
            calc.content_text().map(|t| t.len()).unwrap_or(0) > 1024,
            "latest AX snapshot should keep full body"
        );
    }

    #[test]
    fn collapse_brings_40k_token_history_below_model_ctx() {
        const MODEL_CTX: usize = 40_192;

        let mut messages = vec![
            Message::system("You are an agent."),
            Message::user("## Goal\nMulti-phase workflow across Signal and Calculator"),
        ];
        let rotation = [
            "cdp_take_dom_snapshot",
            "cdp_find_elements",
            "cdp_wait_for",
            "take_ax_snapshot",
        ];
        for i in 0..14 {
            let tool = rotation[i % rotation.len()];
            let (asst, result) = snapshot_pair(tool, &format!("snap_{i}"), 14);
            messages.push(asst);
            messages.push(result);
        }
        let (asst, result) = snapshot_pair("click", "click_0", 1);
        messages.push(asst);
        messages.push(result);

        let before = estimate_messages_tokens(&messages);
        assert!(
            before > MODEL_CTX,
            "precondition: fixture should overflow model ctx, was {before}"
        );

        let collapsed = collapse_superseded_snapshots(&messages).expect("collapse should fire");
        let after = estimate_messages_tokens(&collapsed);
        assert!(
            after < MODEL_CTX,
            "collapsed history still exceeds model ctx: {after} > {MODEL_CTX}",
        );
        assert!(
            after * 4 < before,
            "collapse barely helped: before={before} after={after}",
        );
    }

    #[test]
    fn collapse_treats_take_ax_snapshot_as_snapshot_tool() {
        let mut messages = vec![Message::system("System"), Message::user("Goal")];
        for i in 0..3 {
            let (asst, result) = snapshot_pair("take_ax_snapshot", &format!("ax_{}", i), 4);
            messages.push(asst);
            messages.push(result);
        }

        let collapsed = collapse_superseded_snapshots(&messages)
            .expect("take_ax_snapshot supersession should fire");

        let collapsed_ids: Vec<String> = collapsed
            .iter()
            .filter(|m| m.role == Role::Tool)
            .filter(|m| {
                m.content_text()
                    .is_some_and(|t| t.starts_with("[superseded "))
            })
            .filter_map(|m| m.tool_call_id.clone())
            .collect();
        assert_eq!(collapsed_ids, vec!["ax_0".to_string(), "ax_1".to_string()]);
    }

    #[test]
    fn collapse_preserves_take_screenshot_results() {
        let mut messages = vec![Message::system("System"), Message::user("Goal")];
        for i in 0..3 {
            let (asst, result) = snapshot_pair("take_screenshot", &format!("shot_{}", i), 2);
            messages.push(asst);
            messages.push(result);
        }
        assert!(collapse_superseded_snapshots(&messages).is_none());
    }

    #[test]
    fn collapse_ignores_non_snapshot_tools() {
        let mut messages = vec![Message::system("System"), Message::user("Goal")];
        let (asst, result) = snapshot_pair("click", "call_0", 1);
        messages.push(asst);
        messages.push(result);

        for i in 0..2 {
            let (asst, result) = snapshot_pair("cdp_find_elements", &format!("snap_{}", i), 2);
            messages.push(asst);
            messages.push(result);
        }

        let collapsed = collapse_superseded_snapshots(&messages).unwrap();

        let click_body = collapsed
            .iter()
            .find(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some("call_0"))
            .and_then(|m| m.content_text().map(|s| s.len()))
            .unwrap();
        assert!(
            click_body > 500,
            "click tool result was incorrectly collapsed (len={})",
            click_body
        );
    }

    #[test]
    fn collapse_bounds_history_tokens_across_many_snapshot_calls() {
        let mut messages = vec![
            Message::system("You are an agent."),
            Message::user("## Goal\nMulti-step CDP workflow"),
        ];
        for i in 0..8 {
            let (asst, result) = snapshot_pair("cdp_find_elements", &format!("snap_{}", i), 8);
            messages.push(asst);
            messages.push(result);
        }

        let before_tokens = estimate_messages_tokens(&messages);
        let collapsed =
            collapse_superseded_snapshots(&messages).expect("expected collapse to fire");
        let after_tokens = estimate_messages_tokens(&collapsed);

        assert!(
            before_tokens > 10_000,
            "precondition: uncompressed history must be heavy, was {}",
            before_tokens
        );
        assert!(
            after_tokens < 4_000,
            "collapsed history too large: {} tokens (before={})",
            after_tokens,
            before_tokens,
        );
    }
}
