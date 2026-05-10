//! Mechanical prose generator for walkthrough-saved `SKILL.md` bodies.
//!
//! Walks `&[ActionSketchStep]` and emits a Claude-Code-readable markdown body
//! with:
//! - A single default section heading derived from the skill name (the caller
//!   supplies it so the generator stays a pure function).
//! - One prose line per top-level step, anchored with `<!-- step: <id> -->`.
//! - Loop steps rendered as a single block with a "(repeats up to N times)"
//!   suffix and their body steps listed beneath.
//! - Variable substitution placeholders (`{{var_name}}`) preserved verbatim.
//!
//! The output is designed to pass `parse_skill_md` round-trip validation when
//! appended to valid frontmatter + the matching fenced `action_sketch` block.

#![allow(dead_code)]

use super::types::ActionSketchStep;

/// Generate the markdown prose body for a walkthrough-saved skill.
///
/// `skill_name` is used for the top-level section heading. The returned string
/// contains the section heading and step markers in document order; the caller
/// appends the fenced `action_sketch` block via [`super::emitter::emit_skill_md`].
pub fn generate(steps: &[ActionSketchStep], skill_name: &str) -> String {
    let mut out = String::new();

    // Single top-level section derived from the skill name.
    let section_id = slugify_section(skill_name);
    out.push_str(&format!("## {skill_name}\n"));
    out.push_str(&format!("<!-- section: {section_id} -->\n"));

    for step in steps {
        match step {
            ActionSketchStep::ToolCall {
                step_id,
                tool,
                args,
                ..
            } => {
                let prose = tool_prose(tool, args);
                out.push_str(&format!("<!-- step: {step_id} -->\n"));
                out.push_str(&format!("{prose}\n"));
            }
            ActionSketchStep::Loop {
                step_id,
                body,
                max_iterations,
                ..
            } => {
                out.push_str(&format!("<!-- step: {step_id} -->\n"));
                out.push_str(&format!(
                    "Repeat the following steps (repeats up to {max_iterations} times):\n"
                ));
                for body_step in body {
                    if let ActionSketchStep::ToolCall {
                        step_id: inner_id,
                        tool,
                        args,
                        ..
                    } = body_step
                    {
                        let prose = tool_prose(tool, args);
                        out.push_str(&format!("  - <!-- step: {inner_id} --> {prose}\n"));
                    }
                }
            }
        }
    }

    out.push('\n');
    out
}

/// Produce a human-readable prose description for a single tool call.
fn tool_prose(tool: &str, args: &serde_json::Value) -> String {
    match tool {
        "click" => {
            let x = args.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = args.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            format!("Click at ({x:.0}, {y:.0}).")
        }
        "type_text" => {
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("<text>");
            format!("Type `{text}`.")
        }
        "find_text" => {
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("<text>");
            format!("Find text `{text}` on screen.")
        }
        "take_screenshot" => "Take a screenshot.".to_string(),
        "press_key" => {
            let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("<key>");
            format!("Press key `{key}`.")
        }
        "scroll" => {
            let dir = args
                .get("direction")
                .and_then(|v| v.as_str())
                .unwrap_or("down");
            format!("Scroll {dir}.")
        }
        "focus_window" => {
            let name = args
                .get("app_name")
                .and_then(|v| v.as_str())
                .unwrap_or("<app>");
            format!("Focus window `{name}`.")
        }
        "launch_app" => {
            let name = args
                .get("app_name")
                .and_then(|v| v.as_str())
                .unwrap_or("<app>");
            format!("Launch `{name}`.")
        }
        _ => {
            // Generic fallback: tool name + first string argument if any.
            let arg_hint = first_string_arg(args);
            if let Some(hint) = arg_hint {
                format!("Run `{tool}` with `{hint}`.")
            } else {
                format!("Run `{tool}`.")
            }
        }
    }
}

fn first_string_arg(args: &serde_json::Value) -> Option<&str> {
    if let Some(obj) = args.as_object() {
        obj.values().find_map(|v| v.as_str())
    } else {
        None
    }
}

