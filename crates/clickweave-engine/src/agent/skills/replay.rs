//! Inline replay engine for `InvokeSkill` actions (Spec 3 Phase 4).
//!
//! Public surface here is the pure logic the runner-side dispatch
//! helpers compose with: `SkillFrame` (the in-flight skill state),
//! `validate_parameters` (parameter-schema enforcement), the
//! `evaluate_loop_predicate` helper used by the `Loop` step expansion,
//! and the success-rate EMA update used at run-completion bookkeeping.
//!
//! `dispatch_skill` lives as a `StateRunner` method in `runner.rs`
//! (lookup + parameter validation + `SkillInvoked` emission). The
//! per-step expansion that consumes the resulting `SkillFrame` —
//! `run_skill_frame`, the shared `dispatch_tool_call_through_helper`
//! that routes both live and replayed `tool_call` steps through the
//! existing safety surface (permission policy, coordinate-primitive
//! guard, consecutive-destructive cap, approval gate), the `Loop` arm,
//! and the `<skill_in_progress>` LLM-fallback rendering — is staged for
//! the Phase 4 follow-up. The handoff report enumerates the resume
//! seam.
//!
//! # Replay outcomes
//!
//! Three outcomes drive the post-run success-rate update, each with a
//! different EMA weight:
//!
//! - `Clean`: every step's `expected_world_model_delta` matched and
//!   every `tool_call` dispatched successfully. Weight 1.0; updates
//!   `last_invoked_at`.
//! - `Adapted`: a divergence forced an LLM-fallback turn but the agent
//!   eventually closed the active subgoal. Weight 0.6; still updates
//!   `last_invoked_at`.
//! - `Abandoned`: the LLM-fallback turn emitted `agent_replan` (or the
//!   frame was dropped without subgoal closure). Weight 0.0; does not
//!   touch `last_invoked_at`.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::{
    Fidelity, LoopPredicate, ParameterSlot, Skill, SkillError, SkillId, SkillStats,
};

/// Wire-format version stamped into every `replay.json` sidecar.
/// Bumped on any breaking format change; loaders reject
/// `schema_version > REPLAY_SCHEMA_VERSION` with
/// `ReplayParseError::UnsupportedSchemaVersion`.
pub const REPLAY_SCHEMA_VERSION: u32 = 1;

/// FIFO cap on `ReplayStepBundle::repair_history` per D31. Successive
/// repairs evict the oldest entry once the cap is reached.
pub const REPAIR_HISTORY_CAP: usize = 16;

/// On-disk sidecar adjacent to `SKILL.md`. Carries per-step replay
/// metadata (intent, signals, postconditions, repair history) keyed on
/// the same `step_id` strings the action_sketch uses, plus the section
/// retirement chain that lets historical run records resolve through
/// chat-driven section splits.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReplayJson {
    pub skill_id: SkillId,
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub steps: HashMap<String, ReplayStepBundle>,
    #[serde(default)]
    pub section_history: Vec<SectionHistoryEntry>,
}

/// Per-step replay bundle. Phase 1 leaves `intent` / `action_kind` /
/// preconditions / postconditions / signals empty for freshly-recorded
/// skills — the batched intent-extraction pass that fills them lands in
/// Phase 2. The fallback in D32 substitutes destructive-tool annotations
/// when `requires_approval` is `None`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReplayStepBundle {
    pub intent: Option<String>,
    pub action_kind: Option<String>,
    pub preconditions: Option<serde_json::Value>,
    #[serde(default)]
    pub signals: Vec<Signal>,
    pub postconditions: Option<serde_json::Value>,
    pub requires_approval: Option<bool>,
    #[serde(default)]
    pub irreversible: bool,
    #[serde(default)]
    pub fidelity: Fidelity,
    #[serde(default)]
    pub repair_history: Vec<RepairHistoryEntry>,
}

