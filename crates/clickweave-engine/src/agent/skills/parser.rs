//! `SKILL.md` body-marker parser.
//!
//! The new format inverts the prior YAML-full-Skill round-trip: the
//! markdown body is canonical (heading sections + per-step
//! `<!-- step: -->` markers + a single fenced ` ```json action_sketch `
//! block carrying the executable plan), and the YAML frontmatter is the
//! intentionally-minimal [`SkillFrontmatter`] subset that Claude Code /
//! Codex / Gemini renderers can read verbatim.
//!
//! Public surface: [`parse_skill_md`] returns a [`Skill`] assembled
//! from frontmatter + body sections + extracted `action_sketch`. The
//! emitter side lives in [`super::emitter`] and round-trips with this
//! parser for any skill whose markers are well-formed.

#![allow(dead_code)]

use std::collections::{BTreeSet, HashSet};

use super::SKILL_SCHEMA_VERSION;
use super::types::{ActionSketchStep, Skill, SkillError, SkillFrontmatter, SkillSection};

const FRONTMATTER_DELIMITER: &str = "---";

/// Parse the canonical `SKILL.md` byte string into an in-memory [`Skill`].
///
/// Pipeline:
/// 1. Split `---`-delimited frontmatter; `serde_yaml::from_str::<SkillFrontmatter>`.
/// 2. Reject `schema_version > SKILL_SCHEMA_VERSION`.
/// 3. Walk markdown body for `##`/`###` headings + `<!-- section: -->`
///    + `<!-- step: -->` markers (slug fallback only when no marker appears).
///
/// 4. Locate the single fenced ` ```json action_sketch ` block and
///    deserialize its body as `Vec<ActionSketchStep>`.
/// 5. Validate marker correspondence, step / section uniqueness, and
///    variable resolution.
pub fn parse_skill_md(contents: &str) -> Result<Skill, SkillError> {
    let trimmed = contents.trim_start_matches(['\u{feff}', '\n', '\r']);
    if !trimmed.starts_with(FRONTMATTER_DELIMITER) {
        return Err(SkillError::MissingFrontmatterDelimiter(
            "expected leading `---` frontmatter delimiter".into(),
        ));
    }
    let after_open = trimmed[FRONTMATTER_DELIMITER.len()..].trim_start_matches(['\r', '\n']);
    let close_marker = format!("\n{FRONTMATTER_DELIMITER}");
    let close_idx = after_open.find(&close_marker).ok_or_else(|| {
        SkillError::MissingFrontmatterDelimiter("missing trailing `---` delimiter".into())
    })?;
    let yaml_text = &after_open[..close_idx];
    let after_close = &after_open[close_idx + close_marker.len()..];
    let body = after_close.trim_start_matches(['\r', '\n']).to_string();

    let frontmatter: SkillFrontmatter = serde_yaml::from_str(yaml_text)?;
    if frontmatter.schema_version > SKILL_SCHEMA_VERSION {
        return Err(SkillError::UnsupportedSchemaVersion {
            found: frontmatter.schema_version,
            max_supported: SKILL_SCHEMA_VERSION,
        });
    }

    let action_sketch_json = extract_action_sketch_fence(&body)?;
    let action_sketch: Vec<ActionSketchStep> =
        serde_json::from_str(&action_sketch_json).map_err(SkillError::MalformedActionSketchJson)?;

    // Strip the fenced action_sketch block from the prose body so the
    // round-trip emit→parse doesn't accumulate duplicate fences.
    let body_prose = strip_action_sketch_fence(&body);
    let (sections, marker_step_ids) = parse_sections_and_markers(&body_prose)?;

    let top_level_step_ids: Vec<String> = action_sketch
        .iter()
        .map(|step| top_level_step_id(step).to_string())
        .collect();

    // Step id uniqueness: combined markers + sketch (recursive). Section
    // markers are validated separately below.
    let mut seen = HashSet::new();
    for id in &marker_step_ids {
        if !seen.insert(id.clone()) {
            return Err(SkillError::DuplicateStepId(id.clone()));
        }
    }
    let mut sketch_ids = HashSet::new();
    collect_action_sketch_step_ids(&action_sketch, &mut sketch_ids, &mut seen)?;

    // Top-level marker correspondence: marker step_ids must match
    // top-level action_sketch step_ids exactly. Loop body step_ids are
    // deliberately excluded — they are not addressable via markers.
    //
    // Extractor- and walkthrough-generated skills currently emit the
    // executable plan without an accompanying prose body (the
    // mechanical prose generator that backfills markers lands in a
    // later subphase of the skill-only-shell rewrite). Tolerate that
    // shape by skipping the correspondence check when *no* markers
    // were authored — the action_sketch alone is canonical for
    // marker-less skills until prose is added.
    let marker_set: BTreeSet<String> = marker_step_ids.iter().cloned().collect();
    let top_level_set: BTreeSet<String> = top_level_step_ids.iter().cloned().collect();
    if !marker_set.is_empty() && marker_set != top_level_set {
        return Err(SkillError::StepMarkerMismatch {
            in_markers: marker_step_ids,
            in_action_sketch_top_level: top_level_step_ids,
        });
    }

    // Section id uniqueness.
    let mut section_seen = HashSet::new();
    for section in &sections {
        if !section_seen.insert(section.id.clone()) {
            return Err(SkillError::DuplicateSectionId(section.id.clone()));
        }
    }

    // Variable resolution: every `{{var}}` reference must resolve to a
    // declared variable name in frontmatter.
    let declared: HashSet<String> = frontmatter
        .variables
        .iter()
        .map(|v| v.name.clone())
        .collect();
    for var in find_variable_refs(&body_prose) {
        if !declared.contains(&var) {
            return Err(SkillError::UnresolvedVariableRef(var));
        }
    }

    // Hydrate Clickweave-internal metadata from the optional nested
    // `clickweave:` frontmatter block. When the block is absent (e.g.
    // a hand-imported Claude Code skill), the defaults give a usable
    // `Confirmed` / `ProjectLocal` skill that still functions through
    // the runtime's lifecycle gates.
    let meta = frontmatter.clickweave.unwrap_or_default();
    Ok(Skill {
        id: frontmatter.id.clone(),
        version: frontmatter.version,
        state: meta.state,
        scope: meta.scope,
        name: frontmatter.name.clone(),
        description: frontmatter.description.clone(),
        tags: meta.tags,
        subgoal_text: if meta.subgoal_text.is_empty() {
            frontmatter.name.clone()
        } else {
            meta.subgoal_text
        },
        subgoal_signature: meta.subgoal_signature,
        applicability: meta.applicability,
        parameter_schema: meta.parameter_schema,
        action_sketch,
        outputs: meta.outputs,
        outcome_predicate: meta.outcome_predicate,
        provenance: meta.provenance,
        stats: meta.stats,
        edited_by_user: meta.edited_by_user,
        created_at: meta.created_at,
        updated_at: meta.updated_at,
        produced_node_ids: meta.produced_node_ids,
        body: body_prose,
        schema_version: frontmatter.schema_version.max(SKILL_SCHEMA_VERSION.min(1)),
        variables: frontmatter.variables,
        sections,
        replay: None,
    })
}

