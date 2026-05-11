use super::*;

/// Observation tools that do not become workflow action nodes. Mirrors
/// the legacy `OBSERVATION_TOOLS` list — duplicated here because the
/// legacy list was a private `const` on `AgentRunner`, and lifting it to
/// a shared module is out of scope for Task 3a.2 (refactoring pass
/// owned by 3b).
pub(super) const OBSERVATION_TOOLS: &[&str] = &[
    crate::agent::time_oracle::TOOL_NAME,
    "take_screenshot",
    "list_apps",
    "list_windows",
    "find_text",
    "find_image",
    "element_at_point",
    "take_ax_snapshot",
    "probe_app",
    "get_displays",
    "start_recording",
    "start_hover_tracking",
    "load_image",
    "cdp_list_pages",
    "cdp_take_snapshot",
    "cdp_summarize_page",
    "cdp_find_elements",
    "cdp_get_element_context",
    "cdp_wait_for_page_change",
    "android_list_devices",
];

/// AX dispatch tools whose uid arguments are scoped to one
/// `take_ax_snapshot`. See the legacy `AX_DISPATCH_TOOLS`.
const AX_DISPATCH_TOOLS: &[&str] = &["ax_click", "ax_set_value", "ax_select"];

/// Tools that transition app / window / CDP state. They are unsafe for
/// skill replay because replaying them against unchanged elements would
/// fire the transition a second time. See the legacy `STATE_TRANSITION_TOOLS`.
const STATE_TRANSITION_TOOLS: &[&str] = &[
    "launch_app",
    "focus_window",
    "quit_app",
    "cdp_connect",
    "cdp_disconnect",
];

/// Tools whose successful dispatch shifts which window has keyboard /
/// element focus. `observe()` drains a `FocusChanging` event for each.
pub(super) const FOCUS_CHANGING_TOOLS: &[&str] = &["focus_window", "launch_app", "quit_app"];

/// Tools that cross an app-process boundary (start or end an app).
/// In addition to focus, these invalidate window list, screenshot, and
/// AX-snapshot continuity records.
pub(super) const APP_LIFECYCLE_TOOLS: &[&str] = &["launch_app", "quit_app"];

/// Tools whose success implies a navigation in the active CDP page,
/// invalidating page state and the element surface.
pub(super) const CDP_NAVIGATION_TOOLS: &[&str] =
    &["cdp_navigate", "cdp_new_page", "cdp_select_page"];

/// True when the tool is observation-only — either hardcoded in
/// [`OBSERVATION_TOOLS`] or annotated with `readOnlyHint = true`. The
/// `CONFIRMABLE_TOOLS` carve-out (`launch_app` / `quit_app` / `cdp_connect`)
/// takes precedence so destructive side effects stay gated.
// `pub(crate)` so the ported `observation_union_tests` in
// `crate::agent::world_model` can exercise the predicate directly without
// routing through `StateRunner::classify_tool_result` (Task 3a.7.d).
pub(crate) fn is_observation_tool(
    tool_name: &str,
    annotations_by_tool: &HashMap<String, ToolAnnotations>,
) -> bool {
    if clickweave_core::permissions::CONFIRMABLE_TOOLS
        .iter()
        .any(|(n, _)| *n == tool_name)
    {
        return false;
    }
    if OBSERVATION_TOOLS.contains(&tool_name) {
        return true;
    }
    annotations_by_tool
        .get(tool_name)
        .and_then(|a| a.read_only_hint)
        .unwrap_or(false)
}

// `pub(crate)` so the ported `observation_union_tests` in
// `crate::agent::world_model` can verify dispatch classification without
// reaching through `StateRunner`'s private API (Task 3a.7.d).
pub(crate) fn is_ax_dispatch_tool(tool_name: &str) -> bool {
    AX_DISPATCH_TOOLS.contains(&tool_name)
}

// Same rationale as `is_ax_dispatch_tool` — exposed to the `world_model`-
// hosted port of `observation_union_tests` (Task 3a.7.d).
pub(crate) fn is_state_transition_tool(tool_name: &str) -> bool {
    STATE_TRANSITION_TOOLS.contains(&tool_name)
}

/// Build an index from tool name → MCP annotations from the openai-
/// shaped tool list. Tools without an `annotations` block produce the
/// default (all-`None`) struct. Mirrors the legacy `build_annotations_index`.
pub(super) fn build_annotations_index(mcp_tools: &[Value]) -> HashMap<String, ToolAnnotations> {
    mcp_tools
        .iter()
        .filter_map(|tool| {
            let name = tool
                .get("function")
                .and_then(|f| f.get("name"))
                .or_else(|| tool.get("name"))
                .and_then(Value::as_str)?;
            Some((name.to_string(), ToolAnnotations::from_tool_json(tool)))
        })
        .collect()
}

/// Compress a tool-arguments JSON value into a short string suitable
/// for the episodic [`CompactAction::brief_args`] field. Capped at
/// 120 chars (a multi-byte-safe truncation) so a giant blob argument
/// can never bloat the writer's payload.
pub(super) fn brief_summarize_args(arguments: &Value) -> String {
    let s = serde_json::to_string(arguments).unwrap_or_default();
    if s.len() <= 120 {
        return s;
    }
    let cut = (0..=117)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    format!("{}...", &s[..cut])
}

/// Join all text content from a `ToolCallResult` into a single string —
/// this is the body the LLM sees in the `tool_result` message.
// `pub(crate)` so the ported `observation_union_tests` in
// `crate::agent::world_model` can pin the joined-text contract (Task 3a.7.d).
pub(crate) fn extract_result_text(result: &clickweave_mcp::ToolCallResult) -> String {
    result
        .content
        .iter()
        .map(|content| match content {
            clickweave_mcp::ToolContent::Text { text } => text.clone(),
            clickweave_mcp::ToolContent::Image { mime_type, .. } => {
                format!("[image: {}]", mime_type)
            }
            clickweave_mcp::ToolContent::Unknown(_) => "[unknown content]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Pure diff over two `WorldModel::field_signatures()` snapshots.
///
/// Returns a `WorldModelDiff` whose `changed_fields` lists — in the
/// order `field_signatures` emits them — every field name whose
/// signature differs between `pre` and `post`. Used by `run_turn` to
/// emit `AgentEvent::WorldModelChanged` once per step after `observe`.
///
/// Panics only in the programmer-error case where `pre` and `post`
/// disagree on field ordering or length; `WorldModel::field_signatures`
/// is deterministic so this should never happen at runtime.
pub(crate) fn diff_world_model_signatures(
    pre: &[(&'static str, Option<usize>)],
    post: &[(&'static str, Option<usize>)],
) -> WorldModelDiff {
    debug_assert_eq!(
        pre.len(),
        post.len(),
        "field_signatures must return a stable-length vec",
    );
    let changed_fields = pre
        .iter()
        .zip(post.iter())
        .filter_map(|(p, q)| {
            debug_assert_eq!(
                p.0, q.0,
                "field_signatures must return fields in the same order",
            );
            (p.1 != q.1).then(|| p.0.to_string())
        })
        .collect();
    WorldModelDiff { changed_fields }
}
