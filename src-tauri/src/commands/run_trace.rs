//! Hydrates the latest run's trace from `events.jsonl` on disk so the
//! Trace canvas can show the most recent execution after an app reload.
//!
//! Disk format note: every `AgentEvent` variant is appended to the
//! execution-level `events.jsonl` by the agent forwarder, so the file
//! contains a chronological log of `step_completed`, `step_failed`,
//! `task_state_changed`, `world_model_changed`, `boundary_record_written`,
//! `goal_complete`, `error`, `consecutive_destructive_cap_hit`, etc.
//! `StepRecord` boundary records are also written separately by the
//! runner; those lines lack a `type` tag and are skipped here because
//! they're redundant with `boundary_record_written`.

use super::error::CommandError;
use super::types::resolve_storage;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::fs;
use std::io::{BufRead, BufReader};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize, Type)]
pub struct LoadLatestRunTraceRequest {
    pub project_path: Option<String>,
    pub project_name: String,
    pub project_id: String,
    pub store_traces: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum HydratedPhase {
    Exploring,
    Executing,
    Recovering,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct HydratedTraceStep {
    pub step_index: u32,
    pub tool_name: String,
    pub phase: HydratedPhase,
    pub body: String,
    pub failed: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct HydratedWorldModelDelta {
    pub step_index: u32,
    pub changed_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum HydratedMilestoneKind {
    SubgoalCompleted,
    RecoverySucceeded,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct HydratedTraceMilestone {
    pub step_index: u32,
    pub kind: HydratedMilestoneKind,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum HydratedTerminalKind {
    Complete,
    Stopped,
    Error,
    DisagreementCancelled,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct HydratedTerminalFrame {
    pub kind: HydratedTerminalKind,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Type)]
pub struct HydratedRunTrace {
    pub run_id: String,
    pub phase: HydratedPhase,
    pub active_subgoal: String,
    pub steps: Vec<HydratedTraceStep>,
    pub world_model_deltas: Vec<HydratedWorldModelDelta>,
    pub milestones: Vec<HydratedTraceMilestone>,
    pub terminal_frame: Option<HydratedTerminalFrame>,
}

/// Pick the most recently created execution directory. Names are
/// timestamp-prefixed (`YYYY-MM-DD_HH-MM-SS_<short-uuid>`) so a
/// lexicographic max gives the latest. Returns `None` when the runs
/// directory is missing or empty.
fn latest_execution_dir(base: &std::path::Path) -> Option<std::path::PathBuf> {
    let entries = fs::read_dir(base).ok()?;
    let mut best: Option<std::ffi::OsString> = None;
    for entry in entries.flatten() {
        if !entry.file_type().ok()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let take = match &best {
            None => true,
            Some(current) => name > *current,
        };
        if take {
            best = Some(name);
        }
    }
    best.map(|n| base.join(n))
}

fn parse_phase(value: &serde_json::Value) -> Option<HydratedPhase> {
    match value.as_str()? {
        "exploring" => Some(HydratedPhase::Exploring),
        "executing" => Some(HydratedPhase::Executing),
        "recovering" => Some(HydratedPhase::Recovering),
        _ => None,
    }
}

fn parse_milestone_kind(value: &serde_json::Value) -> Option<HydratedMilestoneKind> {
    match value.as_str()? {
        "subgoal_completed" => Some(HydratedMilestoneKind::SubgoalCompleted),
        "recovery_succeeded" => Some(HydratedMilestoneKind::RecoverySucceeded),
        _ => None,
    }
}

/// Whether the current terminal frame can still be upgraded by a
/// later, more authoritative on-disk record.
///
/// VLM disagreement runs write a terminal `boundary_record_written`
/// *before* `completion_disagreement_resolved`, so the boundary
/// fallback (`BoundaryFallback`) gets there first. We must let the
/// later resolution upgrade it. Explicit AgentEvent terminals
/// (`goal_complete`, `error`, `consecutive_destructive_cap_hit`) and
/// the resolution itself are `Hard` and cannot be overwritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalConfidence {
    BoundaryFallback,
    Hard,
}

#[derive(Default)]
struct TraceBuilder {
    phase: Option<HydratedPhase>,
    active_subgoal: String,
    steps: Vec<HydratedTraceStep>,
    deltas: Vec<HydratedWorldModelDelta>,
    milestones: Vec<HydratedTraceMilestone>,
    terminal: Option<HydratedTerminalFrame>,
    terminal_confidence: Option<TerminalConfidence>,
}

impl TraceBuilder {
    fn current_phase(&self) -> HydratedPhase {
        self.phase.clone().unwrap_or(HydratedPhase::Exploring)
    }

    /// Index to attach an upcoming `world_model_changed` delta to.
    ///
    /// Mirrors `nextTraceStepIndex` in `assistantSlice.ts`: live runs
    /// emit `world_model_changed` during a step's *observe* phase,
    /// before `step_completed` for that same step is dispatched. So at
    /// parse time the delta belongs to the step that hasn't been
    /// recorded yet (`max(steps.step_index) + 1`, or `0` when no steps
    /// exist).
    fn next_step_index(&self) -> u32 {
        match self.steps.iter().map(|s| s.step_index).max() {
            Some(max) => max + 1,
            None => 0,
        }
    }

    /// Set the terminal frame, respecting precedence:
    /// `Hard` cannot be overwritten; `BoundaryFallback` can be
    /// upgraded by any later setter.
    fn set_terminal(&mut self, frame: HydratedTerminalFrame, confidence: TerminalConfidence) {
        if matches!(self.terminal_confidence, Some(TerminalConfidence::Hard)) {
            return;
        }
        self.terminal = Some(frame);
        self.terminal_confidence = Some(confidence);
    }

    fn handle(&mut self, event: &serde_json::Value) {
        let Some(kind) = event.get("type").and_then(|v| v.as_str()) else {
            return;
        };
        match kind {
            "step_completed" => {
                let step_index = event
                    .get("step_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let tool_name = event
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let body = event
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let phase = self.current_phase();
                self.steps.push(HydratedTraceStep {
                    step_index,
                    tool_name,
                    phase,
                    body,
                    failed: false,
                });
            }
            "step_failed" => {
                let step_index = event
                    .get("step_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let tool_name = event
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let body = event
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let phase = self.current_phase();
                self.steps.push(HydratedTraceStep {
                    step_index,
                    tool_name,
                    phase,
                    body,
                    failed: true,
                });
            }
            "task_state_changed" => {
                if let Some(ts) = event.get("task_state") {
                    if let Some(p) = ts.get("phase").and_then(parse_phase) {
                        self.phase = Some(p);
                    }
                    if let Some(stack) = ts.get("subgoal_stack").and_then(|v| v.as_array()) {
                        self.active_subgoal = stack
                            .last()
                            .and_then(|s| s.get("text"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string();
                    }
                }
            }
            "world_model_changed" => {
                let changed_fields = event
                    .get("diff")
                    .and_then(|d| d.get("changed_fields"))
                    .and_then(|f| f.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                self.deltas.push(HydratedWorldModelDelta {
                    step_index: self.next_step_index(),
                    changed_fields,
                });
            }
            "boundary_record_written" => {
                let raw_kind = event.get("boundary_kind").and_then(|v| v.as_str());
                if raw_kind == Some("terminal") {
                    // `agent://stopped` reasons (max_steps_reached,
                    // max_errors_reached, approval_unavailable,
                    // loop_detected, etc.) are emitted directly by
                    // the Tauri layer and never reach the AgentEvent
                    // forwarder, so they don't appear as a tagged
                    // line in events.jsonl. The terminal boundary
                    // record is the only on-disk marker. Marked
                    // `BoundaryFallback` so that a later
                    // `completion_disagreement_resolved` (which
                    // arrives *after* the boundary in disagreement
                    // flows) can upgrade it to Complete or
                    // DisagreementCancelled.
                    let detail = event
                        .get("milestone_text")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "Run halted".to_string());
                    self.set_terminal(
                        HydratedTerminalFrame {
                            kind: HydratedTerminalKind::Stopped,
                            detail,
                        },
                        TerminalConfidence::BoundaryFallback,
                    );
                    return;
                }
                let Some(kind) = event.get("boundary_kind").and_then(parse_milestone_kind) else {
                    return;
                };
                let step_index = event
                    .get("step_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let text = event
                    .get("milestone_text")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| match kind {
                        HydratedMilestoneKind::SubgoalCompleted => "Subgoal completed".to_string(),
                        HydratedMilestoneKind::RecoverySucceeded => {
                            "Recovery succeeded".to_string()
                        }
                    });
                self.milestones.push(HydratedTraceMilestone {
                    step_index,
                    kind,
                    text,
                });
            }
            "goal_complete" => {
                let detail = event
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Goal completed.")
                    .to_string();
                self.set_terminal(
                    HydratedTerminalFrame {
                        kind: HydratedTerminalKind::Complete,
                        detail,
                    },
                    TerminalConfidence::Hard,
                );
            }
            "error" => {
                let detail = event
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.set_terminal(
                    HydratedTerminalFrame {
                        kind: HydratedTerminalKind::Error,
                        detail,
                    },
                    TerminalConfidence::Hard,
                );
            }
            "consecutive_destructive_cap_hit" => {
                let cap = event.get("cap").and_then(|v| v.as_u64()).unwrap_or(0);
                self.set_terminal(
                    HydratedTerminalFrame {
                        kind: HydratedTerminalKind::Stopped,
                        detail: format!("reached {cap} consecutive destructive actions"),
                    },
                    TerminalConfidence::Hard,
                );
            }
            "completion_disagreement_resolved" => {
                // Confirm and Cancel both halt the run, but neither
                // is followed by a persisted `goal_complete` /
                // `error` AgentEvent — the Tauri layer emits the
                // matching `agent://complete` or `agent://stopped`
                // directly. So this is the only on-disk marker for
                // either resolution. The flow writes the terminal
                // boundary record *before* this resolution, so a
                // BoundaryFallback Stopped is already set by the
                // time we reach here; `set_terminal(Hard)` lets us
                // upgrade past that fallback while still respecting
                // an explicit `goal_complete` / `error` (which
                // shouldn't happen alongside a resolution but is
                // defensive).
                let action = event.get("action").and_then(|v| v.as_str()).unwrap_or("");
                let agent_summary = event
                    .get("agent_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Goal completed.")
                    .to_string();
                let frame = match action {
                    "cancel" => HydratedTerminalFrame {
                        kind: HydratedTerminalKind::DisagreementCancelled,
                        detail: "user cancelled after VLM disagreement".to_string(),
                    },
                    _ => HydratedTerminalFrame {
                        kind: HydratedTerminalKind::Complete,
                        detail: agent_summary,
                    },
                };
                self.set_terminal(frame, TerminalConfidence::Hard);
            }
            _ => {}
        }
    }
}

fn hydrate_from_events_file(path: &std::path::Path) -> Result<TraceBuilder, CommandError> {
    let file =
        fs::File::open(path).map_err(|e| CommandError::io(format!("open events.jsonl: {e}")))?;
    let mut builder = TraceBuilder::default();
    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        builder.handle(&value);
    }
    Ok(builder)
}

#[tauri::command]
#[specta::specta]
pub async fn load_latest_run_trace(
    app: tauri::AppHandle,
    request: LoadLatestRunTraceRequest,
) -> Result<Option<HydratedRunTrace>, CommandError> {
    if !request.store_traces {
        return Ok(None);
    }
    let project_uuid: Uuid = request
        .project_id
        .parse()
        .map_err(|_| CommandError::validation("Invalid project ID"))?;
    let storage = resolve_storage(
        &app,
        &request.project_path,
        &request.project_name,
        project_uuid,
    );
    let base = storage.base_path();
    if !base.exists() {
        return Ok(None);
    }
    let Some(exec_dir) = latest_execution_dir(base) else {
        return Ok(None);
    };
    let events_path = exec_dir.join("events.jsonl");
    if !events_path.exists() {
        return Ok(None);
    }
    let builder = hydrate_from_events_file(&events_path)?;
    if builder.steps.is_empty() && builder.terminal.is_none() && builder.milestones.is_empty() {
        return Ok(None);
    }
    let run_id = format!(
        "hydrated-{}",
        exec_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
    );
    Ok(Some(HydratedRunTrace {
        run_id,
        phase: builder.phase.unwrap_or(HydratedPhase::Exploring),
        active_subgoal: builder.active_subgoal,
        steps: builder.steps,
        world_model_deltas: builder.deltas,
        milestones: builder.milestones,
        terminal_frame: builder.terminal,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_events(path: &std::path::Path, lines: &[&str]) {
        let mut f = std::fs::File::create(path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    #[test]
    fn hydrates_step_completed_into_a_step_node() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_events(
            &path,
            &[
                r#"{"type":"step_completed","step_index":1,"tool_name":"click","summary":"Clicked button"}"#,
            ],
        );
        let builder = hydrate_from_events_file(&path).unwrap();
        assert_eq!(builder.steps.len(), 1);
        let s = &builder.steps[0];
        assert_eq!(s.step_index, 1);
        assert_eq!(s.tool_name, "click");
        assert_eq!(s.body, "Clicked button");
        assert!(!s.failed);
    }

    #[test]
    fn marks_step_failed_with_red_flag_and_error_body() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_events(
            &path,
            &[
                r#"{"type":"step_failed","step_index":2,"tool_name":"click","error":"element not found"}"#,
            ],
        );
        let builder = hydrate_from_events_file(&path).unwrap();
        assert_eq!(builder.steps.len(), 1);
        let s = &builder.steps[0];
        assert!(s.failed);
        assert_eq!(s.body, "element not found");
    }

    #[test]
    fn task_state_change_updates_phase_and_subgoal() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_events(
            &path,
            &[
                r#"{"type":"task_state_changed","task_state":{"phase":"executing","subgoal_stack":[{"text":"Open settings"}]}}"#,
            ],
        );
        let builder = hydrate_from_events_file(&path).unwrap();
        assert!(matches!(builder.phase, Some(HydratedPhase::Executing)));
        assert_eq!(builder.active_subgoal, "Open settings");
    }

    #[test]
    fn boundary_record_appends_milestone() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_events(
            &path,
            &[
                r#"{"type":"boundary_record_written","boundary_kind":"subgoal_completed","step_index":3,"milestone_text":"Logged in"}"#,
            ],
        );
        let builder = hydrate_from_events_file(&path).unwrap();
        assert_eq!(builder.milestones.len(), 1);
        let m = &builder.milestones[0];
        assert!(matches!(m.kind, HydratedMilestoneKind::SubgoalCompleted));
        assert_eq!(m.step_index, 3);
        assert_eq!(m.text, "Logged in");
    }

    #[test]
    fn goal_complete_yields_complete_terminal_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_events(&path, &[r#"{"type":"goal_complete","summary":"Done."}"#]);
        let builder = hydrate_from_events_file(&path).unwrap();
        let f = builder.terminal.unwrap();
        assert!(matches!(f.kind, HydratedTerminalKind::Complete));
        assert_eq!(f.detail, "Done.");
    }

    #[test]
    fn world_model_delta_attributes_to_next_step_index() {
        // Live runs emit `world_model_changed` during a step's observe
        // phase, before the matching `step_completed` is dispatched.
        // The on-disk order mirrors that. The delta therefore belongs
        // to the upcoming step, not the last completed one.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_events(
            &path,
            &[
                r#"{"type":"step_completed","step_index":5,"tool_name":"click","summary":"x"}"#,
                r#"{"type":"world_model_changed","diff":{"changed_fields":["focused_app","modal_present"]}}"#,
                r#"{"type":"step_completed","step_index":6,"tool_name":"type_text","summary":"y"}"#,
            ],
        );
        let builder = hydrate_from_events_file(&path).unwrap();
        assert_eq!(builder.deltas.len(), 1);
        let d = &builder.deltas[0];
        assert_eq!(
            d.step_index, 6,
            "delta after step 5's step_completed and before step 6's step_completed should land on step 6 (the step whose observe produced it)",
        );
        assert_eq!(d.changed_fields, vec!["focused_app", "modal_present"]);
    }

    #[test]
    fn first_world_model_delta_with_no_prior_step_lands_on_step_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_events(
            &path,
            &[r#"{"type":"world_model_changed","diff":{"changed_fields":["focused_app"]}}"#],
        );
        let builder = hydrate_from_events_file(&path).unwrap();
        assert_eq!(builder.deltas.len(), 1);
        assert_eq!(builder.deltas[0].step_index, 0);
    }

    #[test]
    fn terminal_boundary_yields_stopped_frame_when_no_other_terminal_present() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_events(
            &path,
            &[
                r#"{"type":"boundary_record_written","boundary_kind":"terminal","step_index":29,"milestone_text":"reached max steps"}"#,
            ],
        );
        let builder = hydrate_from_events_file(&path).unwrap();
        let f = builder.terminal.unwrap();
        assert!(matches!(f.kind, HydratedTerminalKind::Stopped));
        assert_eq!(f.detail, "reached max steps");
    }

    #[test]
    fn terminal_boundary_does_not_overwrite_an_explicit_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_events(
            &path,
            &[
                r#"{"type":"goal_complete","summary":"Done."}"#,
                r#"{"type":"boundary_record_written","boundary_kind":"terminal","step_index":29,"milestone_text":"reached max steps"}"#,
            ],
        );
        let builder = hydrate_from_events_file(&path).unwrap();
        let f = builder.terminal.unwrap();
        assert!(matches!(f.kind, HydratedTerminalKind::Complete));
        assert_eq!(f.detail, "Done.");
    }

    #[test]
    fn confirmed_disagreement_after_terminal_boundary_upgrades_to_complete() {
        // Real flow: agent writes a terminal boundary before the user
        // resolves the disagreement. The boundary fallback must not
        // mask the later resolution.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_events(
            &path,
            &[
                r#"{"type":"boundary_record_written","boundary_kind":"terminal","step_index":12,"milestone_text":"awaiting confirmation"}"#,
                r#"{"type":"completion_disagreement_resolved","action":"confirm","agent_summary":"Login completed.","vlm_reasoning":"unsure"}"#,
            ],
        );
        let builder = hydrate_from_events_file(&path).unwrap();
        let f = builder.terminal.unwrap();
        assert!(matches!(f.kind, HydratedTerminalKind::Complete));
        assert_eq!(f.detail, "Login completed.");
    }

    #[test]
    fn cancelled_disagreement_after_terminal_boundary_upgrades_to_disagreement_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_events(
            &path,
            &[
                r#"{"type":"boundary_record_written","boundary_kind":"terminal","step_index":12,"milestone_text":"awaiting confirmation"}"#,
                r#"{"type":"completion_disagreement_resolved","action":"cancel","agent_summary":"x","vlm_reasoning":"y"}"#,
            ],
        );
        let builder = hydrate_from_events_file(&path).unwrap();
        let f = builder.terminal.unwrap();
        assert!(matches!(
            f.kind,
            HydratedTerminalKind::DisagreementCancelled
        ));
    }

    #[test]
    fn explicit_goal_complete_is_not_overwritten_by_later_resolution() {
        // Defensive: a `goal_complete` AgentEvent must remain Hard
        // even if a `completion_disagreement_resolved` arrives later.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_events(
            &path,
            &[
                r#"{"type":"goal_complete","summary":"All done."}"#,
                r#"{"type":"completion_disagreement_resolved","action":"cancel","agent_summary":"x","vlm_reasoning":"y"}"#,
            ],
        );
        let builder = hydrate_from_events_file(&path).unwrap();
        let f = builder.terminal.unwrap();
        assert!(matches!(f.kind, HydratedTerminalKind::Complete));
        assert_eq!(f.detail, "All done.");
    }

    #[test]
    fn confirmed_disagreement_resolution_yields_complete_terminal_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_events(
            &path,
            &[
                r#"{"type":"completion_disagreement_resolved","action":"confirm","agent_summary":"Login completed.","vlm_reasoning":"unsure"}"#,
            ],
        );
        let builder = hydrate_from_events_file(&path).unwrap();
        let f = builder.terminal.unwrap();
        assert!(matches!(f.kind, HydratedTerminalKind::Complete));
        assert_eq!(f.detail, "Login completed.");
    }

    #[test]
    fn cancelled_disagreement_resolution_yields_disagreement_cancelled_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_events(
            &path,
            &[
                r#"{"type":"completion_disagreement_resolved","action":"cancel","agent_summary":"x","vlm_reasoning":"y"}"#,
            ],
        );
        let builder = hydrate_from_events_file(&path).unwrap();
        let f = builder.terminal.unwrap();
        assert!(matches!(
            f.kind,
            HydratedTerminalKind::DisagreementCancelled
        ));
    }

    #[test]
    fn unknown_or_untagged_lines_are_silently_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        write_events(
            &path,
            &[
                r#"{"boundary_kind":"terminal","step_index":7}"#, // bare StepRecord
                r#"{"type":"unknown_variant","field":"x"}"#,
                r#"not even json"#,
                r#"{"type":"step_completed","step_index":1,"tool_name":"click","summary":"ok"}"#,
            ],
        );
        let builder = hydrate_from_events_file(&path).unwrap();
        assert_eq!(builder.steps.len(), 1);
        assert!(builder.terminal.is_none());
    }

    #[test]
    fn picks_lexicographically_latest_execution_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("2026-01-01_00-00-00_aaaa")).unwrap();
        std::fs::create_dir_all(tmp.path().join("2026-04-01_12-00-00_bbbb")).unwrap();
        std::fs::create_dir_all(tmp.path().join("2026-02-15_06-30-00_cccc")).unwrap();
        let latest = latest_execution_dir(tmp.path()).unwrap();
        assert!(latest.ends_with("2026-04-01_12-00-00_bbbb"));
    }
}
