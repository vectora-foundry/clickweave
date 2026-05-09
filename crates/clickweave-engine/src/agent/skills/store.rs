//! Filesystem-backed skill store.
//!
//! Phase 1 leaves the on-disk filename layout backwards-compatible while
//! the higher-level rewrite migrates callers to the
//! `<skill_id>/SKILL.md` directory shape. Writes go through an atomic
//! `<basename>.tmp` → rename so a partial file is never visible to
//! readers (or the file watcher). The store records the timestamp of
//! every successful write so the file watcher can skip self-write
//! events when flipping `edited_by_user` on external edits.
//!
//! Atomic-write protocol (P1.H.2): [`SkillStore::write_atomic_multi_file`]
//! stages every byte buffer under `<skill_dir>/.tx/pending/<basename>.new`,
//! writes a `manifest.json`, fsyncs, then creates the
//! `<skill_dir>/.tx/commit` marker via `OpenOptions::create_new` — the
//! single atomic boundary. After the marker exists, every staged file
//! is renamed over its live target and the journal is cleaned up.
//! [`SkillStore::recover_atomic_writes`] replays a partial commit on
//! next load.

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::frontmatter::{emit_skill_md, parse_skill_md};
use super::replay::ReplayJson;
use super::types::{Skill, SkillError, SkillId};

const RECENT_WRITE_TOLERANCE: Duration = Duration::from_millis(100);
const TX_DIR: &str = ".tx";
const TX_PENDING: &str = "pending";
const TX_COMMIT: &str = "commit";
const TX_MANIFEST: &str = "manifest.json";

