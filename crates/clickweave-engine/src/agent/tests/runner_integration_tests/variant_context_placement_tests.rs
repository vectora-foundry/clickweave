use super::super::super::test_stubs::{
    CapturingLlm, StaticMcp, build_agent_done_response, llm_reply_tool,
};
use crate::agent::types::AgentConfig;
use crate::agent::{build_goal_block, run_agent_workflow};
use clickweave_llm::Role;

/// Variant context must appear in `messages[1]` (user/goal slot) and
/// never in `messages[0]` (system prompt). Asserts the D18 invariant
/// end-to-end through the public `run_agent_workflow` seam.
#[tokio::test]
async fn variant_context_lands_in_messages_1_not_messages_0() {
    const VARIANT_SENTINEL: &str = "VARIANT_CTX_SENTINEL_XYZ";
    let llm = CapturingLlm::new(vec![
        llm_reply_tool(
            "cdp_find_elements",
            serde_json::json!({"query": "", "max_results": 10}),
        ),
        build_agent_done_response("done"),
    ]);
    let mcp = StaticMcp::with_tools(&["cdp_find_elements"]).with_reply(
        "cdp_find_elements",
        r#"{"page_url":"about:blank","source":"cdp","matches":[]}"#,
    );

    // Compose the goal-block exactly the way the Tauri seam now
    // does — prior turns + variant context + user goal.
    let goal_block = build_goal_block(
        "log me in",
        &[],
        Some(&format!("variant=A; sentinel={}", VARIANT_SENTINEL)),
        1000,
    );

    let (_state, _writer_tx) = run_agent_workflow(
        &llm,
        AgentConfig::default(),
        goal_block,
        &mcp,
        None,
        None,
        None,
        uuid::Uuid::new_v4(),
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("run_agent_workflow ok");

    let messages = llm.messages_at(0);
    assert!(
        messages.len() >= 2,
        "runner must send at least [system, user] on the first turn; got len={}",
        messages.len()
    );
    assert_eq!(
        messages[0].role,
        Role::System,
        "messages[0] must be the system prompt"
    );
    assert_eq!(
        messages[1].role,
        Role::User,
        "messages[1] must be the user/goal turn"
    );

    let sys_text = messages[0].content_text().unwrap_or("").to_string();
    let user_text = messages[1].content_text().unwrap_or("").to_string();

    assert!(
        !sys_text.contains(VARIANT_SENTINEL),
        "D18: variant-context sentinel must NOT appear in messages[0] (system prompt); \
             found sentinel in system prompt: {sys_text}"
    );
    assert!(
        !sys_text.contains("Variant context:"),
        "D18: `Variant context:` header must NOT appear in messages[0]; \
             system prompt must stay stable across runs for prompt-cache hits"
    );
    assert!(
        user_text.contains(VARIANT_SENTINEL),
        "D18: variant-context sentinel must appear in messages[1] (goal slot); \
             user turn: {user_text}"
    );
    assert!(
        user_text.contains("Variant context:"),
        "D18: `Variant context:` header must appear in messages[1]; user turn: {user_text}"
    );
}

/// When no variant context is supplied, messages[0] and messages[1]
/// both remain free of a `Variant context:` header — the composed
/// goal-block collapses to the raw goal.
#[tokio::test]
async fn variant_context_absent_produces_clean_goal_block() {
    let llm = CapturingLlm::new(vec![build_agent_done_response("done")]);
    let mcp = StaticMcp::with_tools(&["cdp_find_elements"]);

    let goal_block = build_goal_block("just a goal", &[], None, 1000);

    let (_state, _writer_tx) = run_agent_workflow(
        &llm,
        AgentConfig::default(),
        goal_block,
        &mcp,
        None,
        None,
        None,
        uuid::Uuid::new_v4(),
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("run_agent_workflow ok");

    let messages = llm.messages_at(0);
    let sys_text = messages[0].content_text().unwrap_or("").to_string();
    let user_text = messages[1].content_text().unwrap_or("").to_string();

    assert!(
        !sys_text.contains("Variant context:"),
        "messages[0] must never carry a `Variant context:` header"
    );
    assert!(
        !user_text.contains("Variant context:"),
        "messages[1] must not carry a `Variant context:` header when none was supplied"
    );
    assert!(
        user_text.contains("just a goal"),
        "messages[1] must carry the raw user goal; got: {user_text}"
    );
}
