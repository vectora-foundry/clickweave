use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{Artifact, ArtifactKind, NodeRun, NodeVerdict, RunStatus, TraceEvent, TraceLevel};

/// Returns the current time as milliseconds since the Unix epoch.
pub fn now_millis() -> u64 {
    Utc::now().timestamp_millis() as u64
}

/// Formats a timestamped directory name as `YYYY-MM-DD_HH-MM-SS_<short_uuid>`.
pub fn format_timestamped_dirname(started_at_ms: u64, id: Uuid) -> String {
    let ts = i64::try_from(started_at_ms).ok();
    let dt = ts
        .and_then(DateTime::from_timestamp_millis)
        .unwrap_or_default();
    let short_id = &id.to_string()[..12];
    format!("{}_{short_id}", dt.format("%Y-%m-%d_%H-%M-%S"))
}

/// Serializes a value as pretty-printed JSON and writes it to a file.
pub fn write_json_pretty<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(value).context("Failed to serialize JSON")?;
    std::fs::write(path, json).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

/// Appends a single JSON line to a file (newline-delimited JSON).
pub fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut line = serde_json::to_string(value).context("Failed to serialize JSONL entry")?;
    line.push('\n');

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .context("Failed to open JSONL file")?;
    file.write_all(line.as_bytes())
        .context("Failed to write JSONL entry")?;

    Ok(())
}

/// Sanitizes a name for use as a directory component.
///
/// Lowercases, replaces non-alphanumeric chars with `-`, collapses consecutive
/// dashes, and trims leading/trailing dashes.
pub fn sanitize_name(name: &str) -> String {
    crate::sanitize::sanitize_for_path(name)
}

/// Formats an execution directory name as `YYYY-MM-DD_HH-MM-SS_<short_uuid>`.
fn format_execution_dirname(started_at_ms: u64, run_id: Uuid) -> String {
    format_timestamped_dirname(started_at_ms, run_id)
}

/// Manages on-disk storage for node run artifacts and trace data.
///
/// Directory layout:
/// ```text
/// runs/<workflow_dir>/
///   <YYYY-MM-DD_HH-MM-SS_shortid>/   ← one per workflow execution
///     <sanitized_node_name>/           ← one per node
///       run.json
///       events.jsonl
///       artifacts/
/// ```
pub struct RunStorage {
    /// Points to `runs/<workflow_dir>/`
    base_path: PathBuf,
    /// The current execution directory name (set by `begin_execution`).
    execution_dir: Option<String>,
}

impl RunStorage {
    pub fn execution_dir_name(&self) -> Option<&str> {
        self.execution_dir.as_deref()
    }

    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// Path for the workflow's decision cache file.
    ///
    /// Stored as `decisions.json` at the workflow run directory level
    /// (sibling to per-execution directories).
    pub fn cache_path(&self) -> PathBuf {
        self.base_path.join("decisions.json")
    }

    /// Path to the agent decision cache (workflow-level, persists across runs).
    pub fn agent_cache_path(&self) -> PathBuf {
        self.base_path.join("agent_cache.json")
    }

    /// Path to the variant index file (workflow-level, not per-execution).
    pub fn variant_index_path(&self) -> PathBuf {
        self.base_path.join("variant_index.jsonl")
    }

    /// Append a serializable agent event to the execution-level events.jsonl.
    pub fn append_agent_event(&self, event: &impl Serialize) -> Result<()> {
        let execution_dir = self
            .execution_dir
            .as_ref()
            .context("begin_execution() must be called before append_agent_event()")?;
        let events_path = self.base_path.join(execution_dir).join("events.jsonl");
        append_jsonl(&events_path, event)
    }

    /// Create storage for a saved project.
    ///
    /// Path: `<project>/.clickweave/runs/<sanitized_workflow_name>/`
    pub fn new(project_path: &Path, workflow_name: &str) -> Self {
        Self {
            base_path: project_path
                .join(".clickweave")
                .join("runs")
                .join(sanitize_name(workflow_name)),
            execution_dir: None,
        }
    }

