//! Prompt construction for the state-spine agent runner.
//!
//! The system prompt is built once per run and never re-rendered, preserving
//! prompt-cache hits on the stable prefix (D6). The per-turn user message
//! composes the harness-rendered state block with the current observation.
//!
//! `truncate_summary` is preserved for VLM completion check paths.
//!
//! Phase 2a: this module is dormant — nothing in the live runner imports it.
//! Wiring lands in Phase 3 (cutover), at which point the old `prompt.rs` is
//! deleted and this file is renamed `prompt.rs`.

#![allow(dead_code)] // Phase 2a: module is dormant; live consumers land in Phase 3 cutover.

use clickweave_mcp::Tool;
use serde_json::{Value, json};

use crate::agent::render::{DEFAULT_MAX_ELEMENTS, render_step_input_with_cap};
use crate::agent::task_state::TaskState;
use crate::agent::world_model::{AppKind, WorldModel};

const SYSTEM_PROMPT_HEADER: &str = include_str!("../../prompts/agent_system.md");

/// Build the stable system prompt for the state-spine runner.
///
/// Stability is critical: this string is the prompt-cache prefix for every
/// turn of every run, so it must not embed run-specific data (goal, variant
/// context, timestamps). Variant context lands in `messages[1]` at the user
/// layer (D18).
pub fn build_system_prompt(tools: &[Tool]) -> String {
    build_system_prompt_with_header(SYSTEM_PROMPT_HEADER, tools)
}

/// Build a system prompt from an explicit header. Used by the eval harness
/// to run GEPA/candidate prompts without changing the production default.
pub fn build_system_prompt_with_header(header: &str, tools: &[Tool]) -> String {
    let mut out = String::from(header.trim_end());
    out.push_str("\n\nAvailable tools:\n");
    for t in tools {
        out.push_str("- ");
        out.push_str(&t.name);
        if let Some(desc) = &t.description
            && !desc.is_empty()
        {
            out.push_str(": ");
            out.push_str(desc);
        }
        out.push('\n');
    }
    out
}

/// Dispatch family a tool belongs to. Used by [`tools_in_scope`] to narrow
/// the per-turn `<tools_in_scope>` block based on the world-model state.
///
/// Classification is by tool name only — the MCP server keeps advertising
/// the full set, this enum lets the engine bias the LLM toward the right
/// family without changing what MCP exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchFamily {
    /// CDP-backed (`cdp_*`) — focus-preserving, requires a live CDP page.
    Cdp,
    /// macOS AX-backed (`ax_*`, `take_ax_snapshot`) — focus-preserving.
    Ax,
    /// Coordinate primitives (`click(x,y)`, `type_text`, `press_key`,
    /// `find_text`, `find_image`, etc.). Move the cursor and steal focus.
    Coordinate,
    /// Tools that don't dispatch a UI action — observation, app control,
    /// snapshots. Always in scope regardless of focused-app kind.
    Universal,
}

/// Classify a tool by name into its dispatch family. Conservative: tools
/// not on the explicit coordinate list fall through to `Universal` so a
/// new MCP tool added in the future doesn't get hidden by accident.
pub fn classify_tool_family(name: &str) -> DispatchFamily {
    if name.starts_with("cdp_") {
        return DispatchFamily::Cdp;
    }
    if name.starts_with("ax_") || name == "take_ax_snapshot" {
        return DispatchFamily::Ax;
    }
    match name {
        "click"
        | "type_text"
        | "press_key"
        | "move_mouse"
        | "scroll"
        | "drag"
        | "find_text"
        | "find_image"
        | "element_at_point"
        | "start_hover_tracking" => DispatchFamily::Coordinate,
        _ => DispatchFamily::Universal,
    }
}

