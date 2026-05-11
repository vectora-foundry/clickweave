use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Discriminant carried in supervision and approval events to distinguish
/// skill-scoped pauses (with a known step position) from ad-hoc agent pauses
/// (identified only by run ID).
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SafetyScope {
    Skill {
        skill_id: String,
        section_id: String,
        step_id: String,
    },
    AdHoc {
        run_id: Uuid,
    },
}