    /// Create storage for an unsaved project (app data fallback).
    ///
    /// Path: `<app_data>/runs/<sanitized_workflow_name>_<short_uuid>/`
    pub fn new_app_data(app_data_dir: &Path, workflow_name: &str, workflow_id: Uuid) -> Self {
        let short_id = &workflow_id.to_string()[..8];
        let dir_name = format!("{}_{short_id}", sanitize_name(workflow_name));
        Self {
            base_path: app_data_dir.join("runs").join(dir_name),
            execution_dir: None,
        }
    }

    /// Start a new workflow execution. Creates a shared datetime directory
    /// under the workflow dir and stores it for subsequent `create_run` calls.
    ///
    /// Returns the execution directory name.
    pub fn begin_execution(&mut self) -> Result<String> {
        let exec_id = Uuid::new_v4();
        let started_at = Self::now_millis();
        let dirname = format_execution_dirname(started_at, exec_id);
        std::fs::create_dir_all(self.base_path.join(&dirname))
            .context("Failed to create execution directory")?;
        self.execution_dir = Some(dirname.clone());
        Ok(dirname)
    }

    /// Deterministic path for a run whose metadata is known.
    pub fn run_dir(&self, run: &NodeRun) -> PathBuf {
        self.base_path
            .join(&run.execution_dir)
            .join(sanitize_name(&run.node_name))
    }

    /// Finds an existing run directory.
    ///
    /// If `execution_dir` is provided, looks up the path directly (O(1)).
    /// Otherwise falls back to scanning all execution dirs (O(n)).
    pub fn find_run_dir(
        &self,
        node_name: &str,
        run_id: Uuid,
        execution_dir: Option<&str>,
    ) -> Result<PathBuf> {
        let sanitized = sanitize_name(node_name);

        // Fast path: direct lookup when execution_dir is known
        if let Some(exec_dir) = execution_dir {
            let candidate = self.base_path.join(exec_dir).join(&sanitized);
            if candidate.join("run.json").exists() {
                return Ok(candidate);
            }
        }

        // Slow path: scan all execution dirs
        if !self.base_path.exists() {
            anyhow::bail!("Workflow directory not found: {}", self.base_path.display());
        }

        let run_str = run_id.to_string();

        for exec_entry in
            std::fs::read_dir(&self.base_path).context("Failed to read workflow directory")?
        {
            let exec_entry = exec_entry?;
            if !exec_entry.file_type()?.is_dir() {
                continue;
            }
            let node_dir = exec_entry.path().join(&sanitized);
            if !node_dir.exists() {
                continue;
            }
            // Check if run.json in this node dir matches the run_id
            let run_json = node_dir.join("run.json");
            if run_json.exists()
                && let Ok(data) = std::fs::read_to_string(&run_json)
                && let Ok(run) = serde_json::from_str::<NodeRun>(&data)
                && run.run_id == run_id
            {
                return Ok(node_dir);
            }
        }

        anyhow::bail!(
            "Run directory not found for node '{}' run {}",
            node_name,
            run_str
        )
    }

    pub fn now_millis() -> u64 {
        now_millis()
    }

    /// Create a new run for a node within the current execution.
    ///
    /// Requires `begin_execution()` to have been called first.
    pub fn create_run(
        &self,
        node_id: Uuid,
        node_name: &str,
        trace_level: TraceLevel,
    ) -> Result<NodeRun> {
        let execution_dir = self
            .execution_dir
            .as_ref()
            .context("begin_execution() must be called before create_run()")?
            .clone();

        let run_id = Uuid::new_v4();
        let started_at = Self::now_millis();
        let sanitized = sanitize_name(node_name);
        let dir = self.base_path.join(&execution_dir).join(&sanitized);

        // Guard against node name collisions within the same execution.
        // Allow re-creation for the same node_id (loop re-execution) but
        // reject collisions from different nodes whose names sanitize identically.
        let run_json_path = dir.join("run.json");
        if run_json_path.exists() {
            let existing: NodeRun = Self::read_run_json(&run_json_path)?;
            if existing.node_id != node_id {
                anyhow::bail!(
                    "Node directory '{}' already exists in execution '{}' — \
                     two nodes may have names that sanitize identically",
                    sanitized,
                    execution_dir
                );
            }
        }

        std::fs::create_dir_all(dir.join("artifacts")).context("Failed to create run directory")?;

        let run = NodeRun {
            run_id,
            node_id,
            node_name: node_name.to_string(),
            execution_dir,
            started_at,
            ended_at: None,
            status: RunStatus::Ok,
            trace_level,
            events: Vec::new(),
            artifacts: Vec::new(),
            observed_summary: None,
        };

        self.save_run(&run)?;
        Ok(run)
    }