/// Compute the subset of advertised MCP tool names the LLM should prefer
/// for the current world-model state.
///
/// Modes (focused_kind takes priority over `cdp_page_attached` so a stale
/// CDP page surviving an app switch doesn't push the wrong family for the
/// new focused app):
/// - `Electron` / `Chrome` focused + `cdp_page` attached → `Universal` + `Cdp`.
/// - `Electron` / `Chrome` focused, no `cdp_page` → `Universal` + `cdp_connect`.
///   The LLM calls `cdp_connect` or waits one turn for the harness's auto-connect.
/// - `Native` focused → `Universal` + `Ax`. Any `cdp_page` is treated as stale
///   and ignored — its binding is to a previously-focused CDP-capable app.
/// - No focused app yet → empty `Vec`. Callers render no block, so the LLM
///   sees the full `Available tools:` listing from the system prompt.
pub fn tools_in_scope(
    focused_kind: Option<AppKind>,
    cdp_page_attached: bool,
    all_tool_names: &[String],
) -> Vec<String> {
    match focused_kind {
        Some(AppKind::ElectronApp | AppKind::ChromeBrowser) if cdp_page_attached => all_tool_names
            .iter()
            .filter(|n| {
                n.as_str() != "cdp_evaluate_script"
                    && matches!(
                        classify_tool_family(n),
                        DispatchFamily::Cdp | DispatchFamily::Universal
                    )
            })
            .cloned()
            .collect(),
        Some(AppKind::ElectronApp | AppKind::ChromeBrowser) => all_tool_names
            .iter()
            .filter(|n| {
                matches!(classify_tool_family(n), DispatchFamily::Universal) || *n == "cdp_connect"
            })
            .cloned()
            .collect(),
        Some(AppKind::Native) => all_tool_names
            .iter()
            .filter(|n| {
                matches!(
                    classify_tool_family(n),
                    DispatchFamily::Ax | DispatchFamily::Universal
                )
            })
            .cloned()
            .collect(),
        None => Vec::new(),
    }
}

/// Render a `<tools_in_scope>` block listing the active subset by name.
/// Empty input → empty string so the caller can splice without branching.
pub fn render_tools_in_scope_block(names: &[String]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let body_estimate: usize = names.iter().map(|n| n.len() + 3).sum();
    let mut out =
        String::with_capacity(body_estimate + "<tools_in_scope>\n</tools_in_scope>\n".len());
    out.push_str("<tools_in_scope>\n");
    for n in names {
        out.push_str("- ");
        out.push_str(n);
        out.push('\n');
    }
    out.push_str("</tools_in_scope>\n");
    out
}

/// Build the per-turn user message. State block first (above the observation),
/// so the LLM reads world + task state before reacting to the observation.
///
/// `retrieved` is the optional Spec 2 episodic-memory result list. When
/// non-empty, a `<retrieved_recoveries>` sibling block is spliced in
/// after the state block and before the observation so the LLM sees
/// remembered recoveries before reacting to the new observation (D23).
///
/// `tools_in_scope_names` is the per-turn dispatch-family-narrowed subset
/// of advertised MCP tools (see [`tools_in_scope`]). Empty = no block.
pub fn build_user_turn_message(
    wm: &WorldModel,
    ts: &TaskState,
    current_step: usize,
    observation_text: &str,
    retrieved: &[crate::agent::episodic::RetrievedEpisode],
    tools_in_scope_names: &[String],
) -> String {
    build_user_turn_message_from_input(UserTurnMessageInput {
        wm,
        ts,
        current_step,
        observation_text,
        retrieved,
        applicable_skills: &[],
        tools_in_scope_names,
        max_elements: DEFAULT_MAX_ELEMENTS,
    })
}

/// Spec 3 variant of [`build_user_turn_message`] that also splices an
/// `<applicable_skills>` block when `applicable_skills` is non-empty.
/// The block lands after `<retrieved_recoveries>` and before the tools
/// scope / observation so the LLM sees the candidate procedural skills
/// alongside remembered recoveries.
pub fn build_user_turn_message_with_skills(
    wm: &WorldModel,
    ts: &TaskState,
    current_step: usize,
    observation_text: &str,
    retrieved: &[crate::agent::episodic::RetrievedEpisode],
    applicable_skills: &[crate::agent::skills::RetrievedSkill],
    tools_in_scope_names: &[String],
) -> String {
    build_user_turn_message_from_input(UserTurnMessageInput {
        wm,
        ts,
        current_step,
        observation_text,
        retrieved,
        applicable_skills,
        tools_in_scope_names,
        max_elements: DEFAULT_MAX_ELEMENTS,
    })
}

pub(crate) struct UserTurnMessageInput<'a> {
    pub wm: &'a WorldModel,
    pub ts: &'a TaskState,
    pub current_step: usize,
    pub observation_text: &'a str,
    pub retrieved: &'a [crate::agent::episodic::RetrievedEpisode],
    pub applicable_skills: &'a [crate::agent::skills::RetrievedSkill],
    pub tools_in_scope_names: &'a [String],
    pub max_elements: usize,
}

