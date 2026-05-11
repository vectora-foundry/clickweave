use crate::storage::{append_jsonl, format_timestamped_dirname, write_json_pretty};

use super::types::{WalkthroughAction, WalkthroughEvent, WalkthroughSessionMeta};

// --- Walkthrough storage ---

/// Manages on-disk storage for walkthrough session data and artifacts.
///
/// Directory layout:
/// ```text
/// walkthroughs/<timestamp>_<shortid>/
///   session.json
///   events.jsonl
///   actions.json
///   draft.json
///   artifacts/
/// ```
#[derive(Clone)]
pub struct WalkthroughStorage {
    base_path: std::path::PathBuf,
}

impl WalkthroughStorage {
    /// Create storage for a saved project.
    ///
    /// Path: `<project>/.clickweave/walkthroughs/`
    pub fn new(project_path: &std::path::Path) -> Self {
        Self {
            base_path: project_path.join(".clickweave").join("walkthroughs"),
        }
    }

    /// Create storage for an unsaved project (app data fallback).
    ///
    /// Path: `<app_data>/walkthroughs/`
    pub fn new_app_data(app_data_dir: &std::path::Path) -> Self {
        Self {
            base_path: app_data_dir.join("walkthroughs"),
        }
    }

    /// Create a directory for a new walkthrough session.
    /// Returns the full path to the session directory.
    pub fn create_session_dir(
        &self,
        session: &WalkthroughSessionMeta,
    ) -> anyhow::Result<std::path::PathBuf> {
        let dirname = format_timestamped_dirname(session.started_at, session.id);
        let session_dir = self.base_path.join(&dirname);
        std::fs::create_dir_all(session_dir.join("artifacts"))
            .map_err(|e| anyhow::anyhow!("Failed to create walkthrough session directory: {e}"))?;

        Ok(session_dir)
    }

    /// Save the session metadata to `session.json`.
    ///
    /// `events` and `actions` never flow through this record — they live in
    /// `events.jsonl` and `actions.json` and are written separately by
    /// [`append_event`](Self::append_event) / [`save_actions`](Self::save_actions).
    pub fn save_session(
        &self,
        session_dir: &std::path::Path,
        session: &WalkthroughSessionMeta,
    ) -> anyhow::Result<()> {
        write_json_pretty(&session_dir.join("session.json"), session)
    }

    /// Append a raw event to `events.jsonl`.
    pub fn append_event(
        &self,
        session_dir: &std::path::Path,
        event: &WalkthroughEvent,
    ) -> anyhow::Result<()> {
        append_jsonl(&session_dir.join("events.jsonl"), event)
    }

    /// Save the normalized actions to `actions.json`.
    pub fn save_actions(
        &self,
        session_dir: &std::path::Path,
        actions: &[WalkthroughAction],
    ) -> anyhow::Result<()> {
        write_json_pretty(&session_dir.join("actions.json"), actions)
    }

    /// Read all events from `events.jsonl` in a session directory.
    pub fn read_events(
        &self,
        session_dir: &std::path::Path,
    ) -> anyhow::Result<Vec<WalkthroughEvent>> {
        let path = session_dir.join("events.jsonl");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(anyhow::anyhow!("Failed to read events.jsonl: {e}")),
        };
        let mut events = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let event: WalkthroughEvent = serde_json::from_str(line)
                .map_err(|e| anyhow::anyhow!("Failed to parse event line: {e}"))?;
            events.push(event);
        }
        Ok(events)
    }

    pub fn base_path(&self) -> &std::path::Path {
        &self.base_path
    }
}
