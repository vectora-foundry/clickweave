use super::*;

/// Number of consecutive successful dispatches of the same
/// `(tool_name, arguments)` tuple — over non-observation tools — that
/// trigger a no-progress nudge. The first repeat that crosses the
/// threshold injects the nudge; subsequent repeats keep injecting it
/// until the LLM picks a different action and the counter resets.
pub(super) const REPEAT_ACTION_THRESHOLD: u32 = 3;

/// Largest repeated action-cycle body to detect. Observation tools are
/// excluded before they reach this window.
pub(super) const ACTION_CYCLE_MAX_PATTERN_LEN: usize = 3;
pub(super) const ACTION_CYCLE_WINDOW: usize = ACTION_CYCLE_MAX_PATTERN_LEN * 2;

pub(super) const TEXT_SUBMIT_SEARCH_THRESHOLD: u32 = 3;

/// Prefix on the synthetic observation injected back to the LLM when
/// the repeat-action detector fires. Anchors the test assertion that
/// the nudge actually reached `previous_result`.
pub(crate) const NO_PROGRESS_NUDGE_PREFIX: &str = "[NO-PROGRESS NUDGE]";

/// Prefix on the `AgentEvent::Warning` message emitted alongside the
/// nudge. Anchors the test assertion that subscribers see the event.
pub(crate) const NO_PROGRESS_WARNING_PREFIX: &str = "no-progress";

pub(crate) const NO_ACTION_MUTATION_ONLY_PREFIX: &str = "[NO ACTION DISPATCHED]";

pub(super) const NO_ACTION_MUTATION_ONLY_REASON: &str = "[NO ACTION DISPATCHED] You emitted only task-state mutation pseudo-tools. The harness updated the task state, but no MCP/environment action ran: no click, fill, typing, navigation, or selection happened. Do not infer that the UI changed; choose a real action next or emit agent_replan with a new tactic.";

pub(crate) const UNVERIFIED_SIDE_EFFECT_PREFIX: &str = "[UNVERIFIED SIDE EFFECT]";

pub(super) const UNVERIFIED_SIDE_EFFECT_COMPLETION_BLOCKED_REASON: &str = "[UNVERIFIED SIDE EFFECT] The previous action may have changed external state, but its return value is not proof that the requested state is active. Verify the intended state with a structured observation or typed dispatch before calling complete_subgoal or agent_done.";

pub(crate) const STALE_CDP_UID_PREFIX: &str = "[STALE CDP UID]";

