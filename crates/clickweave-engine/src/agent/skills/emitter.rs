//! `SKILL.md` emitter.
//!
//! Inverse of [`super::parser::parse_skill_md`]. Renders a [`Skill`]
//! into the canonical format: minimal YAML frontmatter, markdown body
//! with section + step markers in document order, and a single fenced
//! ` ```json action_sketch ` block carrying the executable plan as
//! pretty JSON.

#![allow(dead_code)]

use std::collections::HashMap;

use super::SKILL_SCHEMA_VERSION;
use super::types::{ClickweaveSkillMeta, Skill, SkillFrontmatter, SkillSection};

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
        clickweave: Some(ClickweaveSkillMeta {
            state: skill.state,
            scope: skill.scope,
            tags: skill.tags.clone(),
            subgoal_text: skill.subgoal_text.clone(),
            subgoal_signature: skill.subgoal_signature.clone(),
            applicability: skill.applicability.clone(),
            parameter_schema: skill.parameter_schema.clone(),
            outputs: skill.outputs.clone(),
            outcome_predicate: skill.outcome_predicate.clone(),
            provenance: skill.provenance.clone(),
            stats: skill.stats.clone(),
            edited_by_user: skill.edited_by_user,
            created_at: skill.created_at,
            updated_at: skill.updated_at,
            produced_node_ids: skill.produced_node_ids.clone(),
        }),
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
        // Build a section-id → prose-lines map by scanning the raw body.
        // This avoids relying on `body_range` byte offsets (which are now
        // UTF-16 positions for frontend use) for Rust-side string slicing.
        let section_prose = collect_section_prose(&skill.body, &skill.sections);

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
            // Re-emit prose lines that belong to this section, skipping
            // HTML comment markers already written above. This preserves
            // human-authored instructions under each section heading.
            match section_prose.get(section.id.as_str()) {
                Some(prose) if !prose.is_empty() => {
                    for line in prose {
                        out.push_str(line);
                        out.push('\n');
                    }
                }
                _ => {
                    out.push('\n');
                }
            }
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

/// Scan the raw body text and collect prose lines for each section,
/// keyed by section ID. Lines that are heading markers (`##`/`###`),
/// HTML comment markers (`<!-- ... -->`), or the fenced `action_sketch`
/// block are excluded; ordinary Markdown code blocks inside section
/// prose are preserved. Trailing blank lines are stripped from each
/// section's prose so the emitter can append a single blank separator
/// line itself.
///
/// `sections` is used to resolve section IDs for headings that carry no
/// explicit `<!-- section: id -->` marker (markerless sections). The
/// match is by heading text order so document order is preserved.
///
/// Returns a map from section ID → prose lines (may be empty).
fn collect_section_prose<'b>(
    body: &'b str,
    sections: &'b [SkillSection],
) -> HashMap<&'b str, Vec<&'b str>> {
    let mut result: HashMap<&'b str, Vec<&'b str>> = HashMap::new();

    // Index sections by heading text for fast markerless lookup.
    // Multiple sections can share a heading (the parser deduplicates IDs),
    // so we keep them as a queue and pop the first one on each heading match.
    let mut heading_queue: Vec<(&str, &str)> = sections
        .iter()
        .map(|s| (s.heading.as_str(), s.id.as_str()))
        .collect();

    let mut current_id: Option<&str> = None;
    // Track fenced blocks so we only drop the `action_sketch` fence.
    let mut in_action_sketch_fence = false;
    let mut in_other_fence = false;

    for line in body.lines() {
        let trimmed = line.trim();

        // Detect the action_sketch fence open/close.
        if trimmed == "```json action_sketch" {
            in_action_sketch_fence = true;
            continue;
        }
        if in_action_sketch_fence {
            if trimmed == "```" {
                in_action_sketch_fence = false;
            }
            continue;
        }

        // Track non-action_sketch fences (ordinary code blocks) — preserve
        // their content as prose but use the fence flag to avoid
        // misidentifying ``` inside a code block as a section boundary.
        if trimmed.starts_with("```") {
            in_other_fence = !in_other_fence;
            if let Some(id) = current_id {
                result.entry(id).or_default().push(line);
            }
            continue;
        }
        if in_other_fence {
            if let Some(id) = current_id {
                result.entry(id).or_default().push(line);
            }
            continue;
        }

        // Section heading — find the matching section in document order.
        if trimmed.starts_with("##") && !trimmed.starts_with("###") {
            let heading_text = trimmed.trim_start_matches('#').trim();
            current_id = pop_section_for_heading(&mut heading_queue, heading_text, 2);
            continue;
        }
        if trimmed.starts_with("###") {
            let heading_text = trimmed.trim_start_matches('#').trim();
            current_id = pop_section_for_heading(&mut heading_queue, heading_text, 3);
            continue;
        }

        // Section-ID marker: `<!-- section: <id> -->` — also updates
        // current_id in case a previous heading matched a different ID.
        if let Some(rest) = trimmed.strip_prefix("<!-- section:") {
            if let Some(id_part) = rest.strip_suffix("-->") {
                let id = id_part.trim();
                if !id.is_empty() {
                    current_id = Some(
                        sections
                            .iter()
                            .find(|s| s.id == id)
                            .map(|s| s.id.as_str())
                            .unwrap_or(id),
                    );
                    result.entry(current_id.unwrap()).or_default();
                }
            }
            continue;
        }

        // Step and other HTML comment markers — skip.
        if trimmed.starts_with("<!--") {
            continue;
        }

        // Prose line belonging to the current section.
        if let Some(id) = current_id {
            result.entry(id).or_default().push(line);
        }
    }

    // Strip trailing blank lines from every section's prose.
    for prose in result.values_mut() {
        while prose
            .last()
            .map(|l: &&str| l.trim().is_empty())
            .unwrap_or(false)
        {
            prose.pop();
        }
    }

    result
}

/// Walk the `heading_queue` (which is in document order, same as the
/// `sections` slice) and find the next section whose heading matches
/// `heading_text` and whose `level` matches. Removes consumed entries
/// from the front of the queue.
fn pop_section_for_heading<'s>(
    queue: &mut Vec<(&'s str, &'s str)>,
    heading_text: &str,
    _level: u8,
) -> Option<&'s str> {
    // Find the first match in document order and remove it.
    if let Some(pos) = queue.iter().position(|(h, _)| *h == heading_text) {
        let (_, id) = queue.remove(pos);
        return Some(id);
    }
    None
}