/// Find the single `<heading_text>` for each `##`/`###` heading and
/// pair it with the immediately-following `<!-- section: <id> -->`
/// marker. Returns the parsed sections plus the document-order list of
/// `<!-- step: <id> -->` marker step_ids across the whole body.
fn parse_sections_and_markers(body: &str) -> Result<(Vec<SkillSection>, Vec<String>), SkillError> {
    let mut sections: Vec<SkillSection> = Vec::new();
    let mut all_step_ids: Vec<String> = Vec::new();
    let mut used_slugs: HashSet<String> = HashSet::new();
    let mut current: Option<SkillSection> = None;

    // Track both byte and UTF-16 code-unit positions simultaneously.
    // `byte_cursor` is used for `pick_section_id` (which scans the raw
    // &str slice); `utf16_cursor` is stored in `body_range` so that the
    // frontend can use the values directly with `String.prototype.slice`
    // (JS strings are UTF-16 encoded).
    let mut byte_cursor: usize = 0;
    let mut utf16_cursor: usize = 0;
    let body_utf16_len: usize = body.encode_utf16().count();
    let lines: Vec<&str> = body.split_inclusive('\n').collect();
    for line in &lines {
        let line_start_utf16 = utf16_cursor;
        byte_cursor += line.len();
        utf16_cursor += line.encode_utf16().count();

        let trimmed = line.trim_end_matches(['\n', '\r']);
        if let Some((level, heading)) = parse_heading(trimmed) {
            // Close out the previous section's body range.
            if let Some(mut prev) = current.take() {
                prev.body_range.1 = line_start_utf16;
                sections.push(prev);
            }

            // Section ID: prefer the explicit `<!-- section: -->`
            // marker on the next non-blank line; otherwise derive a
            // slug with collision-numeric dedup.
            let id = pick_section_id(&heading, body, byte_cursor, &mut used_slugs);
            current = Some(SkillSection {
                id,
                heading,
                level,
                step_ids: Vec::new(),
                body_range: (utf16_cursor, body_utf16_len),
            });
            continue;
        }

        // Step marker.
        if let Some(step_id) = parse_step_marker(trimmed) {
            if let Some(section) = current.as_mut() {
                section.step_ids.push(step_id.clone());
            }
            all_step_ids.push(step_id);
        }
    }
    if let Some(prev) = current.take() {
        sections.push(prev);
    }

    Ok((sections, all_step_ids))
}