pub(crate) fn build_user_turn_message_from_input(input: UserTurnMessageInput<'_>) -> String {
    let mut out =
        render_step_input_with_cap(input.wm, input.ts, input.current_step, input.max_elements);

    let recoveries_block =
        crate::agent::episodic::render::render_retrieved_recoveries_block(input.retrieved);
    if !recoveries_block.is_empty() {
        out.push_str(&recoveries_block);
        out.push('\n');
    }

    if !input.applicable_skills.is_empty() {
        let skills_block =
            crate::agent::skills::render::render_applicable_skills_block(input.applicable_skills);
        if !skills_block.is_empty() {
            out.push_str(&skills_block);
            out.push('\n');
        }
    }

    let scope_block = render_tools_in_scope_block(input.tools_in_scope_names);
    if !scope_block.is_empty() {
        out.push_str(&scope_block);
    }

    if !input.observation_text.is_empty() {
        out.push_str("\n<observation>\n");
        out.push_str(input.observation_text);
        out.push_str("\n</observation>\n");
    }
    out
}

// --- ported from prompt.rs ---

/// Truncate text to `max_chars`, snapping to a character boundary.
/// Returns the original text if it fits within the limit.
///
/// Copied verbatim from `prompt.rs` so the VLM completion path can switch
/// over cleanly at Phase 3 cutover. The original copy remains in `prompt.rs`
/// as long as the old runner imports it.
pub fn truncate_summary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let end = text.floor_char_boundary(max_chars);
    format!("{}...", &text[..end])
}

/// Tool definition for the agent_done pseudo-tool.
///
/// Ported verbatim from the legacy `prompt.rs` — the state-spine runner
/// appends this (and `agent_replan_tool`) to the MCP tool list each turn so
/// the LLM sees the completion / replan actions as callable tools.
pub fn agent_done_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "agent_done",
            "description": "Declare the goal as complete. Call this when you have successfully achieved the objective.",
            "parameters": {
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "A brief summary of what was accomplished."
                    }
                },
                "required": ["summary"]
            }
        }
    })
}

/// Tool definition for the agent_replan pseudo-tool.
///
/// Ported verbatim from the legacy `prompt.rs`.
pub fn agent_replan_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "agent_replan",
            "description": "Request a re-plan when the current approach seems stuck or the goal appears unreachable.",
            "parameters": {
                "type": "object",
                "properties": {
                    "reason": {
                        "type": "string",
                        "description": "Why the current approach is not working."
                    }
                },
                "required": ["reason"]
            }
        }
    })
}

/// Tool definition for the harness-local date/time oracle.
///
/// This is intentionally a pseudo-tool, not an MCP server tool: the harness
/// answers it directly from the process clock so relative-date goals can ask
/// for a current runtime fact without relying on model memory or stale prompt
/// context.
pub fn get_current_datetime_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": crate::agent::time_oracle::TOOL_NAME,
            "description": "Return the current UTC and local date/time from the Clickweave runtime. Use this before interpreting relative dates such as today, tomorrow, yesterday, or current time.",
            "parameters": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            },
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": false
            }
        }
    })
}

// --- Task-state mutation pseudo-tools ---
//
// These describe the `AgentTurn.mutations` surface to the LLM via the
// OpenAI tool-calling API. They never dispatch to MCP —
// `parse_agent_turn` recognises their names and routes their arguments
// into `TaskStateMutation` values that the harness applies before the
// turn's action runs. The MCP tool list is unchanged by their presence.

pub fn push_subgoal_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "push_subgoal",
            "description": "Push a new subgoal onto the task-state stack. The new subgoal becomes the active focus until you call complete_subgoal. Mutation only — does not dispatch to MCP.",
            "parameters": {
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Short description of the subgoal."
                    }
                },
                "required": ["text"]
            }
        }
    })
}

pub fn complete_subgoal_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "complete_subgoal",
            "description": "Pop the top of the subgoal stack as completed and record a milestone summary. Mutation only — does not dispatch to MCP.",
            "parameters": {
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "What was accomplished for this subgoal."
                    }
                },
                "required": ["summary"]
            }
        }
    })
}

pub fn set_watch_slot_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "set_watch_slot",
            "description": "Mark a background concern (modal, auth, focus shift) that the harness should keep active while planning. Mutation only — does not dispatch to MCP.",
            "parameters": {
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "enum": ["pending_modal", "pending_auth", "pending_focus_shift"],
                        "description": "Which watch slot to set."
                    },
                    "note": {
                        "type": "string",
                        "description": "Operator-readable note about the concern."
                    }
                },
                "required": ["name", "note"]
            }
        }
    })
}

pub fn clear_watch_slot_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "clear_watch_slot",
            "description": "Clear a previously-set watch slot once the background concern has been resolved. Mutation only — does not dispatch to MCP.",
            "parameters": {
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "enum": ["pending_modal", "pending_auth", "pending_focus_shift"],
                        "description": "Which watch slot to clear."
                    }
                },
                "required": ["name"]
            }
        }
    })
}

