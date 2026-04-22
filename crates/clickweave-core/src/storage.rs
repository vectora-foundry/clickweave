use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use serde::Serialize;
use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

#[cfg(test)]
use crate::TraceEventKind;
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

/// Serialize `value` as pretty-printed JSON to a temp file alongside `path`
/// and atomically rename it into place. A crash or power loss mid-write leaves
/// either the previous content or the new content on disk — never a truncated
/// mix. The temp file is removed on serialization failure so it does not
/// accumulate beside the destination.
pub fn write_json_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    let tmp_path = tmp_path_for(path);
    if let Err(e) = std::fs::write(&tmp_path, json.as_bytes()) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    match std::fs::rename(&tmp_path, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

/// Serialize `value` as pretty-printed JSON to `path`, crash-atomically.
///
/// Thin wrapper over [`write_json_atomic`] that converts `io::Error` into the
/// `anyhow::Error` flavor used by `RunStorage`'s public methods.
pub fn write_json_pretty<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    write_json_atomic(path, value).with_context(|| format!("Failed to write {}", path.display()))
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
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

/// Parses the `YYYY-MM-DD_HH-MM-SS` prefix of an execution directory name
/// back into a UTC datetime. Returns `None` when the prefix does not match
/// the expected format (e.g. unrelated directories under the runs root).
///
/// Pure function — no filesystem access — so it is covered by unit tests
/// without touching disk.
pub fn parse_execution_dir_timestamp(dir_name: &str) -> Option<DateTime<Utc>> {
    // The prefix is exactly `YYYY-MM-DD_HH-MM-SS` (19 chars) followed by an
    // underscore and the short uuid. Reject anything shorter.
    if dir_name.len() < 19 {
        return None;
    }
    let (prefix, rest) = dir_name.split_at(19);
    // Require the separator plus at least one short-uuid char so
    // partially-written names like `2026-04-16_10-00-00_` do not
    // pass as valid execution dirs.
    let suffix = rest.strip_prefix('_')?;
    if suffix.is_empty() {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(prefix, "%Y-%m-%d_%H-%M-%S").ok()?;
    Some(Utc.from_utc_datetime(&naive))
}

/// Remove execution directories whose timestamp prefix is older than the
/// retention window. Only walks the two-level layout produced by
/// `RunStorage::new_app_data` — `runs/<workflow_dir>/<execution_dir>/` —
/// so sibling files (e.g. `decisions.json`, `agent_cache.json`) and any
/// dir that doesn't look like an execution dir are left alone.
///
/// * `runs_root` — the top-level `runs/` directory (e.g. under the app
///   data dir, or a saved project's `.clickweave/` dir).
/// * `retention_days` — maximum age in days. `0` disables cleanup and
///   returns immediately with an empty vec.
/// * `now` — current time, injected for deterministic testing.
///
/// Returns the list of execution directories that were successfully
/// removed. Individual failures are logged via `tracing::warn!` and do
/// not abort the sweep — per the privacy spec, cleanup is best-effort
/// and silent to the user.
pub fn cleanup_expired_runs(
    runs_root: &Path,
    retention_days: u64,
    now: DateTime<Utc>,
) -> Result<Vec<PathBuf>> {
    if retention_days == 0 {
        return Ok(Vec::new());
    }
    if !runs_root.exists() {
        return Ok(Vec::new());
    }

    // Clamp to a safe ceiling before the i64 cast so a hand-edited
    // `settings.json` with a huge `traceRetentionDays` cannot wrap into
    // a negative duration and push the cutoff into the future, which
    // would flag every existing run as expired and delete them all.
    // 10 years is well past any legitimate retention window — the UI
    // clamp is also 3650 days — so saturating here is indistinguishable
    // from "retain forever" in practice.
    const MAX_RETENTION_DAYS: u64 = 3650;
    let retention_days = retention_days.min(MAX_RETENTION_DAYS);

    let cutoff = now - chrono::Duration::days(retention_days as i64);
    let mut removed = Vec::new();

    let workflow_entries = std::fs::read_dir(runs_root)
        .with_context(|| format!("Failed to read runs root {}", runs_root.display()))?;

    for workflow_entry in workflow_entries {
        let workflow_entry = match workflow_entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "Skipping unreadable entry under runs root");
                continue;
            }
        };
        let Ok(file_type) = workflow_entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let workflow_dir = workflow_entry.path();

        let exec_entries = match std::fs::read_dir(&workflow_dir) {
            Ok(it) => it,
            Err(e) => {
                warn!(
                    path = %workflow_dir.display(),
                    error = %e,
                    "Skipping workflow dir whose contents could not be read",
                );
                continue;
            }
        };

        let mut removed_exec_names: Vec<String> = Vec::new();
        for exec_entry in exec_entries {
            let exec_entry = match exec_entry {
                Ok(e) => e,
                Err(e) => {
                    warn!(error = %e, "Skipping unreadable entry under workflow dir");
                    continue;
                }
            };
            let Ok(file_type) = exec_entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let exec_path = exec_entry.path();
            let Some(name) = exec_path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let name = name.to_string();
            let Some(ts) = parse_execution_dir_timestamp(&name) else {
                // Not an execution dir — leave it alone. Preserves
                // sibling files like `decisions.json`, `agent_cache.json`,
                // and future layout additions we do not yet know about.
                continue;
            };
            if ts >= cutoff {
                continue;
            }
            match std::fs::remove_dir_all(&exec_path) {
                Ok(()) => {
                    removed_exec_names.push(name);
                    removed.push(exec_path);
                }
                Err(e) => warn!(
                    path = %exec_path.display(),
                    error = %e,
                    "Failed to remove expired execution dir",
                ),
            }
        }

        // Tombstone the workflow-level variant index for the exec dirs
        // we just removed. Scoping to `removed_exec_names` (instead of
        // filtering all entries against what exists on disk) keeps the
        // rewrite deterministic and safe even when the read-time
        // filter in `VariantIndex::load_existing` is also active:
        // fresh entries appended by a run starting during the sweep
        // reference current exec dirs, which cannot appear in
        // `removed_exec_names`, so the rewrite is a pure minus
        // operation on known-stale lines.
        //
        // This fills the gap for workflows the user may never reopen
        // — without this, expired `divergence_summary` text would
        // linger on disk indefinitely. The read-time filter in
        // `VariantIndex::load_existing` remains as the belt-and-braces
        // safety net for entries we did not see here (manual
        // cleanup, partial failures, etc.).
        if !removed_exec_names.is_empty() {
            let variant_path = workflow_dir.join("variant_index.jsonl");
            if let Err(e) = prune_variant_index_entries(&variant_path, &removed_exec_names) {
                warn!(
                    path = %variant_path.display(),
                    error = %e,
                    "Failed to tombstone variant index entries after cleanup sweep",
                );
            }
        }
    }

    Ok(removed)
}