    pub fn save_run(&self, run: &NodeRun) -> Result<()> {
        let dir = self.run_dir(run);
        std::fs::create_dir_all(&dir).context("Failed to create run directory")?;

        let json = serde_json::to_string_pretty(run).context("Failed to serialize run")?;
        std::fs::write(dir.join("run.json"), json).context("Failed to write run.json")?;
        Ok(())
    }

    pub fn append_event(&self, run: &NodeRun, event: &TraceEvent) -> Result<()> {
        let events_path = self.run_dir(run).join("events.jsonl");
        Self::write_event_line(&events_path, event)
    }

    /// Append a trace event to the execution-level `events.jsonl`.
    ///
    /// Used for events that aren't scoped to a specific node run (e.g.,
    /// control flow evaluations like `branch_evaluated`, `loop_iteration`).
    pub fn append_execution_event(&self, event: &TraceEvent) -> Result<()> {
        let execution_dir = self
            .execution_dir
            .as_ref()
            .context("begin_execution() must be called before append_execution_event()")?;
        let events_path = self.base_path.join(execution_dir).join("events.jsonl");
        Self::write_event_line(&events_path, event)
    }

    fn write_event_line(path: &Path, event: &TraceEvent) -> Result<()> {
        append_jsonl(path, event)
    }

    pub fn save_artifact(
        &self,
        run: &NodeRun,
        kind: ArtifactKind,
        filename: &str,
        data: &[u8],
        metadata: Value,
    ) -> Result<Artifact> {
        let artifact_path = self.run_dir(run).join("artifacts").join(filename);

        std::fs::write(&artifact_path, data).context("Failed to write artifact")?;

        let artifact = Artifact {
            artifact_id: Uuid::new_v4(),
            kind,
            path: artifact_path.to_string_lossy().to_string(),
            metadata,
            overlays: Vec::new(),
        };

        Ok(artifact)
    }

    /// Save a check verdict to the node's run directory as `verdict.json`.
    pub fn save_node_verdict(&self, verdict: &NodeVerdict) -> Result<()> {
        let execution_dir = self
            .execution_dir
            .as_ref()
            .context("begin_execution() must be called before save_node_verdict()")?;

        let sanitized = sanitize_name(&verdict.node_name);
        let path = self
            .base_path
            .join(execution_dir)
            .join(sanitized)
            .join("verdict.json");
        let json = serde_json::to_string_pretty(verdict).context("Failed to serialize verdict")?;
        std::fs::write(&path, json).context("Failed to write verdict.json")?;
        Ok(())
    }

    /// Load all runs for a node by scanning execution directories.
    pub fn load_runs_for_node(&self, node_name: &str) -> Result<Vec<NodeRun>> {
        if !self.base_path.exists() {
            return Ok(Vec::new());
        }

        let sanitized = sanitize_name(node_name);
        let mut runs = Vec::new();

        for exec_entry in
            std::fs::read_dir(&self.base_path).context("Failed to read workflow directory")?
        {
            let exec_entry = exec_entry?;
            if !exec_entry.file_type()?.is_dir() {
                continue;
            }
            let node_dir = exec_entry.path().join(&sanitized);
            let run_json = node_dir.join("run.json");
            if run_json.exists() {
                runs.push(Self::read_run_json(&run_json)?);
            }
        }

        runs.sort_by_key(|r| r.started_at);
        Ok(runs)
    }

    pub fn load_run(
        &self,
        node_name: &str,
        run_id: Uuid,
        execution_dir: Option<&str>,
    ) -> Result<NodeRun> {
        let run_dir = self.find_run_dir(node_name, run_id, execution_dir)?;
        Self::read_run_json(&run_dir.join("run.json"))
    }