/// One file scheduled for atomic write under the skill-directory
/// transaction journal. Carried in [`AtomicWriteManifest::files`] and in
/// the `write_atomic_multi_file` argument list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomicWriteFile {
    /// Final path under the skill dir (relative to the skill dir, not
    /// absolute), e.g. `SKILL.md` or `replay.json`.
    pub relative: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AtomicWriteManifest {
    files: Vec<AtomicWriteFile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveReport {
    pub moved: usize,
}

#[derive(Debug)]
pub struct SkillStore {
    dir: PathBuf,
    last_written: Mutex<HashMap<PathBuf, Instant>>,
}

impl SkillStore {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            last_written: Mutex::new(HashMap::new()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn list_files(&self) -> io::Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        if !self.dir.exists() {
            return Ok(out);
        }
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
                out.push(path);
            }
        }
        out.sort();
        Ok(out)
    }

    pub fn read_skill(&self, path: &Path) -> Result<Skill, SkillError> {
        let contents = fs::read_to_string(path)?;
        parse_skill_md(&contents)
    }

    pub fn write_skill(&self, skill: &Skill) -> Result<PathBuf, SkillError> {
        if !self.dir.exists() {
            fs::create_dir_all(&self.dir)?;
        }
        let final_path = self.dir.join(legacy_basename(skill));
        let tmp_path = self.dir.join(format!("{}.tmp", legacy_basename(skill)));
        let contents = emit_skill_md(skill);
        fs::write(&tmp_path, contents)?;
        fs::rename(&tmp_path, &final_path)?;
        self.record_write(&final_path);
        Ok(final_path)
    }

    /// Write a `replay.json` sidecar for `skill_id` under the skill
    /// directory. Same atomic `<basename>.tmp` → rename pattern as
    /// [`Self::write_skill`]. Phase 1 lands the storage primitive; the
    /// extractor wires it up in a follow-up subphase.
    pub fn write_replay(
        &self,
        skill_id: &SkillId,
        replay: &ReplayJson,
    ) -> Result<PathBuf, SkillError> {
        let skill_dir = self.dir.join(skill_id);
        if !skill_dir.exists() {
            fs::create_dir_all(&skill_dir)?;
        }
        let final_path = skill_dir.join("replay.json");
        let tmp_path = skill_dir.join("replay.json.tmp");
        let bytes = serde_json::to_vec_pretty(replay)
            .map_err(|err| SkillError::InvalidParameters(format!("encode replay: {err}")))?;
        fs::write(&tmp_path, bytes)?;
        fs::rename(&tmp_path, &final_path)?;
        self.record_write(&final_path);
        Ok(final_path)
    }

    /// Atomic write with mtime conflict detection (D31). Returns
    /// [`SkillError::ExternalConflict`] when the on-disk skill file's
    /// mtime differs from `expected_mtime`. Pre-existing absent files
    /// (no live target yet) only conflict when `expected_mtime` is
    /// `Some` — a `None` expected mtime is the "no preexisting file"
    /// promise.
    pub fn write_skill_atomic(
        &self,
        skill: &Skill,
        expected_mtime: Option<SystemTime>,
    ) -> Result<PathBuf, SkillError> {
        let final_path = self.dir.join(legacy_basename(skill));
        if let Some(expected) = expected_mtime {
            match fs::metadata(&final_path) {
                Ok(meta) => {
                    let actual = meta.modified()?;
                    if !mtime_matches(actual, expected) {
                        return Err(SkillError::ExternalConflict);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    return Err(SkillError::ExternalConflict);
                }
                Err(err) => return Err(SkillError::Io(err)),
            }
        } else if final_path.exists() {
            return Err(SkillError::ExternalConflict);
        }
        self.write_skill(skill)
    }

    /// Atomic write of multiple files belonging to one skill directory
    /// via the journal protocol described in §1.H.2.
    ///
    /// Single-file callers (just `SKILL.md`, just `replay.json`, etc.)
    /// pass a one-entry vec. The single atomic boundary is the
    /// `<skill_dir>/.tx/commit` marker created via `OpenOptions::create_new`
    /// — once that file exists, all renames are guaranteed to land on
    /// recovery, so a partial in-flight write never leaves the on-disk
    /// state inconsistent.
    pub fn write_atomic_multi_file(
        &self,
        skill_id: &SkillId,
        files: Vec<(PathBuf, Vec<u8>)>,
        expected_mtime: Option<SystemTime>,
    ) -> Result<(), SkillError> {
        let skill_dir = self.dir.join(skill_id);
        fs::create_dir_all(&skill_dir)?;

        // mtime guard against external concurrent edits (D31).
        if let Some(expected) = expected_mtime {
            // Probe the dominant artifact (`SKILL.md`) when present; the
            // sidecar mtime drifts independently, so only the canonical
            // body file participates in the guard.
            let probe = skill_dir.join("SKILL.md");
            if probe.exists() {
                let actual = fs::metadata(&probe)?.modified()?;
                if !mtime_matches(actual, expected) {
                    return Err(SkillError::ExternalConflict);
                }
            }
        }

        let tx_dir = skill_dir.join(TX_DIR);
        let pending = tx_dir.join(TX_PENDING);
        // Clean any stale pending state before staging fresh writes.
        if pending.exists() {
            fs::remove_dir_all(&pending)?;
        }
        fs::create_dir_all(&pending)?;

        let mut manifest = AtomicWriteManifest { files: Vec::new() };
        for (relative, bytes) in &files {
            // Stage `<basename>.new` under pending/. Only flat-file
            // basenames are supported in Phase 1; nested paths are an
            // error.
            let basename = relative
                .file_name()
                .ok_or_else(|| {
                    SkillError::InvalidParameters("atomic write target lacks a file name".into())
                })?
                .to_string_lossy()
                .into_owned();
            let staged = pending.join(format!("{basename}.new"));
            fs::write(&staged, bytes)?;
            manifest.files.push(AtomicWriteFile {
                relative: relative.clone(),
            });
        }

        let manifest_path = tx_dir.join(TX_MANIFEST);
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|err| SkillError::InvalidParameters(format!("manifest encode: {err}")))?;
        fs::write(&manifest_path, &manifest_bytes)?;

        // Create the commit marker exclusively. Past this point, the
        // transaction is guaranteed to land on the next recovery pass.
        let commit_path = tx_dir.join(TX_COMMIT);
        let mut commit_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&commit_path)?;
        commit_file.write_all(b"")?;
        commit_file.sync_all()?;
        drop(commit_file);

        // Replay the renames now. `recover_atomic_writes` performs the
        // same loop on next load if we crash mid-way.
        apply_committed_transaction(&skill_dir, &manifest)?;

        // Cleanup is best-effort — leftover .tx is harmless.
        let _ = fs::remove_file(&commit_path);
        let _ = fs::remove_file(&manifest_path);
        let _ = fs::remove_dir_all(&pending);
        let _ = fs::remove_dir(&tx_dir);

        for entry in &manifest.files {
            self.record_write(&skill_dir.join(&entry.relative));
        }
        Ok(())
    }

    /// Replay or roll back a partial transaction left over from a
    /// previous run for `skill_id`. If `<skill_dir>/.tx/commit` exists
    /// the manifest's renames are applied and the journal cleared. If
    /// pending state exists without the commit marker, it is dropped.
    /// Idempotent — a clean skill directory short-circuits.
    pub fn recover_atomic_writes(&self, skill_id: &SkillId) -> Result<(), SkillError> {
        let skill_dir = self.dir.join(skill_id);
        let tx_dir = skill_dir.join(TX_DIR);
        if !tx_dir.exists() {
            return Ok(());
        }
        let commit_marker = tx_dir.join(TX_COMMIT);
        let manifest_path = tx_dir.join(TX_MANIFEST);
        let pending = tx_dir.join(TX_PENDING);

        if commit_marker.exists() && manifest_path.exists() {
            let bytes = fs::read(&manifest_path)?;
            let manifest: AtomicWriteManifest = serde_json::from_slice(&bytes)
                .map_err(|err| SkillError::InvalidParameters(format!("manifest decode: {err}")))?;
            apply_committed_transaction(&skill_dir, &manifest)?;
            let _ = fs::remove_file(&commit_marker);
            let _ = fs::remove_file(&manifest_path);
            let _ = fs::remove_dir_all(&pending);
            let _ = fs::remove_dir(&tx_dir);
        } else {
            // No commit → roll back: drop staged + manifest.
            let _ = fs::remove_dir_all(&pending);
            let _ = fs::remove_file(&manifest_path);
            let _ = fs::remove_dir(&tx_dir);
        }
        Ok(())
    }

    pub fn delete_skill(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)?;
        self.record_write(path);
        Ok(())
    }

    /// In-app rename: write the new file under its current `(id,
    /// version)` filename, then drop the old file. The two writes are
    /// not transactional, but the watcher consumer treats both events
    /// as self-writes via `was_recently_written`.
    pub fn rename_skill_in_place(
        &self,
        old_path: &Path,
        skill: &Skill,
    ) -> Result<PathBuf, SkillError> {
        let new_path = self.write_skill(skill)?;
        if old_path != new_path && old_path.exists() {
            fs::remove_file(old_path)?;
            self.record_write(old_path);
        }
        Ok(new_path)
    }

    /// True if the store wrote (or deleted) `path` within the past
    /// `RECENT_WRITE_TOLERANCE`. The watcher consumer uses this to skip
    /// self-write events that would otherwise flip `edited_by_user`.
    pub fn was_recently_written(&self, path: &Path) -> bool {
        let mut guard = self.last_written.lock();
        // Opportunistic GC of stale entries — the table never grows
        // unbounded as long as the watcher drains regularly.
        guard.retain(|_, ts| ts.elapsed() <= RECENT_WRITE_TOLERANCE * 4);
        guard.iter().any(|(written_path, ts)| {
            ts.elapsed() <= RECENT_WRITE_TOLERANCE && paths_equivalent(written_path, path)
        })
    }

    fn record_write(&self, path: &Path) {
        self.last_written
            .lock()
            .insert(path.to_path_buf(), Instant::now());
    }
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }

    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