/// Rewrite `variant_index.jsonl` with every line whose `execution_dir`
/// is **not** in `removed_names`. Preserves unparseable lines so a
/// schema mismatch cannot corrupt history. Uses a temp file + rename
/// so a crash mid-prune leaves either the old or the new content,
/// never a partial write. No-op when the file does not exist.
///
/// Only called from `cleanup_expired_runs` with exec dir names the
/// sweep just removed — the read-time filter in
/// `VariantIndex::load_existing` handles everything else.
fn prune_variant_index_entries(path: &Path, removed_names: &[String]) -> Result<()> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(
                anyhow::Error::from(e).context(format!("Failed to read {}", path.display()))
            );
        }
    };

    let removed_set: std::collections::HashSet<&str> =
        removed_names.iter().map(String::as_str).collect();

    let mut kept = String::with_capacity(content.len());
    let mut pruned_any = false;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let exec_dir_opt = serde_json::from_str::<Value>(line).ok().and_then(|v| {
            v.get("execution_dir")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        match exec_dir_opt {
            Some(name) if removed_set.contains(name.as_str()) => {
                pruned_any = true;
            }
            _ => {
                kept.push_str(line);
                kept.push('\n');
            }
        }
    }

    if !pruned_any {
        return Ok(());
    }

    if kept.is_empty() {
        std::fs::remove_file(path).with_context(|| {
            format!("Failed to remove emptied variant index {}", path.display())
        })?;
        return Ok(());
    }

    let tmp_path = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp_path, &kept)
        .with_context(|| format!("Failed to write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("Failed to rename over {}", path.display()))?;
    Ok(())
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
    /// When false, every write operation is a no-op that returns a
    /// synthesised result. Used by the `Store run traces` privacy kill
    /// switch — the agent and executor code paths still behave as if
    /// storage is available (caches still function in-memory for the
    /// session), but nothing touches the disk.
    persistent: bool,
}

impl RunStorage {
    pub fn execution_dir_name(&self) -> Option<&str> {
        self.execution_dir.as_deref()
    }

    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// Whether this storage persists to disk. When `false`, every write
    /// becomes a no-op.
    pub fn is_persistent(&self) -> bool {
        self.persistent
    }

    /// Disable disk persistence for this storage instance. Used by the
    /// privacy kill switch to make a run entirely in-memory.
    ///
    /// Must be called before `begin_execution()` — toggling persistence
    /// mid-run would leak a partial trace onto disk.
    pub fn set_persistent(&mut self, persistent: bool) {
        self.persistent = persistent;
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

    /// Path to the per-workflow conversational-agent chat transcript.
    /// Sibling to `agent_cache_path()`. Loaded by the UI on project
    /// open, saved on every assistant message push.
    pub fn agent_chat_path(&self) -> PathBuf {
        self.base_path.join("agent_chat.json")
    }

    /// Path to the `artifacts/` directory for the current execution.
    ///
    /// Returns `None` when `begin_execution()` has not yet been called, or
    /// when persistence is disabled (the path would point at a non-existent
    /// directory in that case).
    ///
    /// The directory itself is NOT created by this method; the caller is
    /// responsible for creating it (via `std::fs::create_dir_all`) if needed.
    pub fn execution_artifacts_dir(&self) -> Option<PathBuf> {
        let exec_dir = self.execution_dir.as_ref()?;
        if !self.persistent {
            return None;
        }
        Some(self.base_path.join(exec_dir).join("artifacts"))
    }

    /// Append a serializable agent event to the execution-level events.jsonl.
    ///
    /// No-op when persistence is disabled — the agent run still requires
    /// `begin_execution()` to have been called so the execution dir name
    /// can be reported, but nothing is written to disk.
    pub fn append_agent_event(&self, event: &impl Serialize) -> Result<()> {
        let execution_dir = self
            .execution_dir
            .as_ref()
            .context("begin_execution() must be called before append_agent_event()")?;
        if !self.persistent {
            return Ok(());
        }
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
            persistent: true,
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
            persistent: true,
        }
    }

    /// Start a new workflow execution. Creates a shared datetime directory
    /// under the workflow dir and stores it for subsequent `create_run` calls.
    ///
    /// Returns the execution directory name.
    ///
    /// When persistence is disabled, the directory name is still
    /// computed and stored so downstream code sees a valid
    /// `execution_dir`, but no filesystem entry is created.
    pub fn begin_execution(&mut self) -> Result<String> {
        let exec_id = Uuid::new_v4();
        let started_at = Self::now_millis();
        let dirname = format_timestamped_dirname(started_at, exec_id);
        if self.persistent {
            std::fs::create_dir_all(self.base_path.join(&dirname))
                .context("Failed to create execution directory")?;
        }
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
            let run_json = node_dir.join("run.json");
            if !run_json.exists() {
                continue;
            }
            match std::fs::read_to_string(&run_json) {
                Ok(data) => match serde_json::from_str::<NodeRun>(&data) {
                    Ok(run) if run.run_id == run_id => return Ok(node_dir),
                    Ok(_) => continue,
                    Err(e) => warn!(
                        path = %run_json.display(),
                        error = %e,
                        "run.json exists but failed to parse while scanning for run",
                    ),
                },
                Err(e) => warn!(
                    path = %run_json.display(),
                    error = %e,
                    "failed to read run.json while scanning for run",
                ),
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
    ///
    /// When persistence is disabled, returns a synthesised `NodeRun`
    /// without touching disk. Downstream code (executor, run_loop)
    /// treats it the same as a persisted run — event/artifact writes
    /// short-circuit on the same `persistent` flag.
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

        if self.persistent {
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

            std::fs::create_dir_all(dir.join("artifacts"))
                .context("Failed to create run directory")?;
        }

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
        if !self.persistent {
            return Ok(());
        }
        let dir = self.run_dir(run);
        std::fs::create_dir_all(&dir).context("Failed to create run directory")?;

        write_json_atomic(&dir.join("run.json"), run).context("Failed to write run.json")?;
        Ok(())
    }

    pub fn append_event(&self, run: &NodeRun, event: &TraceEvent) -> Result<()> {
        if !self.persistent {
            return Ok(());
        }
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
        if !self.persistent {
            return Ok(());
        }
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

        if self.persistent {
            std::fs::write(&artifact_path, data).context("Failed to write artifact")?;
        }

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
        if !self.persistent {
            return Ok(());
        }

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
            event_type: TraceEventKind::Unknown,
            payload: serde_json::json!({"key": "value"}),
        };
        storage.append_event(&run, &event).expect("append event");

        let events_path = storage.run_dir(&run).join("events.jsonl");
        let content = std::fs::read_to_string(&events_path).expect("read events");
        // `Unknown` serializes as "unknown" because of rename_all = "snake_case".
        assert!(content.contains("unknown"));

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

        let dirname = format_timestamped_dirname(ts_ms, run_id);
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
    fn agent_chat_path_is_sibling_to_agent_cache_path() {
        let project_dir = PathBuf::from("/tmp/my-project");
        let storage = RunStorage::new(&project_dir, "My Workflow");
        let cache = storage.agent_cache_path();
        let chat = storage.agent_chat_path();
        assert_eq!(
            cache.parent(),
            chat.parent(),
            "agent_chat.json must live beside agent_cache.json"
        );
        assert_eq!(chat.file_name().unwrap(), "agent_chat.json");
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
            event_type: TraceEventKind::BranchEvaluated,
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

    // ── cleanup_expired_runs ─────────────────────────────────────

    fn make_runs_tree(root: &Path, layout: &[(&str, &str)]) {
        for (workflow, exec_dir) in layout {
            let full = root.join(workflow).join(exec_dir);
            std::fs::create_dir_all(&full).expect("create tree");
            std::fs::write(full.join("run.json"), b"{}").expect("write sentinel");
        }
    }

    #[test]
    fn parse_execution_dir_timestamp_round_trips_with_formatter() {
        let ts_ms = 1_771_000_200_000u64;
        let run_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let dirname = format_timestamped_dirname(ts_ms, run_id);

        let parsed = parse_execution_dir_timestamp(&dirname).expect("parse timestamp");
        let expected = DateTime::<Utc>::from_timestamp_millis(ts_ms as i64).unwrap();
        // Formatter drops sub-second precision; compare at second granularity.
        assert_eq!(parsed.timestamp(), expected.timestamp());
    }

    #[test]
    fn parse_execution_dir_timestamp_rejects_short_and_malformed_names() {
        assert!(parse_execution_dir_timestamp("").is_none());
        assert!(parse_execution_dir_timestamp("too-short").is_none());
        // 19 chars but no underscore separator → reject
        assert!(parse_execution_dir_timestamp("2026-04-16_10-00-00").is_none());
        // Bad month
        assert!(parse_execution_dir_timestamp("2026-13-16_10-00-00_abc").is_none());
        // Unrelated prefix
        assert!(parse_execution_dir_timestamp("decisions.json_abc").is_none());
        // Separator present but no short-uuid suffix → reject
        assert!(parse_execution_dir_timestamp("2026-04-16_10-00-00_").is_none());
    }

    #[test]
    fn cleanup_expired_runs_removes_only_expired_dirs() {
        let root = std::env::temp_dir()
            .join("clickweave_cleanup_test")
            .join(Uuid::new_v4().to_string());
        let runs = root.join("runs");

        make_runs_tree(
            &runs,
            &[
                ("workflow-a", "2026-01-01_00-00-00_aaaaaaaaaaaa"), // old
                ("workflow-a", "2026-04-15_12-00-00_bbbbbbbbbbbb"), // fresh
                ("workflow-b", "2026-02-01_00-00-00_cccccccccccc"), // old
            ],
        );

        // Sibling files and unrelated dirs should be preserved.
        std::fs::write(runs.join("workflow-a").join("decisions.json"), b"{}").unwrap();
        std::fs::create_dir_all(runs.join("workflow-a").join("unrelated-dir")).unwrap();

        let now = Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 0).unwrap();
        let removed = cleanup_expired_runs(&runs, 30, now).expect("cleanup");

        // Two dirs are older than 30 days → both removed.
        assert_eq!(removed.len(), 2, "removed {:?}", removed);
        assert!(
            !runs
                .join("workflow-a/2026-01-01_00-00-00_aaaaaaaaaaaa")
                .exists()
        );
        assert!(
            runs.join("workflow-a/2026-04-15_12-00-00_bbbbbbbbbbbb")
                .exists()
        );
        assert!(
            !runs
                .join("workflow-b/2026-02-01_00-00-00_cccccccccccc")
                .exists()
        );
        // Non-execution siblings left alone.
        assert!(runs.join("workflow-a/decisions.json").exists());
        assert!(runs.join("workflow-a/unrelated-dir").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cleanup_expired_runs_retention_zero_is_noop() {
        let root = std::env::temp_dir()
            .join("clickweave_cleanup_test")
            .join(Uuid::new_v4().to_string());
        let runs = root.join("runs");
        make_runs_tree(&runs, &[("wf", "2020-01-01_00-00-00_aaaaaaaaaaaa")]);

        let now = Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 0).unwrap();
        let removed = cleanup_expired_runs(&runs, 0, now).expect("cleanup");
        assert!(removed.is_empty());
        assert!(runs.join("wf/2020-01-01_00-00-00_aaaaaaaaaaaa").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cleanup_expired_runs_missing_root_is_noop() {
        let root = std::env::temp_dir()
            .join("clickweave_cleanup_test")
            .join(Uuid::new_v4().to_string())
            .join("does-not-exist");
        let now = Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 0).unwrap();
        let removed = cleanup_expired_runs(&root, 30, now).expect("cleanup");
        assert!(removed.is_empty());
    }

    #[test]
    fn cleanup_expired_runs_preserves_recent_edge_of_window() {
        let root = std::env::temp_dir()
            .join("clickweave_cleanup_test")
            .join(Uuid::new_v4().to_string());
        let runs = root.join("runs");
        // Exactly 30 days old → still within retention (cutoff is
        // inclusive of "now - 30d", so >= cutoff stays).
        make_runs_tree(&runs, &[("wf", "2026-03-17_00-00-00_aaaaaaaaaaaa")]);

        let now = Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 0).unwrap();
        let removed = cleanup_expired_runs(&runs, 30, now).expect("cleanup");
        assert!(removed.is_empty(), "edge of window should be preserved");
        assert!(runs.join("wf/2026-03-17_00-00-00_aaaaaaaaaaaa").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── persistence kill switch ─────────────────────────────────

    #[test]
    fn non_persistent_storage_writes_nothing_to_disk() {
        let base = std::env::temp_dir()
            .join("clickweave_nonpersist_test")
            .join(Uuid::new_v4().to_string());
        let mut storage = RunStorage::new(&base, "Test Workflow");
        storage.set_persistent(false);

        let exec_dir = storage.begin_execution().expect("begin execution");
        assert!(
            !storage.base_path.exists(),
            "base_path must not be created when persistence is disabled"
        );

        let node_id = Uuid::new_v4();
        let run = storage
            .create_run(node_id, "Launch Calculator", crate::TraceLevel::Minimal)
            .expect("create run");
        assert_eq!(run.node_id, node_id);
        assert_eq!(run.execution_dir, exec_dir);
        assert!(
            !storage.run_dir(&run).exists(),
            "create_run must not create on-disk directories when disabled"
        );

        let event = TraceEvent {
            timestamp: RunStorage::now_millis(),
            event_type: TraceEventKind::Unknown,
            payload: serde_json::json!({"key": "value"}),
        };
        storage.append_event(&run, &event).expect("append event");
        storage
            .append_execution_event(&event)
            .expect("append exec event");
        storage
            .append_agent_event(&serde_json::json!({"k": "v"}))
            .expect("append agent event");

        storage
            .save_artifact(
                &run,
                ArtifactKind::Screenshot,
                "shot.png",
                b"bytes",
                Value::Null,
            )
            .expect("save artifact");

        assert!(
            !base.exists(),
            "nothing under the base path may be written when persistence is disabled — found {}",
            base.display(),
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn set_persistent_true_restores_disk_writes() {
        let base = std::env::temp_dir()
            .join("clickweave_persist_toggle_test")
            .join(Uuid::new_v4().to_string());
        let mut storage = RunStorage::new(&base, "Toggle");
        storage.set_persistent(false);
        storage.set_persistent(true);

        storage.begin_execution().expect("begin execution");
        assert!(
            storage.base_path.exists(),
            "re-enabling persistence should resume on-disk writes"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // ── variant index isolation ─────────────────────────────────

    #[test]
    fn cleanup_expired_runs_tombstones_variant_index_entries_for_removed_dirs() {
        // The cleanup sweep tombstones variant index entries matching
        // the dirs it removed so the retention promise actually ages
        // that privacy-sensitive text off disk at startup — not just
        // on the next `run_agent`. Entries referencing dirs outside
        // the removed set (e.g. fresh runs, legacy orphans) are
        // preserved so the startup sweep cannot race a live append.
        let root = std::env::temp_dir()
            .join("clickweave_cleanup_variant_tombstone_test")
            .join(Uuid::new_v4().to_string());
        let runs = root.join("runs");
        let wf = runs.join("workflow-a");

        let old_exec = "2026-01-01_00-00-00_aaaaaaaaaaaa";
        let fresh_exec = "2026-04-15_12-00-00_bbbbbbbbbbbb";
        let legacy_orphan = "2020-01-01_00-00-00_legacyxxxxxx";
        make_runs_tree(
            &runs,
            &[("workflow-a", old_exec), ("workflow-a", fresh_exec)],
        );

        let vp = wf.join("variant_index.jsonl");
        let original = format!(
            "{{\"execution_dir\":\"{old_exec}\",\"diverged_at_step\":null,\"divergence_summary\":\"old\",\"success\":false}}\n\
             {{\"execution_dir\":\"{fresh_exec}\",\"diverged_at_step\":null,\"divergence_summary\":\"new\",\"success\":true}}\n\
             {{\"execution_dir\":\"{legacy_orphan}\",\"diverged_at_step\":null,\"divergence_summary\":\"legacy\",\"success\":true}}\n",
        );
        std::fs::write(&vp, &original).expect("seed variant index");

        let now = Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 0).unwrap();
        let removed = cleanup_expired_runs(&runs, 30, now).expect("cleanup");
        assert_eq!(removed.len(), 1);
        assert!(
            !wf.join(old_exec).exists(),
            "expired exec dir must be removed"
        );

        let after = std::fs::read_to_string(&vp).expect("read variant index");
        assert!(
            !after.contains(old_exec),
            "entry referencing the removed exec dir must be dropped from disk",
        );
        assert!(after.contains(fresh_exec), "fresh entry must survive",);
        assert!(
            after.contains(legacy_orphan),
            "entry whose dir was not in the current sweep must be left for the read-time filter",
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cleanup_expired_runs_deletes_variant_index_when_every_line_matches_removed_dirs() {
        let root = std::env::temp_dir()
            .join("clickweave_cleanup_variant_delete_test")
            .join(Uuid::new_v4().to_string());
        let runs = root.join("runs");
        let wf = runs.join("wf");
        let old_exec = "2026-01-01_00-00-00_aaaaaaaaaaaa";
        make_runs_tree(&runs, &[("wf", old_exec)]);
        let vp = wf.join("variant_index.jsonl");
        std::fs::write(
            &vp,
            format!(
                "{{\"execution_dir\":\"{old_exec}\",\"diverged_at_step\":null,\"divergence_summary\":\"\",\"success\":false}}\n",
            ),
        )
        .unwrap();

        let now = Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 0).unwrap();
        cleanup_expired_runs(&runs, 30, now).expect("cleanup");

        assert!(
            !vp.exists(),
            "variant_index.jsonl should be removed when every remaining line would be empty",
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cleanup_expired_runs_leaves_variant_index_untouched_when_nothing_removed() {
        let root = std::env::temp_dir()
            .join("clickweave_cleanup_variant_untouched_test")
            .join(Uuid::new_v4().to_string());
        let runs = root.join("runs");
        let wf = runs.join("wf");
        let fresh_exec = "2026-04-15_12-00-00_bbbbbbbbbbbb";
        make_runs_tree(&runs, &[("wf", fresh_exec)]);
        let vp = wf.join("variant_index.jsonl");
        let original = format!(
            "{{\"execution_dir\":\"{fresh_exec}\",\"diverged_at_step\":null,\"divergence_summary\":\"fresh\",\"success\":true}}\n",
        );
        std::fs::write(&vp, &original).unwrap();

        let now = Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 0).unwrap();
        cleanup_expired_runs(&runs, 30, now).expect("cleanup");

        let after = std::fs::read_to_string(&vp).expect("read variant index");
        assert_eq!(
            after, original,
            "variant_index.jsonl must be byte-identical when no exec dirs were removed",
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cleanup_expired_runs_clamps_absurd_retention_values() {
        // `u64::MAX` is the worst-case hand-edited settings.json
        // value. Before the clamp this wrapped to a negative i64 when
        // chrono built the `Duration`, pushing the cutoff into the
        // future and flagging every fresh dir as expired.
        let root = std::env::temp_dir()
            .join("clickweave_cleanup_clamp_test")
            .join(Uuid::new_v4().to_string());
        let runs = root.join("runs");
        let fresh_exec = "2026-04-15_12-00-00_bbbbbbbbbbbb";
        make_runs_tree(&runs, &[("wf", fresh_exec)]);

        let now = Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 0).unwrap();
        let removed = cleanup_expired_runs(&runs, u64::MAX, now).expect("cleanup");
        assert!(
            removed.is_empty(),
            "absurd retention window must not delete fresh dirs after the clamp",
        );
        assert!(runs.join("wf").join(fresh_exec).exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── write_json_atomic ───────────────────────────────────────

    #[test]
    fn write_json_atomic_leaves_no_temp_file_on_success() {
        let dir = std::env::temp_dir()
            .join("clickweave_atomic_write_success")
            .join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.json");

        write_json_atomic(&path, &serde_json::json!({"ok": true})).expect("atomic write");

        assert!(path.exists(), "destination file must exist after success");
        let tmp = tmp_path_for(&path);
        assert!(
            !tmp.exists(),
            "temp file {} must not linger after a successful write",
            tmp.display(),
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_json_atomic_preserves_existing_file_when_write_fails() {
        let dir = std::env::temp_dir()
            .join("clickweave_atomic_write_preserve")
            .join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.json");

        let original = r#"{"before":"untouched"}"#;
        std::fs::write(&path, original).unwrap();

        // Force an I/O failure by asking the helper to write through an
        // existing file (treating it as a parent dir). The failure must
        // leave the pre-existing destination byte-identical and must not
        // leave a stray temp file beside it.
        let unwritable = path.join("cannot-write-inside-a-file");
        let err = write_json_atomic(&unwritable, &serde_json::json!({"x": 1}));
        assert!(err.is_err(), "writing through a file path must fail");

        let preserved = std::fs::read_to_string(&path).expect("read after failure");
        assert_eq!(
            preserved, original,
            "pre-existing destination must survive a failed write",
        );

        let tmp = tmp_path_for(&unwritable);
        assert!(
            !tmp.exists(),
            "temp file must be cleaned up after a failure",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