fn parse_heading(line: &str) -> Option<(u8, String)> {
    let stripped = line.trim_start();
    if stripped.starts_with("###") && !stripped.starts_with("####") {
        return Some((3, stripped.trim_start_matches('#').trim().to_string()));
    }
    if stripped.starts_with("##") && !stripped.starts_with("###") {
        return Some((2, stripped.trim_start_matches('#').trim().to_string()));
    }
    None
}

fn parse_step_marker(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    let id = inner.strip_prefix("step:")?.trim();
    if id.is_empty() {
        return None;
    }
    Some(id.to_string())
}

fn parse_section_marker(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    let id = inner.strip_prefix("section:")?.trim();
    if id.is_empty() {
        return None;
    }
    Some(id.to_string())
}

/// Pick a section ID, preferring an inline `<!-- section: -->` marker
/// in the next non-blank line under the heading. Fallback: slug derived
/// from the heading text with numeric-suffix dedup on collisions.
fn pick_section_id(heading: &str, body: &str, start: usize, used: &mut HashSet<String>) -> String {
    if let Some(marker_id) = next_section_marker(body, start)
        && !marker_id.is_empty()
    {
        used.insert(marker_id.clone());
        return marker_id;
    }
    let base = slugify(heading);
    let chosen = if used.contains(&base) || base.is_empty() {
        let mut n = 2;
        loop {
            let candidate = if base.is_empty() {
                format!("section-{n}")
            } else {
                format!("{base}-{n}")
            };
            if !used.contains(&candidate) {
                break candidate;
            }
            n += 1;
        }
    } else {
        base
    };
    used.insert(chosen.clone());
    chosen
}

fn next_section_marker(body: &str, start: usize) -> Option<String> {
    body[start..]
        .split_inclusive('\n')
        .map(|line| line.trim_end_matches(['\n', '\r']))
        .filter(|line| !line.trim().is_empty())
        .find_map(parse_section_marker)
}

fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !out.is_empty() {
            out.push('-');
            last_was_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Remove the fenced ` ```json action_sketch ` block (and the
/// surrounding blank line) from the markdown body. Used to keep
/// `Skill::body` as pure prose, so the round-trip emit→parse doesn't
/// accumulate duplicate fences. Idempotent: bodies without a fence
/// pass through unchanged.
fn strip_action_sketch_fence(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut lines = body.split_inclusive('\n');
    while let Some(line) = lines.next() {
        let trimmed = line.trim_end_matches(['\n', '\r']).trim();
        if trimmed == "```json action_sketch" {
            // Drop the trailing blank line before the fence, if any.
            while out.ends_with("\n\n") {
                out.pop();
            }
            // Skip until the closing fence.
            for inner in lines.by_ref() {
                let inner_trim = inner.trim_end_matches(['\n', '\r']).trim();
                if inner_trim == "```" {
                    break;
                }
            }
            continue;
        }
        out.push_str(line);
    }
    out
}

/// Extract the contents of the single fenced ` ```json action_sketch `
/// block. Zero matches → `MissingActionSketchFence`. Two or more →
/// `MultipleActionSketchFences`.
fn extract_action_sketch_fence(body: &str) -> Result<String, SkillError> {
    let mut blocks: Vec<String> = Vec::new();
    let mut lines = body.split_inclusive('\n');
    while let Some(line) = lines.next() {
        let trimmed = line.trim_end_matches(['\n', '\r']).trim();
        if trimmed == "```json action_sketch" {
            let mut buf = String::new();
            let mut closed = false;
            for inner in lines.by_ref() {
                let inner_trim = inner.trim_end_matches(['\n', '\r']).trim();
                if inner_trim == "```" {
                    closed = true;
                    break;
                }
                buf.push_str(inner);
            }
            if !closed {
                return Err(SkillError::MissingActionSketchFence);
            }
            blocks.push(buf);
        }
    }
    match blocks.len() {
        0 => Err(SkillError::MissingActionSketchFence),
        1 => Ok(blocks.pop().unwrap()),
        _ => Err(SkillError::MultipleActionSketchFences),
    }
}

fn top_level_step_id(step: &ActionSketchStep) -> &str {
    match step {
        ActionSketchStep::ToolCall { step_id, .. } | ActionSketchStep::Loop { step_id, .. } => {
            step_id.as_str()
        }
    }
}

