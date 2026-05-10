use super::*;
use crate::SkillRun;

/// Manages on-disk storage for skill run records and trace events.
///
/// Skill runs (D27, D28) are persisted under a per-skill, per-run
/// directory inside the project's `.clickweave/skills/` tree:
///
/// ```text
/// <base>/.clickweave/skills/<skill_id>/
///   SKILL.md
///   replay.json
///   runs/
///     <run_id>.json                 ← one record per run (last 20 kept)
///     <run_id>/
///       events.jsonl                ← per-run trace events
/// ```
///
/// Retention is enforced on `create_skill_run` — older `<run_id>.json`
/// files (and their sibling event directories) past the most recent 20
/// are pruned in place.
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

    pub fn now_millis() -> u64 {
        now_millis()
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

    // ── Skill-keyed run storage (D27, D28) ────────────────────────────

    /// Directory holding per-run records for a specific skill:
    /// `<base>/.clickweave/skills/<skill_id>/runs/`.
    ///
    /// `base` for this method is the project-skills root (saved
    /// projects: `<project>/.clickweave/skills/`; unsaved projects:
    /// `<app_data>/skills/<project_id>/`) — the same root returned by
    /// [`Self::project_skills_dir`]. Anchoring runs there keeps the
    /// per-skill directory self-contained and matches the design's D27
    /// storage layout.
    pub fn skill_runs_dir(&self, skill_id: &str) -> PathBuf {
        self.project_skills_path.join(skill_id).join("runs")
    }

    /// Per-run trace-events directory:
    /// `<skill_runs_dir>/<run_id>/`. Events stream to `events.jsonl`
    /// inside this directory.
    pub fn skill_run_events_dir(&self, skill_id: &str, run_id: Uuid) -> PathBuf {
        self.skill_runs_dir(skill_id).join(run_id.to_string())
    }

    /// Maximum number of historical run records kept per skill (D27).
    pub const SKILL_RUN_HISTORY_LIMIT: usize = 20;

    /// Create a new run record for `skill_id`, write it to disk, and
    /// prune older records past [`Self::SKILL_RUN_HISTORY_LIMIT`].
    ///
    /// When persistence is disabled, returns a synthesised `SkillRun`
    /// without touching disk — callers downstream of the runner treat
    /// it identically to a persisted run.
    pub fn create_skill_run(&self, skill_id: &str) -> Result<SkillRun> {
        let run = SkillRun::new(skill_id.to_string());
        if !self.persistent {
            return Ok(run);
        }

        let runs_dir = self.skill_runs_dir(skill_id);
        std::fs::create_dir_all(&runs_dir).with_context(|| {
            format!(
                "Failed to create skill runs directory at {}",
                runs_dir.display()
            )
        })?;

        // Write the new record before pruning so a crash mid-prune
        // never deletes the freshest record we just created.
        self.save_skill_run(&run)?;
        prune_skill_runs(&runs_dir, Self::SKILL_RUN_HISTORY_LIMIT)?;
        Ok(run)
    }

    /// Persist a `SkillRun` atomically. Caller is responsible for
    /// updating `finished_at`, `status`, `duration_ms`, and per-section
    /// outcomes before saving terminal state.
    pub fn save_skill_run(&self, run: &SkillRun) -> Result<()> {
        if !self.persistent {
            return Ok(());
        }
        let runs_dir = self.skill_runs_dir(&run.skill_id);
        std::fs::create_dir_all(&runs_dir)
            .with_context(|| format!("Failed to create runs dir {}", runs_dir.display()))?;
        let path = runs_dir.join(format!("{}.json", run.run_id));
        write_json_atomic(&path, run).context("Failed to write skill-run JSON")
    }

    /// Look up a single run by `(skill_id, run_id)`. Returns `None`
    /// when the record file is absent or when persistence is disabled.
    pub fn find_skill_run(&self, skill_id: &str, run_id: Uuid) -> Result<Option<SkillRun>> {
        if !self.persistent {
            return Ok(None);
        }
        let path = self.skill_runs_dir(skill_id).join(format!("{run_id}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let run: SkillRun = serde_json::from_str(&data)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(Some(run))
    }

    /// Load every persisted run for `skill_id` sorted oldest-first by
    /// `started_at`. Records that fail to parse are logged and skipped
    /// so a corrupted file never breaks the timeline view.
    pub fn load_runs_for_skill(&self, skill_id: &str) -> Result<Vec<SkillRun>> {
        let runs_dir = self.skill_runs_dir(skill_id);
        if !runs_dir.exists() {
            return Ok(Vec::new());
        }
        let mut runs = Vec::new();
        for entry in std::fs::read_dir(&runs_dir)
            .with_context(|| format!("Failed to read {}", runs_dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(data) => match serde_json::from_str::<SkillRun>(&data) {
                    Ok(run) => runs.push(run),
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "Skipping unparseable skill-run record")
                    }
                },
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "Failed to read skill-run record")
                }
            }
        }
        runs.sort_by_key(|r| r.started_at);
        Ok(runs)
    }

    /// Append a trace event to the per-run `events.jsonl` for
    /// `(skill_id, run_id)`.
    ///
    /// Creates the per-run events directory on first call. No-op when
    /// persistence is disabled.
    pub fn append_skill_event(&self, run: &SkillRun, event: &TraceEvent) -> Result<()> {
        if !self.persistent {
            return Ok(());
        }
        let events_dir = self.skill_run_events_dir(&run.skill_id, run.run_id);
        std::fs::create_dir_all(&events_dir)
            .with_context(|| format!("Failed to create events dir {}", events_dir.display()))?;
        let path = events_dir.join("events.jsonl");
        Self::write_event_line(&path, event)
    }
}

/// Trim the per-skill runs directory to the most recent `keep` records,
/// removing both the `<run_id>.json` file and any sibling
/// `<run_id>/` events directory. Records are sorted by file mtime
/// (newest first); ties keep the last `keep` entries deterministically.
fn prune_skill_runs(runs_dir: &Path, keep: usize) -> Result<()> {
    if !runs_dir.exists() {
        return Ok(());
    }
    let mut entries: Vec<(std::time::SystemTime, PathBuf, Uuid)> = Vec::new();
    for entry in std::fs::read_dir(runs_dir)
        .with_context(|| format!("Failed to read {}", runs_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        // Filename without extension is the run id.
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let Ok(run_id) = stem.parse::<Uuid>() else {
            continue;
        };
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        entries.push((mtime, path, run_id));
    }

    if entries.len() <= keep {
        return Ok(());
    }

    entries.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, json_path, run_id) in entries.into_iter().skip(keep) {
        if let Err(e) = std::fs::remove_file(&json_path) {
            warn!(path = %json_path.display(), error = %e, "Failed to remove old skill-run record");
        }
        let events_dir = runs_dir.join(run_id.to_string());
        if events_dir.exists()
            && let Err(e) = std::fs::remove_dir_all(&events_dir)
        {
            warn!(path = %events_dir.display(), error = %e, "Failed to remove old skill-run events dir");
        }
    }
    Ok(())
}
