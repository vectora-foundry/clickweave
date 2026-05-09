//! Exact-string-match provenance tracing.
//!
//! Walks a recorded action sequence forward, replacing literals in each
//! step's arguments with `{{captured.*}}` references where a prior tool
//! result contained the same value. Sub-4-character literals are
//! suppressed (too noisy). Most-recent producer wins on multiple
//! matches. AX-uid arguments are captured as `captures_pre` clauses
//! keyed on `(role, name, parent_name)` so replay can re-resolve the
//! uid against the live AX tree.

#![allow(dead_code)]

use std::collections::HashMap;

use serde_json::Value;

use super::types::{
    ActionSketchStep, AxDescriptorMatch, CaptureClause, CaptureSource, ExpectedWorldModelDelta,
    RecordedStep,
};
use crate::agent::step_record::WorldModelSnapshot;

const MIN_LITERAL_LEN: usize = 4;

pub fn build_action_sketch(action_sequence: &[RecordedStep]) -> Vec<ActionSketchStep> {
    // Two passes: producer steps need capture clauses *attached* to them,
    // but the consumer step that triggers the capture is discovered later
    // in the trace. Pass 1 walks forward and records all clauses keyed by
    // producer step. Pass 2 emits the action sketch with each step's
    // captures inlined.
    let mut prior_results: Vec<(usize, Value)> = Vec::new();
    let mut captures_by_step: HashMap<usize, Vec<CaptureClause>> = HashMap::new();
    let mut rewritten_args_by_step: Vec<Value> = Vec::with_capacity(action_sequence.len());

    for (idx, step) in action_sequence.iter().enumerate() {
        let mut rewritten_args = step.arguments.clone();

        for (path, literal_value) in walk_string_and_number_literals(&rewritten_args) {
            if literal_value
                .as_str()
                .is_some_and(|s| s.len() < MIN_LITERAL_LEN)
            {
                continue;
            }
            if let Some((producer_idx, jsonpath)) =
                find_match_in_prior_results(&literal_value, &prior_results)
            {
                let capture_name = synthesize_capture_name(producer_idx, &jsonpath);
                let already_recorded = captures_by_step
                    .get(&producer_idx)
                    .is_some_and(|clauses| clauses.iter().any(|c| c.name == capture_name));
                if !already_recorded {
                    captures_by_step
                        .entry(producer_idx)
                        .or_default()
                        .push(CaptureClause {
                            name: capture_name.clone(),
                            source: CaptureSource::ToolResult {
                                jsonpath: jsonpath.clone(),
                            },
                        });
                }
                replace_at_path(
                    &mut rewritten_args,
                    &path,
                    &format!("{{{{captured.{capture_name}}}}}"),
                );
            }
        }

        rewrite_ax_uids_to_captures_pre(&mut rewritten_args, step, idx, &mut captures_by_step);

        let result_value: Value = serde_json::from_str(&step.result_text).unwrap_or(Value::Null);
        prior_results.push((idx, result_value));
        rewritten_args_by_step.push(rewritten_args);
    }

    let mut steps: Vec<ActionSketchStep> = Vec::with_capacity(action_sequence.len());
    for (idx, rewritten_args) in rewritten_args_by_step.into_iter().enumerate() {
        let captures_for_step = captures_by_step.remove(&idx).unwrap_or_default();
        let (captures_pre, captures) = split_pre_post_captures(captures_for_step);
        let step = &action_sequence[idx];
        steps.push(ActionSketchStep::ToolCall {
            step_id: format!("s_{:06}", idx),
            tool: step.tool_name.clone(),
            args: rewritten_args,
            captures_pre,
            captures,
            expected_world_model_delta: derive_expected_delta(
                &step.world_model_pre,
                &step.world_model_post,
            ),
        });
    }

    super::loop_folding::fold_polling_loops(&mut steps);
    steps
}

fn walk_string_and_number_literals(value: &Value) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    walk_inner(value, String::new(), &mut out);
    out
}

fn walk_inner(value: &Value, path: String, out: &mut Vec<(String, Value)>) {
    match value {
        Value::String(_) | Value::Number(_) => {
            out.push((path, value.clone()));
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                walk_inner(item, format!("{path}[{i}]"), out);
            }
        }
        Value::Object(map) => {
            for (k, v) in map {
                let next = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                walk_inner(v, next, out);
            }
        }
        _ => {}
    }
}