/// Repair-tracker source for the deterministic runner. Ordered from
/// highest fidelity to lowest in the runtime fallback chain (CDP → AX
/// → image-crop → coords → keyboard).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Signal {
    AccessibilityLabel {
        role: String,
        label: String,
        parent_window: Option<String>,
    },
    CdpSelector {
        selector: String,
    },
    Keyboard {
        shortcut: String,
    },
    ImageCrop {
        path: String,
        bbox: [i32; 4],
    },
    Coords {
        x: i32,
        y: i32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairHistoryEntry {
    pub at: chrono::DateTime<chrono::Utc>,
    pub from_signal: Option<Signal>,
    pub to_signal: Signal,
    pub iteration: u32,
}

/// Recorded chain of section-id retirement so historical
/// `runs/<run_id>.json` records continue to resolve after a chat-driven
/// section split.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionHistoryEntry {
    pub retired: String,
    pub split_into: Vec<String>,
    pub at_version: u32,
    pub at: chrono::DateTime<chrono::Utc>,
}

/// Parse a `replay.json` byte string into [`ReplayJson`]. Rejects
/// future schema versions with `ReplayParseError::UnsupportedSchemaVersion`
/// so a loader running an older binary can never silently zero out a
/// sidecar it doesn't understand.
pub fn parse_replay_json(contents: &str) -> Result<ReplayJson, ReplayParseError> {
    let json: ReplayJson = serde_json::from_str(contents).map_err(ReplayParseError::Malformed)?;
    if json.schema_version > REPLAY_SCHEMA_VERSION {
        return Err(ReplayParseError::UnsupportedSchemaVersion {
            found: json.schema_version,
            max_supported: REPLAY_SCHEMA_VERSION,
        });
    }
    Ok(json)
}