pub fn record_hypothesis_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "record_hypothesis",
            "description": "Record a hypothesis you are about to test (rolling ring buffer, oldest evicted). Mutation only — does not dispatch to MCP.",
            "parameters": {
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The hypothesis under evaluation."
                    }
                },
                "required": ["text"]
            }
        }
    })
}

pub fn refute_hypothesis_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "refute_hypothesis",
            "description": "Mark a previously-recorded hypothesis as refuted. Index is the position in the current <task_state> hypotheses list. Mutation only — does not dispatch to MCP.",
            "parameters": {
                "type": "object",
                "properties": {
                    "index": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Index of the hypothesis to refute."
                    }
                },
                "required": ["index"]
            }
        }
    })
}

/// Tool definition for the invoke_skill pseudo-tool (Spec 3 Phase 4).
///
/// Replays a procedural skill listed in the previous turn's
/// `<applicable_skills>` block. The harness expands the skill's recorded
/// action sketch through the same dispatch helper as live tool calls so
/// the safety surface (permission policy, coordinate-primitive guard,
/// consecutive-destructive cap) is identical.
pub fn invoke_skill_tool() -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "invoke_skill",
            "description": "Invoke a procedural skill listed in <applicable_skills>. The harness expands and dispatches the skill's recorded steps. parameters must validate against the skill's parameter_schema.",
            "parameters": {
                "type": "object",
                "properties": {
                    "skill_id": { "type": "string" },
                    "version":  { "type": "integer" },
                    "parameters": { "type": "object" }
                },
                "required": ["skill_id", "version", "parameters"]
            }
        }
    })
}

/// Tool descriptor for `skill_patch_rebind_target`.
///
/// Emitted by the assistant to change the target binding of a specific step
/// in a skill's action_sketch. The harness synthesizes a `SkillPatch` whose
/// `action_sketch_replacements` updates the named step's `args` and whose
/// `replay_sidecar_mutations` carries `ClearSignals { step_id }` so the
/// replay engine re-records from scratch with the new target.
///
/// Whole-skill rewrites are refused at this boundary — each call must
/// address exactly one step.
pub fn skill_patch_rebind_target_tool() -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "skill_patch_rebind_target",
            "description": "Change the target binding of a single action_sketch step in an active skill. Synthesizes a SkillPatch that updates the step args and clears stale signals so the replay engine re-records with the new target. Whole-skill rewrites are not accepted.",
            "parameters": {
                "type": "object",
                "properties": {
                    "skill_id":         { "type": "string", "description": "Skill to patch." },
                    "step_id":          { "type": "string", "description": "step_id of the step whose target binding changes." },
                    "new_target_kind":  {
                        "type": "string",
                        "enum": ["ax_label", "cdp_selector", "image_crop", "coords"],
                        "description": "Target kind after the rebind."
                    },
                    "new_target_args":  { "type": "object", "description": "New args object for the step (must be valid for the chosen target kind)." }
                },
                "required": ["skill_id", "step_id", "new_target_kind", "new_target_args"]
            }
        }
    })
}

/// Tool descriptor for `skill_patch_reorder_sections`.
///
/// Emitted by the assistant to reorder `##` sections within a skill's body.
/// The harness synthesizes a `SkillPatch` that reorders both the section
/// markdown blocks and the contiguous action_sketch step ranges so they stay
/// in sync. No sidecar mutations are required.
pub fn skill_patch_reorder_sections_tool() -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "skill_patch_reorder_sections",
            "description": "Reorder the ##-level sections of a skill. The harness reorders both the markdown prose blocks and the corresponding contiguous action_sketch step ranges atomically. No sidecar mutations.",
            "parameters": {
                "type": "object",
                "properties": {
                    "skill_id":           { "type": "string", "description": "Skill to patch." },
                    "ordered_section_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Full list of ##-level section IDs in the desired order. Must be a permutation of the current section IDs."
                    }
                },
                "required": ["skill_id", "ordered_section_ids"]
            }
        }
    })
}