    fn read_run_json(path: &Path) -> Result<NodeRun> {
        let data = std::fs::read_to_string(path).context("Failed to read run.json")?;
        serde_json::from_str(&data).context("Failed to parse run.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_storage() -> (RunStorage, PathBuf) {
        let dir = std::env::temp_dir()
            .join("clickweave_test")
            .join(Uuid::new_v4().to_string());
        let storage = RunStorage::new(&dir, "Test Workflow");
        (storage, dir)
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_create_and_load_run() {
        let (mut storage, dir) = temp_storage();
        storage.begin_execution().expect("begin execution");

        let node_id = Uuid::new_v4();
        let run = storage
            .create_run(node_id, "Launch Calculator", crate::TraceLevel::Minimal)
            .expect("create run");
        assert_eq!(run.node_id, node_id);
        assert_eq!(run.node_name, "Launch Calculator");
        assert_eq!(run.status, crate::RunStatus::Ok);

        let loaded = storage
            .load_run("Launch Calculator", run.run_id, None)
            .expect("load run");
        assert_eq!(loaded.run_id, run.run_id);
        assert_eq!(loaded.node_id, node_id);
        assert_eq!(loaded.node_name, "Launch Calculator");

        cleanup(&dir);
    }

    #[test]
    fn test_save_and_load_run() {
        let (mut storage, dir) = temp_storage();
        storage.begin_execution().expect("begin execution");

        let node_id = Uuid::new_v4();
        let mut run = storage
            .create_run(node_id, "Click Button", crate::TraceLevel::Full)
            .expect("create run");
        run.status = crate::RunStatus::Failed;
        run.ended_at = Some(RunStorage::now_millis());
        storage.save_run(&run).expect("save run");

        let loaded = storage
            .load_run("Click Button", run.run_id, None)
            .expect("load run");
        assert_eq!(loaded.status, crate::RunStatus::Failed);
        assert!(loaded.ended_at.is_some());

        cleanup(&dir);
    }

    #[test]
    fn test_append_event() {
        let (mut storage, dir) = temp_storage();
        storage.begin_execution().expect("begin execution");

        let node_id = Uuid::new_v4();
        let run = storage
            .create_run(node_id, "Test Node", crate::TraceLevel::Minimal)
            .expect("create run");

        let event = TraceEvent {
            timestamp: RunStorage::now_millis(),
            event_type: "test_event".to_string(),
            payload: serde_json::json!({"key": "value"}),
        };
        storage.append_event(&run, &event).expect("append event");

        let events_path = storage.run_dir(&run).join("events.jsonl");
        let content = std::fs::read_to_string(&events_path).expect("read events");
        assert!(content.contains("test_event"));

        cleanup(&dir);
    }

    #[test]
    fn test_save_artifact() {
        let (mut storage, dir) = temp_storage();
        storage.begin_execution().expect("begin execution");

        let node_id = Uuid::new_v4();
        let run = storage
            .create_run(node_id, "Screenshot Node", crate::TraceLevel::Full)
            .expect("create run");

        let data = b"fake image data";
        let artifact = storage
            .save_artifact(
                &run,
                ArtifactKind::Screenshot,
                "test.png",
                data,
                Value::Null,
            )
            .expect("save artifact");

        assert_eq!(artifact.kind, ArtifactKind::Screenshot);
        assert!(artifact.path.contains("test.png"));
        assert!(std::path::Path::new(&artifact.path).exists());

        cleanup(&dir);
    }

    #[test]
    fn test_load_runs_for_node() {
        let (mut storage, dir) = temp_storage();

        // Create runs across two separate executions
        let node_id = Uuid::new_v4();

        storage.begin_execution().expect("begin execution 1");
        storage
            .create_run(node_id, "My Node", crate::TraceLevel::Minimal)
            .expect("create run 1");

        storage.begin_execution().expect("begin execution 2");
        storage
            .create_run(node_id, "My Node", crate::TraceLevel::Minimal)
            .expect("create run 2");

        let runs = storage.load_runs_for_node("My Node").expect("load runs");
        assert_eq!(runs.len(), 2);
        assert!(runs[0].started_at <= runs[1].started_at);

        cleanup(&dir);
    }

    #[test]
    fn test_load_runs_for_nonexistent_node() {
        let (storage, dir) = temp_storage();

        let runs = storage
            .load_runs_for_node("Nonexistent")
            .expect("load runs");
        assert!(runs.is_empty());

        cleanup(&dir);
    }

    #[test]
    fn test_format_execution_dirname_produces_expected_format() {
        // 2026-02-13 16:30:00 UTC in milliseconds
        let ts_ms = 1_771_000_200_000u64;
        let run_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();

        let dirname = format_execution_dirname(ts_ms, run_id);
        assert_eq!(dirname, "2026-02-13_16-30-00_550e8400-e29");
    }

    #[test]
    fn test_new_app_data_path_structure() {
        let app_data_dir = PathBuf::from("/tmp/com.clickweave.app");
        let workflow_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let storage = RunStorage::new_app_data(&app_data_dir, "My Workflow", workflow_id);
        assert_eq!(
            storage.base_path,
            PathBuf::from("/tmp/com.clickweave.app/runs/my-workflow_550e8400")
        );
    }

    #[test]
    fn test_new_saved_project_path_structure() {
        let project_dir = PathBuf::from("/tmp/my-project");
        let storage = RunStorage::new(&project_dir, "Open Calculator");
        assert_eq!(
            storage.base_path,
            PathBuf::from("/tmp/my-project/.clickweave/runs/open-calculator")
        );
    }

    #[test]
    fn test_find_run_dir_locates_created_run() {
        let (mut storage, dir) = temp_storage();
        storage.begin_execution().expect("begin execution");

        let node_id = Uuid::new_v4();
        let run = storage
            .create_run(node_id, "Find Me", crate::TraceLevel::Minimal)
            .expect("create run");

        // Fast path: with execution_dir hint
        let found_fast = storage
            .find_run_dir("Find Me", run.run_id, Some(&run.execution_dir))
            .expect("find run dir (fast)");
        assert_eq!(found_fast, storage.run_dir(&run));

        // Slow path: without hint
        let found_slow = storage
            .find_run_dir("Find Me", run.run_id, None)
            .expect("find run dir (slow)");
        assert_eq!(found_slow, storage.run_dir(&run));

        cleanup(&dir);
    }

    #[test]
    fn test_append_execution_event() {
        let (mut storage, dir) = temp_storage();
        let exec_dir = storage.begin_execution().expect("begin execution");

        let event = TraceEvent {
            timestamp: RunStorage::now_millis(),
            event_type: "branch_evaluated".to_string(),
            payload: serde_json::json!({"node_name": "Check Result", "result": true}),
        };
        storage
            .append_execution_event(&event)
            .expect("append execution event");

        let events_path = storage.base_path.join(&exec_dir).join("events.jsonl");
        let content = std::fs::read_to_string(&events_path).expect("read events");
        assert!(content.contains("branch_evaluated"));
        assert!(content.contains("Check Result"));

        cleanup(&dir);
    }

    #[test]
    fn test_begin_execution_required_before_create_run() {
        let (storage, dir) = temp_storage();
        let node_id = Uuid::new_v4();
        let result = storage.create_run(node_id, "Node", crate::TraceLevel::Minimal);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("begin_execution"));
        cleanup(&dir);
    }

    #[test]
    fn test_directory_layout() {
        let (mut storage, dir) = temp_storage();
        let exec_dir = storage.begin_execution().expect("begin execution");

        let node_id = Uuid::new_v4();
        let run = storage
            .create_run(node_id, "Launch Calculator", crate::TraceLevel::Full)
            .expect("create run");

        let expected_path = storage.base_path.join(&exec_dir).join("launch-calculator");
        assert_eq!(storage.run_dir(&run), expected_path);
        assert!(expected_path.join("run.json").exists());
        assert!(expected_path.join("artifacts").exists());

        cleanup(&dir);
    }
}