fn find_match_in_prior_results(
    literal: &Value,
    prior: &[(usize, Value)],
) -> Option<(usize, String)> {
    for (idx, value) in prior.iter().rev() {
        if let Some(jsonpath) = find_value_path(literal, value, String::new()) {
            return Some((*idx, jsonpath));
        }
    }
    None
}

fn find_value_path(needle: &Value, haystack: &Value, path: String) -> Option<String> {
    if needle == haystack {
        return Some(if path.is_empty() {
            "$".into()
        } else {
            format!("$.{path}")
        });
    }
    match haystack {
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                if let Some(p) = find_value_path(needle, item, format!("{path}[{i}]")) {
                    return Some(p);
                }
            }
            None
        }
        Value::Object(map) => {
            for (k, v) in map {
                let next = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                if let Some(p) = find_value_path(needle, v, next) {
                    return Some(p);
                }
            }
            None
        }
        _ => None,
    }
}

fn synthesize_capture_name(producer_idx: usize, jsonpath: &str) -> String {
    let last_segment = jsonpath
        .rsplit(['.', '[', ']', '$'])
        .find(|s| !s.is_empty())
        .unwrap_or("value");
    format!("step{producer_idx}_{last_segment}")
}

fn replace_at_path(value: &mut Value, path: &str, replacement: &str) {
    let segments = parse_path(path);
    if segments.is_empty() {
        return;
    }
    let mut current: &mut Value = value;
    for seg in &segments[..segments.len() - 1] {
        current = match seg {
            PathSeg::Key(k) => match current.get_mut(k) {
                Some(next) => next,
                None => return,
            },
            PathSeg::Index(i) => match current.get_mut(*i) {
                Some(next) => next,
                None => return,
            },
        };
    }
    if let Some(last) = segments.last() {
        match last {
            PathSeg::Key(k) => {
                if let Some(obj) = current.as_object_mut() {
                    obj.insert(k.clone(), Value::String(replacement.to_string()));
                }
            }
            PathSeg::Index(i) => {
                if let Some(arr) = current.as_array_mut()
                    && *i < arr.len()
                {
                    arr[*i] = Value::String(replacement.to_string());
                }
            }
        }
    }
}

#[derive(Debug)]
enum PathSeg {
    Key(String),
    Index(usize),
}

fn parse_path(path: &str) -> Vec<PathSeg> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_index = false;
    for ch in path.chars() {
        match ch {
            '.' if !in_index => {
                if !buf.is_empty() {
                    out.push(PathSeg::Key(std::mem::take(&mut buf)));
                }
            }
            '[' => {
                if !buf.is_empty() {
                    out.push(PathSeg::Key(std::mem::take(&mut buf)));
                }
                in_index = true;
            }
            ']' => {
                if let Ok(i) = buf.parse::<usize>() {
                    out.push(PathSeg::Index(i));
                }
                buf.clear();
                in_index = false;
            }
            c => buf.push(c),
        }
    }
    if !buf.is_empty() {
        out.push(PathSeg::Key(buf));
    }
    out
}

fn rewrite_ax_uids_to_captures_pre(
    args: &mut Value,
    step: &RecordedStep,
    step_idx: usize,
    captures_by_step: &mut HashMap<usize, Vec<CaptureClause>>,
) {
    let Some(uid) = args.get("uid").and_then(|v| v.as_str()).map(str::to_string) else {
        return;
    };
    if let Some(descriptor) = lookup_ax_descriptor_for_uid(&step.world_model_pre, &uid) {
        let capture_name = format!("step{step_idx}_uid");
        captures_by_step
            .entry(step_idx)
            .or_default()
            .push(CaptureClause {
                name: capture_name.clone(),
                source: CaptureSource::AxDescriptor { descriptor },
            });
        if let Some(obj) = args.as_object_mut() {
            obj.insert(
                "uid".into(),
                Value::String(format!("{{{{captured.{capture_name}}}}}")),
            );
        }
    }
}

fn lookup_ax_descriptor_for_uid(
    _world_model_pre: &WorldModelSnapshot,
    _uid: &str,
) -> Option<AxDescriptorMatch> {
    // The AX tree body lives in `WorldModel::last_native_ax_snapshot`,
    // not on the projected `WorldModelSnapshot` Spec 1 ships in
    // boundary records. Phase 3 wires the extractor to the live
    // `WorldModel` (not just the snapshot) so it can call
    // `enrich_ax_descriptor`. For the Phase 1 unit-test surface the
    // fallback returns `None`, which leaves the uid as a literal that
    // the LLM-fallback path handles on replay divergence.
    None
}