/// Tool descriptor for `skill_patch_promote_to_variable`.
///
/// Emitted by the assistant to lift a hard-coded literal value into a named
/// `variables` entry in the SKILL.md frontmatter. The harness synthesizes a
/// `SkillPatch` that:
/// - Adds the variable to `variables_additions`.
/// - Replaces the literal in the prose body with `{{variable_name}}`.
/// - Rewrites the matching action_sketch arg with the template reference.
/// - Clears image-crop signals only (AX/CDP signals remain valid, per D12).
pub fn skill_patch_promote_to_variable_tool() -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "skill_patch_promote_to_variable",
            "description": "Lift a hard-coded literal in a skill step into a named frontmatter variable. Updates both the prose body ({{variable_name}} template) and the action_sketch arg. Clears image-crop signals only — AX/CDP signals remain.",
            "parameters": {
                "type": "object",
                "properties": {
                    "skill_id":       { "type": "string", "description": "Skill to patch." },
                    "step_id":        { "type": "string", "description": "step_id of the step that contains the literal." },
                    "arg_path":       { "type": "string", "description": "Dot-separated path inside the step args object pointing to the literal (e.g. 'text' or 'selector')." },
                    "variable_name":  { "type": "string", "description": "Name for the new variable (must be a valid identifier)." },
                    "variable_type":  { "type": "string", "description": "Type tag for the variable schema (e.g. 'string', 'number')." },
                    "default":        { "description": "Optional default value for the variable." }
                },
                "required": ["skill_id", "step_id", "arg_path", "variable_name", "variable_type"]
            }
        }
    })
}

/// All harness-local pseudo-tools that the LLM may emit in a turn.
///
/// Order is intentional: the action pseudo-tools (`agent_done`,
/// `agent_replan`) remain near the end so the LLM-facing tool list still
/// clusters the "terminate the loop" choices, while the mutations cluster
/// together at the start of the pseudo-tool block. `invoke_skill` is appended
/// after `agent_replan` so the tool-list prefix stays stable for prompt-cache
/// compatibility across runs that toggle the skills layer. The three
/// `skill_patch_*` tools follow `invoke_skill` so the skills-layer prefix
/// remains cache-stable between runs that don't use patch primitives.
pub fn pseudo_tools() -> Vec<Value> {
    vec![
        push_subgoal_tool(),
        complete_subgoal_tool(),
        set_watch_slot_tool(),
        clear_watch_slot_tool(),
        record_hypothesis_tool(),
        refute_hypothesis_tool(),
        get_current_datetime_tool(),
        agent_done_tool(),
        agent_replan_tool(),
        invoke_skill_tool(),
        skill_patch_rebind_target_tool(),
        skill_patch_reorder_sections_tool(),
        skill_patch_promote_to_variable_tool(),
    ]
}

/// Names of the `skill_patch_*` pseudo-tools exposed to the assistant.
/// Used by `parse_agent_turn` to route calls to the patch synthesizer
/// rather than an MCP dispatch.
pub const SKILL_PATCH_TOOL_NAMES: &[&str] = &[
    "skill_patch_rebind_target",
    "skill_patch_reorder_sections",
    "skill_patch_promote_to_variable",
];

/// True when `name` is one of the three named `skill_patch_*` primitives.
pub fn is_skill_patch_tool_name(name: &str) -> bool {
    SKILL_PATCH_TOOL_NAMES.contains(&name)
}

/// Names of the pseudo-tools that map to `TaskStateMutation` rather than
/// `AgentAction`. Used by `parse_agent_turn` to route a tool call into
/// `mutations` instead of `action`. Kept as a small `&'static [&'static str]`
/// so the parser can match on it without rebuilding a HashSet per call.
pub const MUTATION_TOOL_NAMES: &[&str] = &[
    "push_subgoal",
    "complete_subgoal",
    "set_watch_slot",
    "clear_watch_slot",
    "record_hypothesis",
    "refute_hypothesis",
];

pub fn is_mutation_tool_name(name: &str) -> bool {
    MUTATION_TOOL_NAMES.contains(&name)
}

#[cfg(test)]
mod state_spine_prompt_tests {
    use super::*;
    use crate::agent::task_state::TaskState;
    use crate::agent::world_model::WorldModel;

    #[test]
    fn system_prompt_is_stable_across_calls_with_same_tool_list() {
        let tools: Vec<Tool> = vec![];
        let a = build_system_prompt(&tools);
        let b = build_system_prompt(&tools);
        assert_eq!(
            a, b,
            "system prompt must be deterministic for cache stability"
        );
    }

    #[test]
    fn system_prompt_does_not_contain_variant_context() {
        // D18: variant context now lives in messages[1], not messages[0].
        let tools: Vec<Tool> = vec![];
        let s = build_system_prompt(&tools);
        assert!(!s.contains("Variant context"));
    }

    #[test]
    fn system_prompt_does_not_recommend_eval_script_for_routine_cdp_inspection() {
        let tools: Vec<Tool> = vec![];
        let s = build_system_prompt(&tools);
        assert!(
            !s.contains("cdp_evaluate_script"),
            "default guidance should not steer routine CDP inspection through approval-gated eval"
        );
    }