/// Produce a stable section-id slug from the skill name: lowercase, spaces to
/// hyphens, non-alphanumeric stripped. Max 64 characters.
fn slugify_section(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .to_lowercase();
    // Collapse consecutive hyphens
    let mut out = String::new();
    let mut prev_hyphen = false;
    for ch in slug.chars() {
        if ch == '-' {
            if !prev_hyphen {
                out.push('-');
            }
            prev_hyphen = true;
        } else {
            out.push(ch);
            prev_hyphen = false;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "section".to_string()
    } else {
        trimmed.chars().take(64).collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agent::skills::types::{ActionSketchStep, ExpectedWorldModelDelta, LoopPredicate};

    fn tool_step(id: &str, tool: &str, args: serde_json::Value) -> ActionSketchStep {
        ActionSketchStep::ToolCall {
            step_id: id.to_string(),
            tool: tool.to_string(),
            args,
            captures_pre: vec![],
            captures: vec![],
            expected_world_model_delta: ExpectedWorldModelDelta::default(),
            requires_approval: None,
        }
    }

    fn loop_step(id: &str, body: Vec<ActionSketchStep>, max_iterations: u32) -> ActionSketchStep {
        ActionSketchStep::Loop {
            step_id: id.to_string(),
            until: LoopPredicate::StepCountReached {
                count: max_iterations,
            },
            body,
            max_iterations,
            iteration_delay_ms: 500,
        }
    }

    // (a) single section, single step
    #[test]
    fn single_section_single_step() {
        let steps = vec![tool_step(
            "s_000001",
            "click",
            json!({"x": 12.0, "y": 34.0}),
        )];
        let body = generate(&steps, "Click something");
        assert!(body.contains("## Click something"));
        assert!(body.contains("<!-- section: click-something -->"));
        assert!(body.contains("<!-- step: s_000001 -->"));
        assert!(body.contains("Click at (12, 34)."));
    }

    // (b) multi-step section
    #[test]
    fn multi_step_section() {
        let steps = vec![
            tool_step("s_000001", "click", json!({"x": 1.0, "y": 2.0})),
            tool_step("s_000002", "type_text", json!({"text": "hello"})),
        ];
        let body = generate(&steps, "Enter data");
        assert!(body.contains("<!-- step: s_000001 -->"));
        assert!(body.contains("<!-- step: s_000002 -->"));
        assert!(body.contains("Type `hello`."));
    }

    // (c) loop
    #[test]
    fn loop_step_renders_with_repeat_suffix() {
        let inner = vec![tool_step("s_000002", "take_screenshot", json!({}))];
        let steps = vec![loop_step("s_loop_000000", inner, 5)];
        let body = generate(&steps, "Poll loop");
        assert!(body.contains("<!-- step: s_loop_000000 -->"));
        assert!(body.contains("repeats up to 5 times"));
        assert!(body.contains("s_000002"));
    }

    // (d) variable substitution placeholder preserved
    #[test]
    fn variable_substitution_preserved_verbatim() {
        let steps = vec![tool_step(
            "s_000001",
            "type_text",
            json!({"text": "{{email_address}}"}),
        )];
        let body = generate(&steps, "Fill form");
        // The placeholder must appear verbatim in the prose
        assert!(body.contains("{{email_address}}"));
    }

    // Output passes parse_skill_md round-trip when combined with frontmatter
    // and a matching action_sketch fence.
    #[test]
    fn round_trip_via_parse_skill_md() {
        use crate::agent::skills::emitter::emit_skill_md;
        use crate::agent::skills::parser::parse_skill_md;
        use crate::agent::skills::types::{
            ApplicabilityHints, ApplicabilitySignature, OutcomePredicate, ProvenanceEntry, Skill,
            SkillScope, SkillState, SkillStats, SubgoalSignature,
        };
        use chrono::Utc;

        let steps = vec![
            tool_step("s_000000", "click", json!({"x": 5.0, "y": 10.0})),
            tool_step("s_000001", "type_text", json!({"text": "world"})),
        ];
        let skill_name = "Simple test";
        let prose_body = generate(&steps, skill_name);

        let now = Utc::now();
        let skill = Skill {
            id: "skl_test01".to_string(),
            version: 1,
            state: SkillState::Draft,
            scope: SkillScope::ProjectLocal,
            name: skill_name.to_string(),
            description: "Test skill".to_string(),
            tags: vec![],
            subgoal_text: skill_name.to_string(),
            subgoal_signature: SubgoalSignature("sig".to_string()),
            applicability: ApplicabilityHints {
                apps: vec![],
                hosts: vec![],
                signature: ApplicabilitySignature("app_sig".to_string()),
            },
            parameter_schema: vec![],
            action_sketch: steps,
            outputs: vec![],
            outcome_predicate: OutcomePredicate::SubgoalCompleted {
                post_state_world_model_signature: None,
            },
            provenance: vec![ProvenanceEntry {
                run_id: "run_1".to_string(),
                step_index: 0,
                completed_at: now,
                workflow_hash: "h".to_string(),
            }],
            stats: SkillStats {
                occurrence_count: 1,
                success_rate: 1.0,
                last_seen_at: Some(now),
                last_invoked_at: None,
            },
            edited_by_user: false,
            created_at: now,
            updated_at: now,
            produced_node_ids: vec![],
            body: prose_body.clone(),
            schema_version: crate::agent::skills::SKILL_SCHEMA_VERSION,
            variables: vec![],
            sections: vec![],
            replay: None,
        };

        // emit_skill_md uses skill.body when sections is empty
        let md = emit_skill_md(&skill);
        // The round-trip parse should succeed
        let parsed = parse_skill_md(&md).expect("parse_skill_md should succeed");
        assert_eq!(parsed.name, skill_name);
        assert_eq!(parsed.action_sketch.len(), 2);
    }
}
