//! `SKILL.md` emitter.
//!
//! Inverse of [`super::parser::parse_skill_md`]. Renders a [`Skill`]
//! into the canonical format: minimal YAML frontmatter, markdown body
//! with section + step markers in document order, and a single fenced
//! ` ```json action_sketch ` block carrying the executable plan as
//! pretty JSON.

#![allow(dead_code)]

use super::SKILL_SCHEMA_VERSION;
use super::types::{Skill, SkillFrontmatter};

const FRONTMATTER_DELIMITER: &str = "---";

/// Render a [`Skill`] to its canonical `SKILL.md` form. Always succeeds —
/// the input is fully owned in-memory state, and the JSON for the
/// fenced block round-trips through `serde_json::to_string_pretty` so
/// no fallible serializer is on the path.
pub fn emit_skill_md(skill: &Skill) -> String {
    let mut out = String::new();
    out.push_str(FRONTMATTER_DELIMITER);
    out.push('\n');
    let frontmatter = SkillFrontmatter {
        name: skill.name.clone(),
        description: skill.description.clone(),
        id: skill.id.clone(),
        version: skill.version,
        schema_version: SKILL_SCHEMA_VERSION,
        variables: skill.variables.clone(),
    };
    let yaml = serde_yaml::to_string(&frontmatter)
        .unwrap_or_else(|err| format!("# emitter: yaml encode failed: {err}\n"));
    out.push_str(&yaml);
    out.push_str(FRONTMATTER_DELIMITER);
    out.push_str("\n\n");

    if skill.sections.is_empty() {
        // No parsed sections — fall back to the raw body so callers
        // that hand-built a skill in memory can still emit a valid
        // markdown file.
        out.push_str(skill.body.trim_end());
        out.push('\n');
    } else {
        for section in &skill.sections {
            let prefix = "#".repeat(section.level as usize);
            out.push_str(&prefix);
            out.push(' ');
            out.push_str(&section.heading);
            out.push('\n');
            out.push_str(&format!("<!-- section: {} -->\n", section.id));
            for step_id in &section.step_ids {
                out.push_str(&format!("<!-- step: {step_id} -->\n"));
            }
            out.push('\n');
        }
    }

    let pretty =
        serde_json::to_string_pretty(&skill.action_sketch).unwrap_or_else(|_| "[]".to_string());
    out.push_str("```json action_sketch\n");
    out.push_str(&pretty);
    if !pretty.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("```\n");
    out
}