    #[test]
    fn system_prompt_lists_tools_with_descriptions() {
        let tools = vec![
            Tool {
                name: "cdp_click".to_string(),
                description: Some("Click a CDP-backed element".to_string()),
                input_schema: serde_json::json!({}),
                annotations: None,
            },
            Tool {
                name: "ax_click".to_string(),
                description: None,
                input_schema: serde_json::json!({}),
                annotations: None,
            },
        ];
        let s = build_system_prompt(&tools);
        assert!(s.contains("- cdp_click: Click a CDP-backed element"));
        assert!(s.contains("- ax_click\n"));
    }

    #[test]
    fn user_turn_contains_state_block_and_observation() {
        let wm = WorldModel::default();
        let ts = TaskState::new("ship it".to_string());
        let out = build_user_turn_message(&wm, &ts, 3, "observation text here", &[], &[]);
        assert!(out.contains("<world_model>"));
        assert!(out.contains("<task_state>"));
        assert!(out.contains("observation text here"));
        // State block must appear before the observation.
        let wm_end = out.find("</world_model>").unwrap();
        let obs_start = out.find("observation text here").unwrap();
        assert!(
            wm_end < obs_start,
            "state block must precede the observation"
        );
    }

    #[test]
    fn user_turn_without_observation_omits_observation_tag() {
        let wm = WorldModel::default();
        let ts = TaskState::new("ship it".to_string());
        let out = build_user_turn_message(&wm, &ts, 0, "", &[], &[]);
        assert!(out.contains("<world_model>"));
        assert!(!out.contains("<observation>"));
    }

    #[test]
    fn user_turn_renders_tools_in_scope_block_above_observation() {
        let wm = WorldModel::default();
        let ts = TaskState::new("ship it".to_string());
        let scope = vec!["cdp_click".to_string(), "cdp_find_elements".to_string()];
        let out = build_user_turn_message(&wm, &ts, 1, "obs", &[], &scope);
        assert!(out.contains("<tools_in_scope>"));
        assert!(out.contains("- cdp_click"));
        assert!(out.contains("- cdp_find_elements"));
        let scope_end = out.find("</tools_in_scope>").unwrap();
        let obs_start = out.find("obs").unwrap();
        assert!(
            scope_end < obs_start,
            "tools_in_scope block must precede the observation"
        );
    }

    #[test]
    fn user_turn_omits_tools_in_scope_block_when_empty() {
        let wm = WorldModel::default();
        let ts = TaskState::new("ship it".to_string());
        let out = build_user_turn_message(&wm, &ts, 0, "obs", &[], &[]);
        assert!(!out.contains("<tools_in_scope>"));
    }

    #[test]
    fn user_turn_renders_applicable_skills_block_when_non_empty() {
        use crate::agent::skills::{
            ApplicabilityHints, ApplicabilitySignature, OutcomePredicate, RetrievedSkill, Skill,
            SkillScope, SkillState, SkillStats, SubgoalSignature,
        };
        use chrono::TimeZone;
        use std::sync::Arc;

        let wm = WorldModel::default();
        let ts = TaskState::new("ship it".to_string());
        let skill = Skill {
            id: "open-chat".into(),
            version: 1,
            state: SkillState::Confirmed,
            scope: SkillScope::ProjectLocal,
            name: "Open chat".into(),
            description: "desc".into(),
            tags: vec![],
            subgoal_text: "open chat".into(),
            subgoal_signature: SubgoalSignature("sig".into()),
            applicability: ApplicabilityHints {
                apps: vec![],
                hosts: vec![],
                signature: ApplicabilitySignature("a".into()),
            },
            parameter_schema: vec![],
            action_sketch: vec![],
            outputs: vec![],
            outcome_predicate: OutcomePredicate::SubgoalCompleted {
                post_state_world_model_signature: None,
            },
            provenance: vec![],
            stats: SkillStats::default(),
            edited_by_user: false,
            created_at: chrono::Utc.timestamp_opt(0, 0).unwrap(),
            updated_at: chrono::Utc.timestamp_opt(0, 0).unwrap(),
            produced_node_ids: vec![],
            body: String::new(),
            schema_version: crate::agent::skills::SKILL_SCHEMA_VERSION,
            variables: vec![],
            sections: vec![],
            replay: None,
        };
        let applicable = vec![RetrievedSkill {
            skill: Arc::new(skill),
            score: 1.0,
        }];
        let out = build_user_turn_message_with_skills(&wm, &ts, 1, "obs", &[], &applicable, &[]);
        assert!(out.contains("<applicable_skills>"));
        assert!(out.contains("Open chat"));
        let skills_end = out.find("</applicable_skills>").unwrap();
        let obs_start = out.find("obs").unwrap();
        assert!(skills_end < obs_start);
    }

