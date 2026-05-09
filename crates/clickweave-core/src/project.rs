//! Phase 1 project envelope (D33).
//!
//! [`ProjectManifest`] replaces the legacy [`crate::Workflow`]-shaped
//! `<project>.json` envelope. It carries only the non-graph fields of
//! today's workflow record — `id`, `name`, optional `intent`, plus a
//! `schema_version` for future format changes — and is the value that
//! `open_project` / `save_project` round-trip on disk.
//!
//! Pre-1.0: legacy Workflow-shaped `<project>.json` files are **not**
//! auto-migrated. The loader detects them via the legacy graph keys
//! and surfaces a typed error so the UI can ask the user to start a
//! new project.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// On-disk schema version for [`ProjectManifest`]. Bump when the
/// serialized shape changes in a way that requires a reader migration.
pub const PROJECT_SCHEMA_VERSION: u32 = 1;

/// Slim project envelope persisted to `<project>.json`.
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectManifest {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub intent: Option<String>,
    pub schema_version: u32,
}

impl Default for ProjectManifest {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: "New Project".to_string(),
            intent: None,
            schema_version: PROJECT_SCHEMA_VERSION,
        }
    }
}