/// Legacy `<slug>-v<N>.md` basename used by the existing flat-file
/// callers. The skill-only-shell rewrite migrates to a `<skill_id>/SKILL.md`
/// directory layout in a follow-up subphase; this helper is the bridge
/// while consumers still expect the flat shape.
pub fn legacy_basename(skill: &Skill) -> String {
    format!("{}-v{}.md", slugify(&skill.id), skill.version)
}

fn mtime_matches(left: SystemTime, right: SystemTime) -> bool {
    // SystemTime equality is exact on most filesystems but tools like
    // `touch` round to seconds. Treat sub-second drift as equal to keep
    // the conflict guard from false-positiving on innocuous mtime
    // refreshes (e.g. Time Machine's reflink copies).
    let drift = if left >= right {
        left.duration_since(right).unwrap_or_default()
    } else {
        right.duration_since(left).unwrap_or_default()
    };
    drift.as_millis() < 2
}

fn apply_committed_transaction(
    skill_dir: &Path,
    manifest: &AtomicWriteManifest,
) -> Result<(), SkillError> {
    let pending = skill_dir.join(TX_DIR).join(TX_PENDING);
    for entry in &manifest.files {
        let basename = entry
            .relative
            .file_name()
            .ok_or_else(|| {
                SkillError::InvalidParameters("manifest entry lacks a file name".into())
            })?
            .to_string_lossy()
            .into_owned();
        let staged = pending.join(format!("{basename}.new"));
        let target = skill_dir.join(&entry.relative);
        if !staged.exists() {
            // A previous recovery pass may have already moved this
            // file. Skip rather than failing — recovery is idempotent.
            continue;
        }
        if let Some(parent) = target.parent()
            && !parent.exists()
        {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&staged, &target)?;
    }
    Ok(())
}