    #[test]
    fn user_turn_omits_applicable_skills_block_when_empty() {
        let wm = WorldModel::default();
        let ts = TaskState::new("ship it".to_string());
        let out = build_user_turn_message_with_skills(&wm, &ts, 1, "obs", &[], &[], &[]);
        assert!(!out.contains("<applicable_skills>"));
    }

    #[test]
    fn classify_tool_family_buckets_known_prefixes() {
        assert_eq!(classify_tool_family("cdp_click"), DispatchFamily::Cdp);
        assert_eq!(
            classify_tool_family("cdp_take_dom_snapshot"),
            DispatchFamily::Cdp
        );
        assert_eq!(classify_tool_family("ax_click"), DispatchFamily::Ax);
        assert_eq!(classify_tool_family("take_ax_snapshot"), DispatchFamily::Ax);
        assert_eq!(classify_tool_family("click"), DispatchFamily::Coordinate);
        assert_eq!(
            classify_tool_family("type_text"),
            DispatchFamily::Coordinate
        );
        assert_eq!(
            classify_tool_family("find_text"),
            DispatchFamily::Coordinate
        );
        // App-control / observation falls through to Universal.
        assert_eq!(
            classify_tool_family("focus_window"),
            DispatchFamily::Universal
        );
        assert_eq!(
            classify_tool_family("take_screenshot"),
            DispatchFamily::Universal
        );
        assert_eq!(
            classify_tool_family("launch_app"),
            DispatchFamily::Universal
        );
    }

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn tools_in_scope_keeps_cdp_and_universal_when_cdp_page_attached() {
        let all = names(&[
            "cdp_click",
            "cdp_summarize_page",
            "cdp_find_elements",
            "cdp_get_element_context",
            "cdp_wait_for_page_change",
            "cdp_evaluate_script",
            "ax_click",
            "click",
            "find_text",
            "take_screenshot",
            "focus_window",
        ]);
        let scope = tools_in_scope(Some(AppKind::ElectronApp), true, &all);
        assert!(scope.iter().any(|n| n == "cdp_click"));
        assert!(scope.iter().any(|n| n == "cdp_summarize_page"));
        assert!(scope.iter().any(|n| n == "cdp_find_elements"));
        assert!(scope.iter().any(|n| n == "cdp_get_element_context"));
        assert!(scope.iter().any(|n| n == "cdp_wait_for_page_change"));
        assert!(scope.iter().any(|n| n == "take_screenshot"));
        assert!(scope.iter().any(|n| n == "focus_window"));
        assert!(!scope.iter().any(|n| n == "cdp_evaluate_script"));
        // Wrong-family tools must be filtered out so the LLM is not tempted.
        assert!(!scope.iter().any(|n| n == "ax_click"));
        assert!(!scope.iter().any(|n| n == "click"));
        assert!(!scope.iter().any(|n| n == "find_text"));
    }

    #[test]
    fn tools_in_scope_pre_connect_electron_only_exposes_cdp_connect() {
        // Electron focused but `cdp_connect` hasn't run yet: the LLM
        // must see only `cdp_connect` (plus universal) so it cannot
        // fall back to coordinate primitives or AX in the meantime.
        let all = names(&[
            "cdp_click",
            "cdp_connect",
            "ax_click",
            "click",
            "take_screenshot",
        ]);
        let scope = tools_in_scope(Some(AppKind::ElectronApp), false, &all);
        assert!(scope.iter().any(|n| n == "cdp_connect"));
        assert!(scope.iter().any(|n| n == "take_screenshot"));
        assert!(!scope.iter().any(|n| n == "cdp_click"));
        assert!(!scope.iter().any(|n| n == "ax_click"));
        assert!(!scope.iter().any(|n| n == "click"));
    }

    #[test]
    fn tools_in_scope_native_keeps_ax_and_universal() {
        let all = names(&[
            "cdp_click",
            "ax_click",
            "ax_set_value",
            "take_ax_snapshot",
            "click",
            "find_text",
            "take_screenshot",
        ]);
        let scope = tools_in_scope(Some(AppKind::Native), false, &all);
        assert!(scope.iter().any(|n| n == "ax_click"));
        assert!(scope.iter().any(|n| n == "ax_set_value"));
        assert!(scope.iter().any(|n| n == "take_ax_snapshot"));
        assert!(scope.iter().any(|n| n == "take_screenshot"));
        assert!(!scope.iter().any(|n| n == "cdp_click"));
        assert!(!scope.iter().any(|n| n == "click"));
        assert!(!scope.iter().any(|n| n == "find_text"));
    }