fn collect_action_sketch_step_ids(
    steps: &[ActionSketchStep],
    sketch_set: &mut HashSet<String>,
    combined: &mut HashSet<String>,
) -> Result<(), SkillError> {
    for step in steps {
        let id = top_level_step_id(step).to_string();
        if !sketch_set.insert(id.clone()) {
            return Err(SkillError::DuplicateStepId(id));
        }
        // Marker set already has its own ids — top-level ids may
        // legitimately collide with markers (that is the correspondence
        // rule). We only enforce uniqueness *within* the sketch on the
        // combined pass via Loop body recursion.
        if let ActionSketchStep::Loop { body, .. } = step {
            for inner in body {
                let inner_id = top_level_step_id(inner).to_string();
                if !sketch_set.insert(inner_id.clone()) {
                    return Err(SkillError::DuplicateStepId(inner_id));
                }
                if !combined.insert(inner_id.clone()) {
                    // body steps must not collide with marker step_ids
                    return Err(SkillError::DuplicateStepId(inner_id));
                }
                if let ActionSketchStep::Loop { .. } = inner {
                    // Recurse through nested loops.
                    collect_action_sketch_step_ids(
                        std::slice::from_ref(inner),
                        sketch_set,
                        combined,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn find_variable_refs(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if &bytes[i..i + 2] == b"{{"
            && let Some(end_rel) = body[i + 2..].find("}}")
        {
            let name = body[i + 2..i + 2 + end_rel].trim().to_string();
            if !name.is_empty() {
                out.push(name);
            }
            i += 2 + end_rel + 2;
            continue;
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::skills::emitter::emit_skill_md;
    use chrono::Utc;

    use crate::agent::skills::types::{
        ActionSketchStep, ApplicabilityHints, ApplicabilitySignature, ExpectedWorldModelDelta,
        LoopPredicate, OutcomePredicate, SkillFrontmatterVariable, SkillScope, SkillState,
        SkillStats, SubgoalSignature,
    };

    fn baseline_frontmatter(id: &str) -> SkillFrontmatter {
        SkillFrontmatter {
            name: format!("Skill {id}"),
            description: "Round-trip fixture skill".into(),
            id: id.into(),
            version: 1,
            schema_version: SKILL_SCHEMA_VERSION,
            variables: vec![],
            clickweave: None,
        }
    }

    fn tool_step(step_id: &str, tool: &str) -> ActionSketchStep {
        ActionSketchStep::ToolCall {
            step_id: step_id.into(),
            tool: tool.into(),
            args: serde_json::json!({}),
            captures_pre: vec![],
            captures: vec![],
            expected_world_model_delta: ExpectedWorldModelDelta::default(),
            requires_approval: None,
        }
    }

    fn skeleton_skill(
        id: &str,
        sections: Vec<SkillSection>,
        action_sketch: Vec<ActionSketchStep>,
        body: String,
        variables: Vec<SkillFrontmatterVariable>,
    ) -> Skill {
        let now = Utc::now();
        Skill {
            id: id.into(),
            version: 1,
            state: SkillState::Confirmed,
            scope: SkillScope::ProjectLocal,
            name: format!("Skill {id}"),
            description: "Round-trip fixture skill".into(),
            tags: vec![],
            subgoal_text: format!("Skill {id}"),
            subgoal_signature: SubgoalSignature(String::new()),
            applicability: ApplicabilityHints {
                apps: vec![],
                hosts: vec![],
                signature: ApplicabilitySignature(String::new()),
            },
            parameter_schema: vec![],
            action_sketch,
            outputs: vec![],
            outcome_predicate: OutcomePredicate::SubgoalCompleted {
                post_state_world_model_signature: None,
            },
            provenance: vec![],
            stats: SkillStats::default(),
            edited_by_user: false,
            created_at: now,
            updated_at: now,
            produced_node_ids: vec![],
            body,
            schema_version: SKILL_SCHEMA_VERSION,
            variables,
            sections,
            replay: None,
        }
    }

    fn assert_round_trip(skill: &Skill) {
        let emitted = emit_skill_md(skill);
        let parsed = parse_skill_md(&emitted).unwrap_or_else(|err| {
            panic!("emitted markdown failed to parse:\n{emitted}\nerror: {err:?}")
        });
        assert_eq!(parsed.id, skill.id);
        assert_eq!(parsed.version, skill.version);
        assert_eq!(parsed.action_sketch.len(), skill.action_sketch.len());
        assert_eq!(parsed.sections.len(), skill.sections.len());
        for (a, b) in parsed.sections.iter().zip(skill.sections.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.heading, b.heading);
            assert_eq!(a.step_ids, b.step_ids);
        }
    }

    #[test]
    fn roundtrip_simple() {
        let body =
            "## Open Mail\n<!-- section: sec_001 -->\n<!-- step: s_001 -->\n\nLaunch mail.\n"
                .to_string();
        let sections = vec![SkillSection {
            id: "sec_001".into(),
            heading: "Open Mail".into(),
            level: 2,
            step_ids: vec!["s_001".into()],
            body_range: (0, body.len()),
        }];
        let action_sketch = vec![tool_step("s_001", "launch_app")];
        let skill = skeleton_skill("skl_simple", sections, action_sketch, body, vec![]);
        assert_round_trip(&skill);
    }

    #[test]
    fn roundtrip_loop_with_body() {
        let body = "## Poll Until Ready\n<!-- section: sec_loop -->\n<!-- step: s_outer -->\n\nWait for the dialog.\n".to_string();
        let sections = vec![SkillSection {
            id: "sec_loop".into(),
            heading: "Poll Until Ready".into(),
            level: 2,
            step_ids: vec!["s_outer".into()],
            body_range: (0, body.len()),
        }];
        let action_sketch = vec![ActionSketchStep::Loop {
            step_id: "s_outer".into(),
            until: LoopPredicate::WorldModelDelta {
                expr: "world_model.changed".into(),
            },
            body: vec![
                tool_step("s_inner_1", "screenshot"),
                tool_step("s_inner_2", "wait"),
                tool_step("s_inner_3", "screenshot"),
            ],
            max_iterations: 5,
            iteration_delay_ms: 250,
        }];
        let skill = skeleton_skill("skl_loop", sections, action_sketch, body, vec![]);
        assert_round_trip(&skill);
    }

    #[test]
    fn roundtrip_multi_step_section() {
        let body = "## Compose New Email\n<!-- section: sec_compose -->\n<!-- step: s_a -->\n\nClick New.\n\n<!-- step: s_b -->\n\nFocus the field.\n\n<!-- step: s_c -->\n\nType the subject.\n".to_string();
        let sections = vec![SkillSection {
            id: "sec_compose".into(),
            heading: "Compose New Email".into(),
            level: 2,
            step_ids: vec!["s_a".into(), "s_b".into(), "s_c".into()],
            body_range: (0, body.len()),
        }];
        let action_sketch = vec![
            tool_step("s_a", "click"),
            tool_step("s_b", "focus"),
            tool_step("s_c", "type_text"),
        ];
        let skill = skeleton_skill("skl_multi", sections, action_sketch, body, vec![]);
        assert_round_trip(&skill);
    }

    #[test]
    fn parse_rejects_missing_action_sketch_fence() {
        let raw = "---\nname: Foo\ndescription: bar\nid: skl_x\nversion: 1\nschema_version: 1\n---\n\n## Body\n<!-- section: sec_only -->\n";
        let err = parse_skill_md(raw).unwrap_err();
        assert!(
            matches!(err, SkillError::MissingActionSketchFence),
            "got {err:?}"
        );
    }

    #[test]
    fn parse_rejects_marker_mismatch() {
        let raw = "---\nname: Foo\ndescription: bar\nid: skl_x\nversion: 1\nschema_version: 1\n---\n\n## Body\n<!-- section: sec_only -->\n<!-- step: s_marker_only -->\n\n```json action_sketch\n[ { \"type\": \"tool_call\", \"step_id\": \"s_sketch_only\", \"tool\": \"noop\", \"args\": {}, \"captures_pre\": [], \"captures\": [], \"expected_world_model_delta\": { \"changed_fields\": [] } } ]\n```\n";
        let err = parse_skill_md(raw).unwrap_err();
        assert!(
            matches!(err, SkillError::StepMarkerMismatch { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn parse_rejects_duplicate_step_id_across_sections() {
        let raw = "---\nname: Foo\ndescription: bar\nid: skl_x\nversion: 1\nschema_version: 1\n---\n\n## A\n<!-- section: sec_a -->\n<!-- step: s_dup -->\n\n## B\n<!-- section: sec_b -->\n<!-- step: s_dup -->\n\n```json action_sketch\n[ { \"type\": \"tool_call\", \"step_id\": \"s_dup\", \"tool\": \"noop\", \"args\": {}, \"captures_pre\": [], \"captures\": [], \"expected_world_model_delta\": { \"changed_fields\": [] } } ]\n```\n";
        let err = parse_skill_md(raw).unwrap_err();
        assert!(matches!(err, SkillError::DuplicateStepId(_)), "got {err:?}");
    }

    #[test]
    fn parse_rejects_unresolved_variable_ref() {
        let raw = "---\nname: Foo\ndescription: bar\nid: skl_x\nversion: 1\nschema_version: 1\n---\n\n## Body\n<!-- section: sec_only -->\n<!-- step: s_one -->\n\nType {{recipient}} into the field.\n\n```json action_sketch\n[ { \"type\": \"tool_call\", \"step_id\": \"s_one\", \"tool\": \"type_text\", \"args\": {}, \"captures_pre\": [], \"captures\": [], \"expected_world_model_delta\": { \"changed_fields\": [] } } ]\n```\n";
        let err = parse_skill_md(raw).unwrap_err();
        match err {
            SkillError::UnresolvedVariableRef(name) => assert_eq!(name, "recipient"),
            other => panic!("expected UnresolvedVariableRef, got {other:?}"),
        }
    }
}