pub fn move_skills_to_project(
    app_data_skills_root: &Path,
    project_uuid: &str,
    project_path: &Path,
) -> Result<MoveReport, SkillError> {
    let src = app_data_skills_root.join(project_uuid);
    if !src.exists() {
        return Ok(MoveReport { moved: 0 });
    }

    let moved = count_files(&src)?;
    let dest = project_path.join(".clickweave").join("skills");
    fs::create_dir_all(&dest)?;

    match fs::rename(&src, &dest) {
        Ok(()) => {}
        Err(_) => {
            copy_dir_with_integrity_check(&src, &dest)?;
            fs::remove_dir_all(&src)?;
        }
    }

    Ok(MoveReport { moved })
}

fn copy_dir_with_integrity_check(src: &Path, dest: &Path) -> Result<(), SkillError> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_with_integrity_check(&src_path, &dest_path)?;
        } else if file_type.is_file() {
            let bytes = fs::read(&src_path)?;
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&dest_path, &bytes)?;
            let written = fs::read(&dest_path)?;
            if written != bytes {
                return Err(SkillError::InvalidFrontmatter(format!(
                    "copied skill file integrity check failed for {}",
                    dest_path.display()
                )));
            }
        }
    }
    Ok(())
}

fn count_files(dir: &Path) -> Result<usize, SkillError> {
    let mut count = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            count += count_files(&entry.path())?;
        } else if file_type.is_file() {
            count += 1;
        }
    }
    Ok(count)
}

pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !out.is_empty() {
            out.push('-');
            last_was_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_lowercases_and_collapses_non_alphanumerics() {
        assert_eq!(slugify("Open Vesna's Chat!"), "open-vesna-s-chat");
        assert_eq!(slugify("multi   spaces"), "multi-spaces");
        assert_eq!(slugify("trailing!!!"), "trailing");
    }

    #[test]
    fn legacy_basename_combines_slug_and_version() {
        let skill = sample_skill_minimal("open-vesna-chat", 3);
        assert_eq!(legacy_basename(&skill), "open-vesna-chat-v3.md");
    }

    #[test]
    fn move_skills_to_project_moves_unsaved_skill_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let app_data_skills_root = tmp.path().join("app-data").join("skills");
        let workflow_id = "550e8400-e29b-41d4-a716-446655440000";
        let src = app_data_skills_root.join(workflow_id);
        fs::create_dir_all(src.join("nested")).unwrap();
        fs::write(src.join("alpha-v1.md"), b"alpha").unwrap();
        fs::write(src.join("nested").join("beta-v1.md"), b"beta").unwrap();

        let project = tmp.path().join("saved-project");
        let report = move_skills_to_project(&app_data_skills_root, workflow_id, &project).unwrap();

        assert_eq!(report, MoveReport { moved: 2 });
        assert!(!src.exists());
        assert_eq!(
            fs::read(project.join(".clickweave/skills/alpha-v1.md")).unwrap(),
            b"alpha"
        );
        assert_eq!(
            fs::read(project.join(".clickweave/skills/nested/beta-v1.md")).unwrap(),
            b"beta"
        );
    }

    #[test]
    fn move_skills_to_project_is_noop_when_unsaved_dir_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let report = move_skills_to_project(
            &tmp.path().join("app-data").join("skills"),
            "missing",
            &tmp.path().join("saved-project"),
        )
        .unwrap();

        assert_eq!(report, MoveReport { moved: 0 });
        assert!(!tmp.path().join("saved-project/.clickweave/skills").exists());
    }

    #[cfg(unix)]
    #[test]
    fn recent_write_matches_canonicalized_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real-skills");
        let alias_dir = tmp.path().join("alias-skills");
        fs::create_dir_all(&real_dir).unwrap();
        std::os::unix::fs::symlink(&real_dir, &alias_dir).unwrap();

        let store = SkillStore::new(alias_dir);
        let path = store
            .write_skill(&sample_skill_minimal("alias", 1))
            .unwrap();
        let canonical_path = fs::canonicalize(&path).unwrap();

        assert!(
            store.was_recently_written(&canonical_path),
            "recent-write suppression should survive watcher canonicalization"
        );
    }

    #[test]
    fn write_atomic_multi_file_lands_all_files_at_once() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SkillStore::new(tmp.path().to_path_buf());
        let skill_id = "skl_atomic".to_string();
        let files: Vec<(PathBuf, Vec<u8>)> = vec![
            (PathBuf::from("SKILL.md"), b"---\nname: a\n---\n".to_vec()),
            (PathBuf::from("replay.json"), b"{}".to_vec()),
        ];
        store
            .write_atomic_multi_file(&skill_id, files, None)
            .unwrap();
        assert_eq!(
            fs::read(tmp.path().join(&skill_id).join("SKILL.md")).unwrap(),
            b"---\nname: a\n---\n",
        );
        assert_eq!(
            fs::read(tmp.path().join(&skill_id).join("replay.json")).unwrap(),
            b"{}",
        );
        // Journal directory cleaned up after a successful commit.
        assert!(!tmp.path().join(&skill_id).join(TX_DIR).exists());
    }

    #[test]
    fn recover_atomic_writes_replays_committed_transaction() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SkillStore::new(tmp.path().to_path_buf());
        let skill_id = "skl_recovery".to_string();
        let skill_dir = tmp.path().join(&skill_id);
        let pending = skill_dir.join(TX_DIR).join(TX_PENDING);
        fs::create_dir_all(&pending).unwrap();
        fs::write(pending.join("SKILL.md.new"), b"recovered").unwrap();

        let manifest = AtomicWriteManifest {
            files: vec![AtomicWriteFile {
                relative: PathBuf::from("SKILL.md"),
            }],
        };
        fs::write(
            skill_dir.join(TX_DIR).join(TX_MANIFEST),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        // Synthesize a crash-after-commit-marker state.
        fs::write(skill_dir.join(TX_DIR).join(TX_COMMIT), b"").unwrap();

        store.recover_atomic_writes(&skill_id).unwrap();
        assert_eq!(fs::read(skill_dir.join("SKILL.md")).unwrap(), b"recovered");
        assert!(!skill_dir.join(TX_DIR).exists());
    }

    #[test]
    fn recover_atomic_writes_rolls_back_pending_without_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SkillStore::new(tmp.path().to_path_buf());
        let skill_id = "skl_rollback".to_string();
        let skill_dir = tmp.path().join(&skill_id);
        let pending = skill_dir.join(TX_DIR).join(TX_PENDING);
        fs::create_dir_all(&pending).unwrap();
        fs::write(pending.join("SKILL.md.new"), b"discarded").unwrap();
        store.recover_atomic_writes(&skill_id).unwrap();
        // No live SKILL.md and no journal left behind.
        assert!(!skill_dir.join("SKILL.md").exists());
        assert!(!skill_dir.join(TX_DIR).exists());
    }

    #[test]
    fn write_replay_writes_sidecar_under_skill_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SkillStore::new(tmp.path().to_path_buf());
        let skill_id = "skl_replay".to_string();
        let replay = ReplayJson {
            skill_id: skill_id.clone(),
            schema_version: 1,
            ..Default::default()
        };
        let path = store.write_replay(&skill_id, &replay).unwrap();
        assert!(path.exists());
        assert_eq!(path, tmp.path().join(&skill_id).join("replay.json"),);
    }

    fn sample_skill_minimal(id: &str, version: u32) -> Skill {
        use crate::agent::skills::types::*;
        Skill {
            id: id.into(),
            version,
            state: SkillState::Draft,
            scope: SkillScope::ProjectLocal,
            name: "test".into(),
            description: "desc".into(),
            tags: vec![],
            subgoal_text: "open chat".into(),
            subgoal_signature: SubgoalSignature("sig".into()),
            applicability: ApplicabilityHints {
                apps: vec![],
                hosts: vec![],
                signature: ApplicabilitySignature("appsig".into()),
            },
            parameter_schema: vec![],
            action_sketch: vec![],
            outputs: vec![],
            outcome_predicate: OutcomePredicate::SubgoalCompleted {
                post_state_world_model_signature: None,
            },
            provenance: vec![],
            stats: SkillStats::default(),
            edited_by_user: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            produced_node_ids: vec![],
            body: String::new(),
            schema_version: super::super::SKILL_SCHEMA_VERSION,
            variables: vec![],
            sections: vec![],
            replay: None,
        }
    }
}