    #[test]
    fn tools_in_scope_native_ignores_stale_cdp_page() {
        // When the focused app is Native but a `cdp_page` from a prior
        // CDP-capable app still happens to be set, the filter must not
        // be tricked into the CDP arm — that would expose the wrong
        // family for the new focused app and hide AX tools.
        let all = names(&[
            "cdp_click",
            "ax_click",
            "ax_set_value",
            "take_ax_snapshot",
            "click",
            "take_screenshot",
        ]);
        let scope = tools_in_scope(Some(AppKind::Native), true, &all);
        assert!(scope.iter().any(|n| n == "ax_click"));
        assert!(scope.iter().any(|n| n == "take_ax_snapshot"));
        assert!(!scope.iter().any(|n| n == "cdp_click"));
        assert!(!scope.iter().any(|n| n == "click"));
    }

    #[test]
    fn tools_in_scope_returns_empty_when_no_focused_app() {
        // Empty Vec signals "no filter" — `render_tools_in_scope_block`
        // emits no block, so the LLM falls back to the system prompt's
        // full `Available tools:` listing.
        let all = names(&["cdp_click", "ax_click", "click"]);
        let scope = tools_in_scope(None, false, &all);
        assert!(scope.is_empty());
    }

    #[test]
    fn render_tools_in_scope_block_returns_empty_for_empty_input() {
        assert_eq!(render_tools_in_scope_block(&[]), "");
    }

    #[test]
    fn truncate_summary_short_text_unchanged() {
        assert_eq!(truncate_summary("hello", 10), "hello");
    }

    #[test]
    fn truncate_summary_long_text_truncated() {
        let long = "a".repeat(200);
        let result = truncate_summary(&long, 50);
        assert!(result.len() < 60);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn truncate_summary_multibyte_snaps_to_boundary() {
        // Multi-byte char must not be split mid-sequence.
        let text = "café!";
        let result = truncate_summary(text, 4);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn agent_done_tool_has_required_fields() {
        let tool = agent_done_tool();
        assert_eq!(tool["function"]["name"], "agent_done");
        let required = tool["function"]["parameters"]["required"]
            .as_array()
            .unwrap();
        assert!(required.iter().any(|r| r == "summary"));
    }

    #[test]
    fn agent_replan_tool_has_required_fields() {
        let tool = agent_replan_tool();
        assert_eq!(tool["function"]["name"], "agent_replan");
        let required = tool["function"]["parameters"]["required"]
            .as_array()
            .unwrap();
        assert!(required.iter().any(|r| r == "reason"));
    }

    #[test]
    fn get_current_datetime_tool_is_read_only_and_takes_no_args() {
        let tool = get_current_datetime_tool();
        assert_eq!(tool["function"]["name"], "get_current_datetime");
        assert_eq!(
            tool["function"]["annotations"]["readOnlyHint"],
            Value::Bool(true)
        );
        assert_eq!(
            tool["function"]["annotations"]["destructiveHint"],
            Value::Bool(false)
        );
        assert_eq!(
            tool["function"]["parameters"]["additionalProperties"],
            Value::Bool(false)
        );
        assert!(
            tool["function"]["parameters"]["properties"]
                .as_object()
                .is_some_and(|o| o.is_empty())
        );
    }

    #[test]
    fn pseudo_tools_trailing_order_is_stable() {
        // The skills-layer tools are appended at the tail of the list so the
        // system-prompt prefix shared by non-skill runs remains cache-stable.
        // invoke_skill must appear before the skill_patch_* group; the last
        // tool in the list is skill_patch_promote_to_variable.
        let tools = pseudo_tools();

        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("function")?.get("name")?.as_str())
            .collect();

        // invoke_skill precedes the patch primitives.
        let invoke_pos = names.iter().position(|n| *n == "invoke_skill").unwrap();
        let rebind_pos = names
            .iter()
            .position(|n| *n == "skill_patch_rebind_target")
            .unwrap();
        assert!(
            invoke_pos < rebind_pos,
            "invoke_skill must precede skill_patch_rebind_target"
        );

        // The three patch tools appear at the tail.
        let len = names.len();
        assert_eq!(names[len - 3], "skill_patch_rebind_target");
        assert_eq!(names[len - 2], "skill_patch_reorder_sections");
        assert_eq!(names[len - 1], "skill_patch_promote_to_variable");
    }

    #[test]
    fn invoke_skill_tool_requires_skill_id_version_parameters() {
        let v = invoke_skill_tool();
        let required = v
            .get("function")
            .and_then(|f| f.get("parameters"))
            .and_then(|p| p.get("required"))
            .and_then(Value::as_array)
            .unwrap();
        let names: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert!(names.contains(&"skill_id"));
        assert!(names.contains(&"version"));
        assert!(names.contains(&"parameters"));
    }
}
