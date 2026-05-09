use super::*;

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
    /// Points to `runs/<project_dir>/`
    pub(super) base_path: PathBuf,
    /// Points to the project-local procedural-skills directory for this
    /// project. Saved projects use `<project>/.clickweave/skills/`;
    /// unsaved projects use `<app_data>/skills/<project_id>/`.
    pub(super) project_skills_path: PathBuf,
    /// The current execution directory name (set by `begin_execution`).
    pub(super) execution_dir: Option<String>,
    /// When false, every write operation is a no-op that returns a
    /// synthesised result. Used by the `Store run traces` privacy kill
    /// switch — the agent and executor code paths still behave as if
    /// storage is available, but nothing touches the disk.
    pub(super) persistent: bool,
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

    /// Directory holding the project-local procedural-skill files (Spec 3).
    /// Saved projects use `<project>/.clickweave/skills/`; unsaved projects
    /// use `<app_data>/skills/<project_id>/` so first-save can move the
    /// directory into the project without depending on the run-log layout.
    ///
    /// Creates the directory if it does not yet exist (mkdir -p
    /// semantics) so the `SkillStore` can write into it on the first
    /// extraction without a separate setup step. Disk failures are
    /// surfaced; callers fall back to a disabled `SkillContext` when
    /// the dir cannot be created.
    pub fn project_skills_dir(&self) -> Result<PathBuf> {
        let dir = self.project_skills_path.clone();
        if self.persistent && !dir.exists() {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("creating project skills dir at {}", dir.display()))?;
        }
        Ok(dir)
    }

    /// Path to the variant index file (workflow-level, not per-execution).
    pub fn variant_index_path(&self) -> PathBuf {
        self.base_path.join("variant_index.jsonl")
    }

    /// Path to the per-workflow conversational-agent chat transcript.
    /// Loaded by the UI on project open, saved on every assistant
    /// message push.
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
    /// Path: `<project>/.clickweave/runs/<sanitized_project_name>/`
    pub fn new(project_path: &Path, project_name: &str) -> Self {
        let clickweave_dir = project_path.join(".clickweave");
        Self {
            base_path: clickweave_dir
                .join("runs")
                .join(sanitize_name(project_name)),
            project_skills_path: clickweave_dir.join("skills"),
            execution_dir: None,
            persistent: true,
        }
    }

    /// Create storage for an unsaved project (app data fallback).
    ///
    /// Path: `<app_data>/runs/<sanitized_project_name>_<short_uuid>/`
    pub fn new_app_data(app_data_dir: &Path, project_name: &str, project_id: Uuid) -> Self {
        let short_id = &project_id.to_string()[..8];
        let dir_name = format!("{}_{short_id}", sanitize_name(project_name));
        Self {
            base_path: app_data_dir.join("runs").join(dir_name),
            project_skills_path: app_data_dir.join("skills").join(project_id.to_string()),
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
