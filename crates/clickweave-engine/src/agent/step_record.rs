#![allow(dead_code)] // Phase 1: module wired to its own tests only; runtime consumers land in later phases.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::agent::task_state::TaskState;
use crate::agent::world_model::{
    CdpPageState, FocusedApp, ObservedElement, ScreenshotRef, UncertaintyScore, WindowRef,
    WorldModel,
};

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum BoundaryKind {
    Terminal,
    SubgoalCompleted,
    RecoverySucceeded,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AxSnapshotRef {
    pub snapshot_id: String,
    pub element_count: usize,
    pub captured_at_step: usize,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ElementSummary {
    pub total: usize,
    pub by_role: HashMap<String, usize>,
    pub by_source: HashMap<String, usize>,
}

/// Serializable projection of `WorldModel` (D15, remaining-risks review).
/// Drops `ax_tree_text` and the full element list. Keeps identity +
/// signature data only. Used in `StepRecord` for `events.jsonl` writes.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct WorldModelSnapshot {
    pub focused_app: Option<FocusedApp>,
    pub window_list: Option<Vec<WindowRef>>,
    pub cdp_page: Option<CdpPageState>,
    pub element_summary: Option<ElementSummary>,
    pub modal_present: Option<bool>,
    pub dialog_present: Option<bool>,
    pub last_screenshot: Option<ScreenshotRef>,
    pub last_native_ax_snapshot: Option<AxSnapshotRef>,
    pub uncertainty: UncertaintyScore,
}

impl WorldModelSnapshot {
    pub fn from_world_model(wm: &WorldModel) -> Self {
        Self {
            focused_app: wm.focused_app.as_ref().map(|f| f.value.clone()),
            window_list: wm.window_list.as_ref().map(|f| f.value.clone()),
            cdp_page: wm.cdp_page.as_ref().map(|f| f.value.clone()),
            element_summary: wm.elements.as_ref().map(|f| element_summary(&f.value)),
            modal_present: wm.modal_present.as_ref().map(|f| f.value),
            dialog_present: wm.dialog_present.as_ref().map(|f| f.value),
            last_screenshot: wm.last_screenshot.as_ref().map(|f| f.value.clone()),
            last_native_ax_snapshot: wm.last_native_ax_snapshot.as_ref().map(|f| AxSnapshotRef {
                snapshot_id: f.value.snapshot_id.clone(),
                element_count: f.value.element_count,
                captured_at_step: f.value.captured_at_step,
            }),
            uncertainty: wm.uncertainty.clone(),
        }
    }
}

fn element_summary(els: &[ObservedElement]) -> ElementSummary {
    let mut by_role = HashMap::new();
    let mut by_source = HashMap::new();
    for el in els {
        let (source, role) = match el {
            ObservedElement::Cdp(m) => ("cdp", m.role.clone()),
            ObservedElement::Ax(a) => ("ax", a.role.clone()),
            ObservedElement::Ocr(_) => ("ocr", "text".to_string()),
        };
        *by_role.entry(role).or_insert(0) += 1;
        *by_source.entry(source.to_string()).or_insert(0) += 1;
    }
    ElementSummary {
        total: els.len(),
        by_role,
        by_source,
    }
}

/// A single boundary record written to `events.jsonl`. Spec 1 only writes;
/// Spec 2 will read these for episodic memory.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StepRecord {
    pub step_index: usize,
    pub boundary_kind: BoundaryKind,
    pub world_model_snapshot: WorldModelSnapshot,
    pub task_state_snapshot: TaskState,
    pub action_taken: serde_json::Value, // full AgentAction serialized
    pub outcome: serde_json::Value,      // StepOutcome serialized
    pub timestamp: DateTime<Utc>,
}

impl StepRecord {
    /// Append this record as a single JSONL line at `path`. Creates the
    /// file if missing; parent directories must already exist. This is
    /// the low-level counterpart to `StateRunner::write_step_record` —
    /// useful when the caller already holds a concrete path (tests,
    /// offline tools) rather than a `RunStorage` handle.
    pub fn write_to_events_jsonl(&self, path: &std::path::Path) -> anyhow::Result<()> {
        clickweave_core::storage::append_jsonl(path, self)
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // Tests build WorldModel in stages for readability.
mod tests {
    use super::*;
    use crate::agent::world_model::{AxSnapshotData, Fresh, FreshnessSource, WorldModel};

    #[test]
    fn snapshot_drops_ax_tree_text_body() {
        let mut wm = WorldModel::default();
        wm.last_native_ax_snapshot = Some(Fresh {
            value: AxSnapshotData {
                snapshot_id: "a1g3".to_string(),
                element_count: 42,
                captured_at_step: 2,
                ax_tree_text: "A VERY LONG AX TREE BODY".to_string(),
            },
            written_at: 2,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        });

        let snap = WorldModelSnapshot::from_world_model(&wm);
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("\"a1g3\""));
        assert!(json.contains("\"element_count\":42"));
        assert!(
            !json.contains("A VERY LONG AX TREE BODY"),
            "ax_tree_text must not be serialized into WorldModelSnapshot"
        );
    }

    #[test]
    fn snapshot_has_element_summary_not_full_list() {
        use crate::agent::world_model::ObservedElement;
        use clickweave_core::cdp::CdpFindElementMatch;

        let mut wm = WorldModel::default();
        let mut els = Vec::new();
        for i in 0..10 {
            els.push(ObservedElement::Cdp(CdpFindElementMatch {
                uid: format!("d{}", i),
                role: if i < 5 {
                    "button".to_string()
                } else {
                    "link".to_string()
                },
                label: "x".to_string(),
                tag: "x".to_string(),
                disabled: false,
                parent_role: None,
                parent_name: None,
            }));
        }
        wm.elements = Some(Fresh {
            value: els,
            written_at: 1,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        });

        let snap = WorldModelSnapshot::from_world_model(&wm);
        let summary = snap.element_summary.as_ref().unwrap();
        assert_eq!(summary.total, 10);
        // Top roles should be present but the full list should not be.
        let json = serde_json::to_string(&snap).unwrap();
        assert!(!json.contains("\"uid\":\"d0\""));
        assert!(!json.contains("\"uid\":\"d9\""));
    }
}

#[cfg(all(test, feature = "specta"))]
mod specta_derive_tests {
    //! D17: `StepRecord` and its projection types are part of the Tauri
    //! `agent://boundary_record_written` event payload surface and must
    //! derive `specta::Type` so the bindings exporter picks them up.
    use super::*;
    use specta::{Generics, Type, TypeCollection};

    #[test]
    fn boundary_kind_derives_specta_type() {
        let _: specta::DataType =
            BoundaryKind::inline(&mut TypeCollection::default(), Generics::NONE);
    }

    #[test]
    fn ax_snapshot_ref_derives_specta_type() {
        let _: specta::DataType =
            AxSnapshotRef::inline(&mut TypeCollection::default(), Generics::NONE);
    }

    #[test]
    fn element_summary_derives_specta_type() {
        let _: specta::DataType =
            ElementSummary::inline(&mut TypeCollection::default(), Generics::NONE);
    }

    #[test]
    fn world_model_snapshot_derives_specta_type() {
        let _: specta::DataType =
            WorldModelSnapshot::inline(&mut TypeCollection::default(), Generics::NONE);
    }

    #[test]
    fn step_record_derives_specta_type() {
        let _: specta::DataType =
            StepRecord::inline(&mut TypeCollection::default(), Generics::NONE);
    }
}