/// Drop the oldest [`RepairHistoryEntry`] entries until the bundle is
/// at or below [`REPAIR_HISTORY_CAP`]. Idempotent.
pub fn enforce_repair_history_cap(bundle: &mut ReplayStepBundle) {
    while bundle.repair_history.len() > REPAIR_HISTORY_CAP {
        bundle.repair_history.remove(0);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayParseError {
    #[error("malformed replay.json: {0}")]
    Malformed(serde_json::Error),
    #[error("unsupported replay.json schema version {found}; max supported is {max_supported}")]
    UnsupportedSchemaVersion { found: u32, max_supported: u32 },
}

/// Exponential-moving-average smoothing constant for `success_rate`.
/// Picked low enough that a single divergent run can't crater a
/// well-confirmed skill, high enough that confidence updates within a
/// few invocations.
pub const SUCCESS_RATE_ALPHA: f32 = 0.2;

/// In-flight skill state held while the replay engine expands a single
/// `InvokeSkill` action. Cloned into `StateRunner::suspended_skill_frame`
/// when a divergence forces an LLM-fallback turn so the runner can
/// resume — or drop — the frame after the LLM responds.
#[derive(Debug, Clone)]
pub struct SkillFrame {
    pub skill: Arc<Skill>,
    /// Validated parameters (with `parameter_schema` defaults applied).
    pub params: Value,
    /// Bindings populated by `captures_pre` clauses + per-step
    /// `captures` clauses as the frame advances. Read by
    /// `substitution::substitute_value` at every subsequent step.
    pub captured: HashMap<String, Value>,
    /// Index of the next step in `skill.action_sketch` to execute.
    /// `next_step == skill.action_sketch.len()` means the sketch has
    /// been fully expanded and the frame is awaiting the outcome
    /// predicate evaluation.
    pub next_step: usize,
    /// Set true when a divergence pushed the run onto the LLM-fallback
    /// path. Cleared if the LLM successfully recovers and the frame
    /// resumes; persisted into stats bookkeeping at run completion.
    pub diverged: bool,
}

impl SkillFrame {
    pub fn new(skill: Arc<Skill>, params: Value) -> Self {
        Self {
            skill,
            params,
            captured: HashMap::new(),
            next_step: 0,
            diverged: false,
        }
    }

    /// Mark the frame as diverged for the LLM-fallback path. Returns a
    /// clone so the caller can stash the state without surrendering
    /// ownership of the live frame.
    pub fn clone_with_diverged(&self) -> Self {
        let mut clone = self.clone();
        clone.diverged = true;
        clone
    }

    /// True when every step in the action sketch has been expanded.
    pub fn is_exhausted(&self) -> bool {
        self.next_step >= self.skill.action_sketch.len()
    }
}

/// Outcome of a replay invocation, fed to
/// [`update_skill_stats_on_completion`] for the success-rate update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayOutcome {
    /// Every step matched expectations and dispatched cleanly.
    Clean,
    /// A divergence triggered the LLM-fallback path but the active
    /// subgoal closed (either through an LLM-emitted recovery action
    /// followed by `complete_subgoal`, or directly via
    /// `complete_subgoal`).
    Adapted,
    /// The LLM-fallback path emitted `agent_replan`, or the frame was
    /// dropped without subgoal closure.
    Abandoned,
}

impl ReplayOutcome {
    pub fn weight(self) -> f32 {
        match self {
            Self::Clean => 1.0,
            Self::Adapted => 0.6,
            Self::Abandoned => 0.0,
        }
    }

    pub fn updates_last_invoked_at(self) -> bool {
        !matches!(self, Self::Abandoned)
    }
}

/// Validate `parameters` against `schema` and return the value with
/// `default`s applied for any missing optional field. Required fields
/// without a default produce a `SkillError::InvalidParameters`. Unknown
/// fields are rejected so a parameter-schema typo can't silently slip
/// past as a no-op.
pub fn validate_parameters(
    parameters: &Value,
    schema: &[ParameterSlot],
) -> Result<Value, SkillError> {
    let supplied = match parameters {
        Value::Object(map) => map.clone(),
        Value::Null => serde_json::Map::new(),
        other => {
            return Err(SkillError::InvalidParameters(format!(
                "expected object, got {}",
                value_type_tag(other)
            )));
        }
    };

    let mut out = serde_json::Map::new();
    for slot in schema {
        match supplied.get(&slot.name) {
            Some(v) => {
                check_type(&slot.name, &slot.type_tag, v)?;
                if let Some(values) = &slot.enum_values
                    && let Some(s) = v.as_str()
                    && !values.iter().any(|allowed| allowed == s)
                {
                    return Err(SkillError::InvalidParameters(format!(
                        "field `{}`: value `{}` not in enum {:?}",
                        slot.name, s, values
                    )));
                }
                out.insert(slot.name.clone(), v.clone());
            }
            None => match &slot.default {
                Some(d) => {
                    out.insert(slot.name.clone(), d.clone());
                }
                None => {
                    return Err(SkillError::InvalidParameters(format!(
                        "field `{}` is required and has no default",
                        slot.name
                    )));
                }
            },
        }
    }

    for k in supplied.keys() {
        if !schema.iter().any(|s| &s.name == k) {
            return Err(SkillError::InvalidParameters(format!(
                "unknown field `{}`",
                k
            )));
        }
    }

    Ok(Value::Object(out))
}

fn check_type(field: &str, type_tag: &str, value: &Value) -> Result<(), SkillError> {
    let ok = match type_tag {
        "string" => value.is_string(),
        "number" | "integer" => value.is_number(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        // Unknown type tags pass — schema authors can extend the
        // vocabulary without tripping the validator.
        _ => true,
    };
    if !ok {
        return Err(SkillError::InvalidParameters(format!(
            "field `{}`: expected {}, got {}",
            field,
            type_tag,
            value_type_tag(value)
        )));
    }
    Ok(())
}

fn value_type_tag(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Evaluate a `LoopPredicate` against the iteration state. Used by
/// `Loop` expansion in `run_skill_frame` to decide whether the loop
/// body should fire again or whether the loop is satisfied.
pub fn evaluate_loop_predicate(
    predicate: &LoopPredicate,
    iterations_so_far: u32,
    pre_changed_fields: &[String],
    post_changed_fields: &[String],
) -> bool {
    match predicate {
        LoopPredicate::StepCountReached { count } => iterations_so_far >= *count,
        LoopPredicate::WorldModelDelta { expr } => {
            // Minimal predicate language: `world_model.<field>.changed`
            // returns true when the named field appears in the
            // post-iteration changed-fields set but not in the
            // pre-iteration set. The unnamed form `world_model.changed`
            // matches any non-empty post-delta. Any unrecognized
            // expression returns false so an opaque predicate doesn't
            // accidentally exit the loop early.
            if let Some(rest) = expr.strip_prefix("world_model.") {
                if let Some(field) = rest.strip_suffix(".changed") {
                    let appeared_now = post_changed_fields.iter().any(|f| f == field);
                    let was_already_present = pre_changed_fields.iter().any(|f| f == field);
                    return appeared_now && !was_already_present;
                }
                if rest == "changed" {
                    return !post_changed_fields.is_empty();
                }
            }
            false
        }
    }
}

/// Apply the success-rate EMA per the design's replay-outcomes
/// semantics. Returns the updated `SkillStats` so the caller can fold
/// the result back into the in-memory index and the on-disk file.
pub fn update_skill_stats_on_completion(
    stats: SkillStats,
    outcome: ReplayOutcome,
    now: DateTime<Utc>,
) -> SkillStats {
    let new_rate =
        SUCCESS_RATE_ALPHA * outcome.weight() + (1.0 - SUCCESS_RATE_ALPHA) * stats.success_rate;
    let last_invoked_at = if outcome.updates_last_invoked_at() {
        Some(now)
    } else {
        stats.last_invoked_at
    };
    SkillStats {
        success_rate: new_rate,
        last_invoked_at,
        ..stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::skills::types::{ParameterSlot, SkillStats};

    fn slot(name: &str, type_tag: &str, default: Option<Value>) -> ParameterSlot {
        ParameterSlot {
            name: name.to_string(),
            type_tag: type_tag.to_string(),
            description: None,
            default,
            enum_values: None,
        }
    }

    #[test]
    fn validate_parameters_applies_defaults_for_missing_optional() {
        let schema = vec![slot("k", "integer", Some(Value::from(7)))];
        let out = validate_parameters(&serde_json::json!({}), &schema).unwrap();
        assert_eq!(out, serde_json::json!({"k": 7}));
    }

    #[test]
    fn validate_parameters_rejects_required_without_default() {
        let schema = vec![slot("name", "string", None)];
        let err = validate_parameters(&serde_json::json!({}), &schema).unwrap_err();
        assert!(matches!(err, SkillError::InvalidParameters(_)));
    }

    #[test]
    fn validate_parameters_rejects_wrong_type() {
        let schema = vec![slot("count", "integer", None)];
        let err = validate_parameters(&serde_json::json!({"count": "not a number"}), &schema)
            .unwrap_err();
        assert!(matches!(err, SkillError::InvalidParameters(_)));
    }

    #[test]
    fn validate_parameters_rejects_unknown_field() {
        let schema = vec![slot("name", "string", None)];
        let err = validate_parameters(&serde_json::json!({"name": "x", "stray": true}), &schema)
            .unwrap_err();
        assert!(matches!(err, SkillError::InvalidParameters(_)));
    }

    #[test]
    fn validate_parameters_rejects_value_outside_enum() {
        let schema = vec![ParameterSlot {
            name: "scope".to_string(),
            type_tag: "string".to_string(),
            description: None,
            default: None,
            enum_values: Some(vec!["a".to_string(), "b".to_string()]),
        }];
        let err = validate_parameters(&serde_json::json!({"scope": "c"}), &schema).unwrap_err();
        assert!(matches!(err, SkillError::InvalidParameters(_)));
    }

    #[test]
    fn validate_parameters_accepts_null_when_schema_empty() {
        let out = validate_parameters(&Value::Null, &[]).unwrap();
        assert_eq!(out, serde_json::json!({}));
    }

    #[test]
    fn evaluate_loop_predicate_step_count() {
        let p = LoopPredicate::StepCountReached { count: 3 };
        assert!(!evaluate_loop_predicate(&p, 0, &[], &[]));
        assert!(!evaluate_loop_predicate(&p, 2, &[], &[]));
        assert!(evaluate_loop_predicate(&p, 3, &[], &[]));
        assert!(evaluate_loop_predicate(&p, 99, &[], &[]));
    }

    #[test]
    fn evaluate_loop_predicate_world_model_delta_named_field() {
        let p = LoopPredicate::WorldModelDelta {
            expr: "world_model.cdp_page.changed".to_string(),
        };
        assert!(!evaluate_loop_predicate(&p, 0, &[], &[]));
        assert!(evaluate_loop_predicate(
            &p,
            0,
            &[],
            &["cdp_page".to_string()]
        ));
        // Already present pre-iteration → not "newly changed".
        assert!(!evaluate_loop_predicate(
            &p,
            0,
            &["cdp_page".to_string()],
            &["cdp_page".to_string()]
        ));
    }

    #[test]
    fn evaluate_loop_predicate_world_model_delta_any() {
        let p = LoopPredicate::WorldModelDelta {
            expr: "world_model.changed".to_string(),
        };
        assert!(!evaluate_loop_predicate(&p, 0, &[], &[]));
        assert!(evaluate_loop_predicate(
            &p,
            0,
            &[],
            &["focused_app".to_string()]
        ));
    }

    #[test]
    fn evaluate_loop_predicate_unknown_expr_returns_false() {
        let p = LoopPredicate::WorldModelDelta {
            expr: "nope.unsupported".to_string(),
        };
        // Opaque predicate must not accidentally exit the loop.
        assert!(!evaluate_loop_predicate(
            &p,
            0,
            &[],
            &["focused_app".to_string()]
        ));
    }

    #[test]
    fn replay_outcome_weights_match_design() {
        assert_eq!(ReplayOutcome::Clean.weight(), 1.0);
        assert_eq!(ReplayOutcome::Adapted.weight(), 0.6);
        assert_eq!(ReplayOutcome::Abandoned.weight(), 0.0);
        assert!(ReplayOutcome::Clean.updates_last_invoked_at());
        assert!(ReplayOutcome::Adapted.updates_last_invoked_at());
        assert!(!ReplayOutcome::Abandoned.updates_last_invoked_at());
    }

    #[test]
    fn update_skill_stats_blends_success_rate() {
        let now = Utc::now();
        let stats = SkillStats {
            occurrence_count: 5,
            success_rate: 0.5,
            last_seen_at: None,
            last_invoked_at: None,
        };
        let updated = update_skill_stats_on_completion(stats, ReplayOutcome::Clean, now);
        // 0.2 * 1.0 + 0.8 * 0.5 = 0.6
        assert!((updated.success_rate - 0.6).abs() < 1e-6);
        assert_eq!(updated.last_invoked_at, Some(now));
    }

    #[test]
    fn update_skill_stats_abandoned_does_not_touch_last_invoked() {
        let now = Utc::now();
        let stats = SkillStats {
            occurrence_count: 5,
            success_rate: 1.0,
            last_seen_at: None,
            last_invoked_at: None,
        };
        let updated = update_skill_stats_on_completion(stats, ReplayOutcome::Abandoned, now);
        // 0.2 * 0.0 + 0.8 * 1.0 = 0.8
        assert!((updated.success_rate - 0.8).abs() < 1e-6);
        assert_eq!(updated.last_invoked_at, None);
    }

    #[test]
    fn replay_roundtrip_skeleton_replay() {
        let original = ReplayJson {
            skill_id: "skl_skeleton".into(),
            schema_version: REPLAY_SCHEMA_VERSION,
            steps: HashMap::new(),
            section_history: vec![],
        };
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded = parse_replay_json(&encoded).unwrap();
        assert_eq!(decoded.skill_id, original.skill_id);
        assert_eq!(decoded.schema_version, REPLAY_SCHEMA_VERSION);
        assert!(decoded.steps.is_empty());
        assert!(decoded.section_history.is_empty());
    }

    #[test]
    fn replay_roundtrip_populated_step_bundle() {
        let mut steps = HashMap::new();
        steps.insert(
            "s_002".to_string(),
            ReplayStepBundle {
                intent: Some("Open the compose window".into()),
                action_kind: Some("Click".into()),
                preconditions: None,
                signals: vec![
                    Signal::AccessibilityLabel {
                        role: "button".into(),
                        label: "New Message".into(),
                        parent_window: Some("Inbox".into()),
                    },
                    Signal::Keyboard {
                        shortcut: "cmd+n".into(),
                    },
                ],
                postconditions: None,
                requires_approval: Some(false),
                irreversible: false,
                fidelity: Fidelity::Solid,
                repair_history: vec![RepairHistoryEntry {
                    at: chrono::DateTime::parse_from_rfc3339("2026-05-09T12:34:56Z")
                        .unwrap()
                        .with_timezone(&chrono::Utc),
                    from_signal: None,
                    to_signal: Signal::Coords { x: 12, y: 34 },
                    iteration: 1,
                }],
            },
        );
        let original = ReplayJson {
            skill_id: "skl_populated".into(),
            schema_version: REPLAY_SCHEMA_VERSION,
            steps,
            section_history: vec![SectionHistoryEntry {
                retired: "sec_old".into(),
                split_into: vec!["sec_old".into(), "sec_old_2".into()],
                at_version: 4,
                at: chrono::DateTime::parse_from_rfc3339("2026-05-09T13:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            }],
        };
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded = parse_replay_json(&encoded).unwrap();
        let bundle = decoded.steps.get("s_002").expect("step bundle present");
        assert_eq!(bundle.signals.len(), 2);
        assert_eq!(bundle.repair_history.len(), 1);
        assert_eq!(bundle.fidelity, Fidelity::Solid);
        assert_eq!(decoded.section_history[0].retired, "sec_old");
    }

    #[test]
    fn replay_parse_rejects_unsupported_schema_version() {
        let raw = serde_json::json!({
            "skill_id": "skl_future",
            "schema_version": REPLAY_SCHEMA_VERSION + 1,
        });
        let err = parse_replay_json(&raw.to_string()).unwrap_err();
        match err {
            ReplayParseError::UnsupportedSchemaVersion {
                found,
                max_supported,
            } => {
                assert_eq!(found, REPLAY_SCHEMA_VERSION + 1);
                assert_eq!(max_supported, REPLAY_SCHEMA_VERSION);
            }
            other => panic!("expected UnsupportedSchemaVersion, got {other:?}"),
        }
    }

    #[test]
    fn enforce_repair_history_cap_evicts_oldest() {
        let mut bundle = ReplayStepBundle::default();
        for i in 0..(REPAIR_HISTORY_CAP as u32 + 4) {
            bundle.repair_history.push(RepairHistoryEntry {
                at: chrono::DateTime::parse_from_rfc3339("2026-05-09T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                from_signal: None,
                to_signal: Signal::Coords { x: i as i32, y: 0 },
                iteration: i,
            });
        }
        enforce_repair_history_cap(&mut bundle);
        assert_eq!(bundle.repair_history.len(), REPAIR_HISTORY_CAP);
        // Oldest entries (iterations 0..=3) were evicted.
        let first_kept = bundle.repair_history.first().unwrap();
        assert_eq!(first_kept.iteration, 4);
    }
}