#[derive(Debug, Clone)]
pub(super) struct LastActionProgress {
    pub(super) tool_name: String,
    pub(super) arguments: Value,
    pub(super) context_signature: String,
    pub(super) count: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ActionProgressSignature {
    pub(super) tool_name: String,
    pub(super) arguments: Value,
    pub(super) context_signature: String,
}

#[derive(Debug, Clone)]
pub(super) struct TextSubmitSearchProgress {
    pub(super) context_signature: String,
    pub(super) count: u32,
}

pub(super) fn reset_no_progress_tracking(
    last_action: &mut Option<LastActionProgress>,
    recent_actions: &mut VecDeque<ActionProgressSignature>,
) {
    *last_action = None;
    recent_actions.clear();
}

pub(super) fn combine_with_side_effect_nudge(
    side_effect_nudge: Option<&str>,
    nudge: String,
) -> String {
    match side_effect_nudge {
        Some(side_effect_nudge) => format!("{side_effect_nudge}\n\n{nudge}"),
        None => nudge,
    }
}

pub(super) fn stable_no_progress_context_signature(world_model: &WorldModel) -> String {
    let focused_app = world_model.focused_app.as_ref().map(|fresh| {
        serde_json::json!({
            "name": &fresh.value.name,
            "kind": fresh.value.kind,
            "pid": fresh.value.pid,
        })
    });
    let cdp_page_url = world_model
        .cdp_page
        .as_ref()
        .map(|fresh| fresh.value.url.as_str());
    let element_surface = world_model
        .elements
        .as_ref()
        .map(|fresh| stable_element_surface_signature(&fresh.value));
    let cdp_page_fingerprint = world_model.cdp_page.as_ref().and_then(|fresh| {
        element_surface
            .is_none()
            .then_some(fresh.value.page_fingerprint.as_str())
    });
    let cdp_connect_status = world_model
        .cdp_connect_status
        .as_ref()
        .map(|fresh| fresh.value.as_str());
    let modal_present = world_model.modal_present.as_ref().map(|fresh| fresh.value);
    let dialog_present = world_model.dialog_present.as_ref().map(|fresh| fresh.value);
    let signature = serde_json::json!({
        "focused_app": focused_app,
        "cdp_page_url": cdp_page_url,
        "cdp_page_fingerprint": cdp_page_fingerprint,
        "cdp_connect_status": cdp_connect_status,
        "element_surface": element_surface,
        "modal_present": modal_present,
        "dialog_present": dialog_present,
    });
    let bytes = serde_json::to_vec(&signature).unwrap_or_default();
    blake3::hash(&bytes).to_hex().to_string()
}

fn stable_element_surface_signature(elements: &[ObservedElement]) -> String {
    let mut stable_entries: Vec<Value> = elements.iter().map(stable_observed_element_key).collect();
    stable_entries.sort_by_key(|entry| serde_json::to_string(entry).unwrap_or_default());
    let bytes = serde_json::to_vec(&stable_entries).unwrap_or_default();
    blake3::hash(&bytes).to_hex()[..16].to_string()
}

fn stable_observed_element_key(element: &ObservedElement) -> Value {
    match element {
        ObservedElement::Cdp(el) => serde_json::json!({
            "source": "cdp",
            "role": &el.role,
            "label": &el.label,
            "accessible_name": &el.accessible_name,
            "visible_text": &el.visible_text,
            "value": &el.value,
            "placeholder": &el.placeholder,
            "title": &el.title,
            "alt_text": &el.alt_text,
            "test_id": &el.test_id,
            "tag": &el.tag,
            "disabled": el.disabled,
            "parent_role": &el.parent_role,
            "parent_name": &el.parent_name,
        }),
        ObservedElement::Ax(el) => serde_json::json!({
            "source": "ax",
            "role": &el.role,
            "name": &el.name,
            "value": &el.value,
            "depth": el.depth,
            "focused": el.focused,
            "disabled": el.disabled,
            "parent_name": &el.parent_name,
        }),
        ObservedElement::Ocr(el) => serde_json::json!({
            "source": "ocr",
            "text": &el.text,
            "x_bin": el.x.div_euclid(10),
            "y_bin": el.y.div_euclid(10),
            "width_bin": el.width.div_euclid(10),
            "height_bin": el.height.div_euclid(10),
        }),
    }
}

pub(super) fn detect_repeated_action_cycle(
    recent_actions: &VecDeque<ActionProgressSignature>,
) -> Option<Vec<String>> {
    for pattern_len in 2..=ACTION_CYCLE_MAX_PATTERN_LEN {
        let needed = pattern_len * 2;
        if recent_actions.len() < needed {
            continue;
        }
        let len = recent_actions.len();
        let first: Vec<_> = recent_actions
            .iter()
            .skip(len - needed)
            .take(pattern_len)
            .collect();
        let second: Vec<_> = recent_actions
            .iter()
            .skip(len - pattern_len)
            .take(pattern_len)
            .collect();
        let has_distinct_actions = first.iter().skip(1).any(|sig| *sig != first[0]);
        if has_distinct_actions && first == second {
            return Some(first.iter().map(|sig| sig.tool_name.clone()).collect());
        }
    }
    None
}

/// Build the no-progress nudge body. Pure function so the prompt copy
/// stays out of the inner loop and can be exercised independently.
pub(super) fn build_no_progress_nudge(tool: &str, count: u32, prev_body: &str) -> String {
    format!(
        "{prefix} You have issued `{tool}` with the same arguments {count} turns in a row in the same stable app/page context, but the task is not advancing. Stop repeating this call. Either (1) switch dispatch family — if `<world_model>` has a `cdp_page` block, use CDP query/expand/action tools (e.g. `cdp_find_elements`, `cdp_get_element_context`, `cdp_click`, `cdp_fill`, `cdp_type_text`); if it has an AX tree, take a fresh `take_ax_snapshot` and use `ax_*` tools — or (2) push a narrower subgoal via `push_subgoal` and try a different tactic, or (3) emit `agent_replan`.\n\nPrevious tool body:\n{prev_body}",
        prefix = NO_PROGRESS_NUDGE_PREFIX,
    )
}

pub(super) fn build_action_cycle_nudge(cycle_summary: &str, prev_body: &str) -> String {
    format!(
        "{prefix} You are in a repeated action cycle in the same stable app/page context: `{cycle_summary}`. The task is not advancing. Do not run the same cycle again. Change the `cdp_find_elements` query, expand a candidate with `cdp_get_element_context`, verify the active context, or emit `agent_replan`.\n\nPrevious tool body:\n{prev_body}",
        prefix = NO_PROGRESS_NUDGE_PREFIX,
    )
}

pub(super) fn build_post_text_submit_nudge(count: u32, prev_body: &str) -> String {
    format!(
        "{prefix} You already wrote text into a textbox/editor, then searched for Send/Submit {count} times in the same stable page context without finding a matching control. Stop repeating send-button searches. If the focused editor should submit on Enter, call `cdp_press_key` with `{{\"key\":\"Enter\"}}`; otherwise use `cdp_get_element_context` around the composer controls or emit `agent_replan`.\n\nPrevious tool body:\n{prev_body}",
        prefix = NO_PROGRESS_NUDGE_PREFIX,
    )
}

pub(super) fn is_text_composition_tool(tool_name: &str) -> bool {
    matches!(tool_name, "cdp_fill" | "cdp_type_text")
}

pub(super) fn is_send_submit_cdp_search(arguments: &Value) -> bool {
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    query.contains("send") || query.contains("submit")
}

pub(super) fn cdp_find_elements_has_matches(tool_body: &str) -> Option<bool> {
    let parsed: Value = serde_json::from_str(tool_body).ok()?;
    let matches = parsed.get("matches")?.as_array()?;
    Some(!matches.is_empty())
}

fn cdp_evaluate_script_function(arguments: &Value) -> Option<&str> {
    arguments.get("function").and_then(Value::as_str)
}

fn cdp_evaluate_script_has_side_effect(function: &str) -> bool {
    let f = function.to_ascii_lowercase();
    [
        ".click(",
        ".dispatch_event(",
        ".dispatchevent(",
        ".submit(",
        ".focus(",
        ".blur(",
        ".scroll",
        ".setattribute(",
        ".removeattribute(",
        ".value =",
        ".checked =",
        ".selected =",
        ".innerhtml =",
        ".textcontent =",
        "localstorage.setitem(",
        "sessionstorage.setitem(",
        "window.location",
        "location.href",
        "location =",
        "history.pushstate(",
        "history.replacestate(",
    ]
    .iter()
    .any(|needle| f.contains(needle))
}

fn is_side_effectful_cdp_evaluate_script(tool_name: &str, arguments: &Value) -> bool {
    tool_name == "cdp_evaluate_script"
        && cdp_evaluate_script_function(arguments).is_some_and(cdp_evaluate_script_has_side_effect)
}

pub(super) fn is_unverified_side_effect_action(
    tool_name: &str,
    arguments: &Value,
    annotations_by_tool: &HashMap<String, ToolAnnotations>,
) -> bool {
    if tool_name == "cdp_evaluate_script" {
        return is_side_effectful_cdp_evaluate_script(tool_name, arguments);
    }

    annotations_by_tool
        .get(tool_name)
        .is_some_and(|annotations| {
            annotations.open_world_hint == Some(true) && annotations.destructive_hint == Some(true)
        })
}

pub(super) fn build_unverified_side_effect_nudge(tool_body: &str) -> String {
    format!(
        "{prefix} The last action may have changed external state, but its return value is not proof that the requested state is active. Before completing a subgoal or the whole goal, verify with structured state: use a typed dispatch when a stable target exists, or run a focused observation that proves the intended active context.\n\nPrevious action result:\n{tool_body}",
        prefix = UNVERIFIED_SIDE_EFFECT_PREFIX,
    )
}

fn previous_result_is_unverified_side_effect(previous_result: Option<&str>) -> bool {
    previous_result.is_some_and(|body| body.starts_with(UNVERIFIED_SIDE_EFFECT_PREFIX))
}

pub(super) fn guard_completion_after_unverified_side_effect(
    previous_result: Option<&str>,
    turn: &mut AgentTurn,
) -> bool {
    if !previous_result_is_unverified_side_effect(previous_result) {
        return false;
    }

    let before = turn.mutations.len();
    turn.mutations
        .retain(|m| !matches!(m, TaskStateMutation::CompleteSubgoal { .. }));
    let stripped_complete = before != turn.mutations.len();

    let blocked_done = matches!(turn.action, AgentAction::AgentDone { .. });
    if blocked_done {
        turn.action = AgentAction::AgentReplan {
            reason: UNVERIFIED_SIDE_EFFECT_COMPLETION_BLOCKED_REASON.to_string(),
        };
    }

    stripped_complete || blocked_done
}

pub(super) fn is_stale_cdp_uid_error(tool_name: &str, error: &str) -> bool {
    tool_name.starts_with("cdp_")
        && (error.contains("No node with given id found")
            || error.contains("could not be resolved to a DOM node")
            || error.contains("element is not attached")
            || error.contains("stale element"))
}

pub(super) fn build_stale_cdp_uid_nudge(error: &str) -> String {
    format!(
        "{prefix} The CDP element id from a previous observation is no longer valid. No click, fill, selection, or typing happened. Rediscover the target with `cdp_find_elements` before the next `cdp_click`/`cdp_fill`; do not reuse prior `d<N>` ids.\n\nOriginal error:\n{error}",
        prefix = STALE_CDP_UID_PREFIX,
    )
}
