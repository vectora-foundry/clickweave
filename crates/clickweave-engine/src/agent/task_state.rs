#![allow(dead_code)] // Phase 1: module wired to its own tests only; runtime consumers land in later phases.

use serde::{Deserialize, Serialize};

use crate::agent::phase::Phase;

/// Capacity of the rolling hypothesis ring buffer.
/// Chosen so the state block renders in a bounded number of lines even
/// if the LLM over-produces hypotheses. Oldest are evicted first.
pub const HYPOTHESIS_RING_CAP: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SubgoalId(uuid::Uuid);

impl SubgoalId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for SubgoalId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Subgoal {
    pub id: SubgoalId,
    pub text: String,
    pub pushed_at_step: usize,
    pub parent: Option<SubgoalId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)] // `Pending*` prefix is part of the LLM-facing schema (D10).
pub enum WatchSlotName {
    PendingModal,
    PendingAuth,
    PendingFocusShift,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct WatchSlot {
    pub name: WatchSlotName,
    pub note: String,
    pub set_at_step: usize,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Hypothesis {
    pub text: String,
    pub recorded_at_step: usize,
    pub refuted: bool,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Milestone {
    pub subgoal_id: SubgoalId,
    pub text: String,
    pub summary: String,
    pub pushed_at_step: usize,
    pub completed_at_step: usize,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TaskState {
    pub goal: String,
    pub subgoal_stack: Vec<Subgoal>,
    pub watch_slots: Vec<WatchSlot>,
    pub hypotheses: Vec<Hypothesis>,
    pub phase: Phase,
    pub milestones: Vec<Milestone>,
}

impl TaskState {
    pub fn new(goal: String) -> Self {
        Self {
            goal,
            subgoal_stack: Vec::new(),
            watch_slots: Vec::new(),
            hypotheses: Vec::new(),
            phase: Phase::Exploring,
            milestones: Vec::new(),
        }
    }
}

/// Typed pseudo-tool mutations parsed from the LLM's `AgentTurn.mutations`.
/// These are harness-local; they never dispatch to MCP (D10).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskStateMutation {
    PushSubgoal {
        text: String,
    },
    /// Completes the top of the stack. No id field because the LLM
    /// never sees SubgoalIds — the flat stack in D4 makes "complete the
    /// active subgoal" unambiguous.
    CompleteSubgoal {
        summary: String,
    },
    SetWatchSlot {
        name: WatchSlotName,
        note: String,
    },
    ClearWatchSlot {
        name: WatchSlotName,
    },
    RecordHypothesis {
        text: String,
    },
    RefuteHypothesis {
        index: usize,
    },
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum MutationError {
    #[error("subgoal stack is empty")]
    StackEmpty,
    #[error("watch slot {name:?} not set")]
    WatchSlotNotSet { name: WatchSlotName },
    #[error("hypothesis index {index} out of range (len={len})")]
    HypothesisIndexOutOfRange { index: usize, len: usize },
}

impl TaskState {
    pub fn apply(&mut self, m: &TaskStateMutation, step_index: usize) -> Result<(), MutationError> {
        match m {
            TaskStateMutation::PushSubgoal { text } => {
                let parent = self.subgoal_stack.last().map(|s| s.id);
                self.subgoal_stack.push(Subgoal {
                    id: SubgoalId::new(),
                    text: text.clone(),
                    pushed_at_step: step_index,
                    parent,
                });
                Ok(())
            }
            TaskStateMutation::CompleteSubgoal { summary } => {
                let top = self.subgoal_stack.pop().ok_or(MutationError::StackEmpty)?;
                self.milestones.push(Milestone {
                    subgoal_id: top.id,
                    text: top.text,
                    summary: summary.clone(),
                    pushed_at_step: top.pushed_at_step,
                    completed_at_step: step_index,
                });
                Ok(())
            }
            TaskStateMutation::SetWatchSlot { name, note } => {
                // Idempotent by name — replacing any existing slot with that name.
                self.watch_slots.retain(|s| s.name != *name);
                self.watch_slots.push(WatchSlot {
                    name: *name,
                    note: note.clone(),
                    set_at_step: step_index,
                });
                Ok(())
            }
            TaskStateMutation::ClearWatchSlot { name } => {
                let had = self.watch_slots.iter().any(|s| s.name == *name);
                self.watch_slots.retain(|s| s.name != *name);
                if had {
                    Ok(())
                } else {
                    Err(MutationError::WatchSlotNotSet { name: *name })
                }
            }
            TaskStateMutation::RecordHypothesis { text } => {
                self.hypotheses.push(Hypothesis {
                    text: text.clone(),
                    recorded_at_step: step_index,
                    refuted: false,
                });
                // Evict oldest entries when over cap. Only one push per
                // call, so at most one eviction is ever needed.
                if self.hypotheses.len() > HYPOTHESIS_RING_CAP {
                    self.hypotheses.remove(0);
                }
                Ok(())
            }
            TaskStateMutation::RefuteHypothesis { index } => {
                let len = self.hypotheses.len();
                let h = self
                    .hypotheses
                    .get_mut(*index)
                    .ok_or(MutationError::HypothesisIndexOutOfRange { index: *index, len })?;
                h.refuted = true;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_state() -> TaskState {
        TaskState {
            goal: "test goal".to_string(),
            subgoal_stack: Vec::new(),
            watch_slots: Vec::new(),
            hypotheses: Vec::new(),
            phase: crate::agent::phase::Phase::Exploring,
            milestones: Vec::new(),
        }
    }

    #[test]
    fn push_subgoal_grows_stack() {
        let mut s = new_state();
        s.apply(
            &TaskStateMutation::PushSubgoal {
                text: "open login".to_string(),
            },
            1,
        )
        .unwrap();
        assert_eq!(s.subgoal_stack.len(), 1);
        assert_eq!(s.subgoal_stack[0].text, "open login");
        assert_eq!(s.subgoal_stack[0].pushed_at_step, 1);
    }

    #[test]
    fn push_subgoal_parent_is_previous_top() {
        let mut s = new_state();
        s.apply(
            &TaskStateMutation::PushSubgoal {
                text: "a".to_string(),
            },
            1,
        )
        .unwrap();
        let a_id = s.subgoal_stack[0].id;
        s.apply(
            &TaskStateMutation::PushSubgoal {
                text: "b".to_string(),
            },
            2,
        )
        .unwrap();
        assert_eq!(s.subgoal_stack[1].parent, Some(a_id));
    }

    #[test]
    fn complete_subgoal_pops_top_and_records_milestone() {
        // CompleteSubgoal does not take an id (P1.H1): the state block
        // never surfaces SubgoalIds to the LLM, so the LLM cannot author
        // them. The flat stack guarantees completion always targets the
        // top; the harness pops whichever subgoal is active.
        let mut s = new_state();
        s.apply(
            &TaskStateMutation::PushSubgoal {
                text: "a".to_string(),
            },
            1,
        )
        .unwrap();
        s.apply(
            &TaskStateMutation::CompleteSubgoal {
                summary: "done".to_string(),
            },
            3,
        )
        .unwrap();
        assert!(s.subgoal_stack.is_empty());
        assert_eq!(s.milestones.len(), 1);
        assert_eq!(s.milestones[0].summary, "done");
    }

    #[test]
    fn complete_subgoal_rejects_when_stack_empty() {
        let mut s = new_state();
        let err = s
            .apply(
                &TaskStateMutation::CompleteSubgoal {
                    summary: "x".to_string(),
                },
                1,
            )
            .unwrap_err();
        assert!(matches!(err, MutationError::StackEmpty));
    }

    #[test]
    fn set_watch_slot_adds_slot() {
        let mut s = new_state();
        s.apply(
            &TaskStateMutation::SetWatchSlot {
                name: WatchSlotName::PendingModal,
                note: "expect confirm dialog".to_string(),
            },
            4,
        )
        .unwrap();
        assert_eq!(s.watch_slots.len(), 1);
        assert_eq!(s.watch_slots[0].name, WatchSlotName::PendingModal);
    }

    #[test]
    fn set_watch_slot_replaces_existing_name() {
        let mut s = new_state();
        s.apply(
            &TaskStateMutation::SetWatchSlot {
                name: WatchSlotName::PendingModal,
                note: "first".to_string(),
            },
            4,
        )
        .unwrap();
        s.apply(
            &TaskStateMutation::SetWatchSlot {
                name: WatchSlotName::PendingModal,
                note: "second".to_string(),
            },
            5,
        )
        .unwrap();
        assert_eq!(s.watch_slots.len(), 1);
        assert_eq!(s.watch_slots[0].note, "second");
        assert_eq!(s.watch_slots[0].set_at_step, 5);
    }

    #[test]
    fn clear_watch_slot_removes_by_name() {
        let mut s = new_state();
        s.apply(
            &TaskStateMutation::SetWatchSlot {
                name: WatchSlotName::PendingAuth,
                note: "n".to_string(),
            },
            1,
        )
        .unwrap();
        s.apply(
            &TaskStateMutation::ClearWatchSlot {
                name: WatchSlotName::PendingAuth,
            },
            2,
        )
        .unwrap();
        assert!(s.watch_slots.is_empty());
    }

    #[test]
    fn record_hypothesis_appends() {
        let mut s = new_state();
        s.apply(
            &TaskStateMutation::RecordHypothesis {
                text: "this is a form".to_string(),
            },
            1,
        )
        .unwrap();
        assert_eq!(s.hypotheses.len(), 1);
        assert!(!s.hypotheses[0].refuted);
    }

    #[test]
    fn record_hypothesis_caps_at_ring_buffer_size() {
        let mut s = new_state();
        for i in 0..(HYPOTHESIS_RING_CAP + 5) {
            s.apply(
                &TaskStateMutation::RecordHypothesis {
                    text: format!("h{}", i),
                },
                i,
            )
            .unwrap();
        }
        assert_eq!(s.hypotheses.len(), HYPOTHESIS_RING_CAP);
        // Oldest entries evicted; the last one must be the most recent.
        assert!(
            s.hypotheses
                .last()
                .unwrap()
                .text
                .ends_with(&format!("{}", HYPOTHESIS_RING_CAP + 4))
        );
    }

    #[test]
    fn refute_hypothesis_rejects_out_of_range() {
        let mut s = new_state();
        let err = s
            .apply(&TaskStateMutation::RefuteHypothesis { index: 0 }, 1)
            .unwrap_err();
        assert!(matches!(
            err,
            MutationError::HypothesisIndexOutOfRange { .. }
        ));
    }

    #[test]
    fn refute_hypothesis_marks_existing() {
        let mut s = new_state();
        s.apply(
            &TaskStateMutation::RecordHypothesis {
                text: "h".to_string(),
            },
            1,
        )
        .unwrap();
        s.apply(&TaskStateMutation::RefuteHypothesis { index: 0 }, 2)
            .unwrap();
        assert!(s.hypotheses[0].refuted);
    }
}

#[cfg(all(test, feature = "specta"))]
mod specta_derive_tests {
    //! D17: `TaskState` and its transitive members are part of the Tauri
    //! `agent://task_state_changed` event payload surface and must derive
    //! `specta::Type` so the bindings exporter picks them up.
    use super::*;
    use specta::{Generics, Type, TypeCollection};

    #[test]
    fn task_state_derives_specta_type() {
        let _: specta::DataType = TaskState::inline(&mut TypeCollection::default(), Generics::NONE);
    }

    #[test]
    fn subgoal_derives_specta_type() {
        let _: specta::DataType = Subgoal::inline(&mut TypeCollection::default(), Generics::NONE);
    }

    #[test]
    fn subgoal_id_derives_specta_type() {
        let _: specta::DataType = SubgoalId::inline(&mut TypeCollection::default(), Generics::NONE);
    }

    #[test]
    fn watch_slot_derives_specta_type() {
        let _: specta::DataType = WatchSlot::inline(&mut TypeCollection::default(), Generics::NONE);
    }

    #[test]
    fn watch_slot_name_derives_specta_type() {
        let _: specta::DataType =
            WatchSlotName::inline(&mut TypeCollection::default(), Generics::NONE);
    }

    #[test]
    fn hypothesis_derives_specta_type() {
        let _: specta::DataType =
            Hypothesis::inline(&mut TypeCollection::default(), Generics::NONE);
    }

    #[test]
    fn milestone_derives_specta_type() {
        let _: specta::DataType = Milestone::inline(&mut TypeCollection::default(), Generics::NONE);
    }
}