fn derive_expected_delta(
    pre: &WorldModelSnapshot,
    post: &WorldModelSnapshot,
) -> ExpectedWorldModelDelta {
    // Snapshots derive Serialize but not PartialEq, so compare via
    // canonical JSON. The frontend-facing field names mirror the
    // runtime `WorldModelDiff::changed_fields` taxonomy
    // (`element_summary` is rendered as `elements`).
    let pre_json = serde_json::to_value(pre).unwrap_or(Value::Null);
    let post_json = serde_json::to_value(post).unwrap_or(Value::Null);
    const FIELDS: &[(&str, &str)] = &[
        ("focused_app", "focused_app"),
        ("window_list", "window_list"),
        ("cdp_page", "cdp_page"),
        ("element_summary", "elements"),
        ("modal_present", "modal_present"),
        ("dialog_present", "dialog_present"),
        ("last_screenshot", "last_screenshot"),
        ("last_native_ax_snapshot", "last_native_ax_snapshot"),
        ("uncertainty", "uncertainty"),
    ];
    let mut changed = Vec::new();
    for (snapshot_field, public_name) in FIELDS {
        if pre_json.get(snapshot_field) != post_json.get(snapshot_field) {
            changed.push(public_name.to_string());
        }
    }
    ExpectedWorldModelDelta {
        changed_fields: changed,
    }
}

fn split_pre_post_captures(
    clauses: Vec<CaptureClause>,
) -> (Vec<CaptureClause>, Vec<CaptureClause>) {
    let mut pre = Vec::new();
    let mut post = Vec::new();
    for clause in clauses {
        match clause.source {
            CaptureSource::AxDescriptor { .. } => pre.push(clause),
            _ => post.push(clause),
        }
    }
    (pre, post)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> WorldModelSnapshot {
        WorldModelSnapshot::default()
    }

    fn step(tool: &str, args: Value, result: &str) -> RecordedStep {
        RecordedStep {
            tool_name: tool.into(),
            arguments: args,
            result_text: result.into(),
            world_model_pre: snap(),
            world_model_post: snap(),
        }
    }

    #[test]
    fn cross_step_match_threads_captured_reference() {
        let seq = vec![
            step(
                "list_items",
                serde_json::json!({}),
                r#"{"first":"abcdef-token"}"#,
            ),
            step(
                "use_item",
                serde_json::json!({ "id": "abcdef-token" }),
                "{}",
            ),
        ];
        let sketch = build_action_sketch(&seq);
        let ActionSketchStep::ToolCall { args, .. } = &sketch[1] else {
            panic!("expected ToolCall, got {:?}", sketch[1]);
        };
        assert_eq!(args["id"], serde_json::json!("{{captured.step0_first}}"));
    }

    #[test]
    fn short_literals_under_minimum_length_are_not_captured() {
        let seq = vec![
            step("list_items", serde_json::json!({}), r#"{"first":"abc"}"#),
            step("use_item", serde_json::json!({ "id": "abc" }), "{}"),
        ];
        let sketch = build_action_sketch(&seq);
        let ActionSketchStep::ToolCall { args, .. } = &sketch[1] else {
            panic!("expected ToolCall, got {:?}", sketch[1]);
        };
        assert_eq!(args["id"], serde_json::json!("abc"));
    }

    #[test]
    fn most_recent_producer_wins_on_multiple_matches() {
        let seq = vec![
            step("first", serde_json::json!({}), r#"{"value":"abcdefgh"}"#),
            step("second", serde_json::json!({}), r#"{"value":"abcdefgh"}"#),
            step("use", serde_json::json!({ "id": "abcdefgh" }), "{}"),
        ];
        let sketch = build_action_sketch(&seq);
        let ActionSketchStep::ToolCall { args, .. } = &sketch[2] else {
            panic!("expected ToolCall, got {:?}", sketch[2]);
        };
        let val = args["id"].as_str().unwrap();
        assert!(
            val.starts_with("{{captured.step1_"),
            "expected reference to step 1, got {val}"
        );
    }

    #[test]
    fn first_step_records_capture_clause_for_threaded_value() {
        let seq = vec![
            step(
                "list_items",
                serde_json::json!({}),
                r#"{"first":"abcdef-token"}"#,
            ),
            step(
                "use_item",
                serde_json::json!({ "id": "abcdef-token" }),
                "{}",
            ),
        ];
        let sketch = build_action_sketch(&seq);
        let ActionSketchStep::ToolCall { captures, .. } = &sketch[0] else {
            panic!("expected ToolCall, got {:?}", sketch[0]);
        };
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].name, "step0_first");
    }
}
