//! `notify`-crate file watcher for the on-disk skill store.
//!
//! Spawned once per agent run against the project + (optional) global
//! skills directories. Translates raw OS filesystem events into
//! [`SkillFileEvent`] variants and pushes them onto a bounded mpsc
//! channel for the watcher consumer (see [`watcher_consumer`]) to
//! drain. The consumer compares each event against
//! `SkillStore::was_recently_written` to skip self-writes that the
//! store itself emitted.
//!
//! [`watcher_consumer`]: super::watcher_consumer

#![allow(dead_code)]

use std::path::PathBuf;

use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher, event::ModifyKind};
use tokio::sync::mpsc;

use super::store::SKILL_MD;
use super::types::SkillError;

const EVENT_CHANNEL_BUFFER: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillFileEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
}

pub struct SkillWatcher {
    _watcher: RecommendedWatcher,
    pub events: mpsc::Receiver<SkillFileEvent>,
}

impl SkillWatcher {
    /// Spawn a watcher over `dirs`. Each path is watched recursively
    /// because skills live one level deep at `<dir>/<skill_id>/SKILL.md`,
    /// alongside per-skill sidecars (`replay.json`) and the `.tx/`
    /// journal directory. The non-`SKILL.md` paths are filtered out in
    /// [`classify_event`]. Missing directories are skipped silently;
    /// the consumer treats an absent skills tree as an empty index.
    pub fn spawn(dirs: Vec<PathBuf>) -> Result<Self, SkillError> {
        let (tx, rx) = mpsc::channel(EVENT_CHANNEL_BUFFER);

        let event_tx = tx;
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                let Ok(event) = res else {
                    return;
                };
                for path in event.paths {
                    let Some(skill_event) = classify_event(&event.kind, path) else {
                        continue;
                    };
                    // The notify callback runs on the watcher thread,
                    // not a tokio runtime — `try_send` avoids blocking
                    // and silently drops on full buffer (the consumer's
                    // backlog is bounded by design).
                    let _ = event_tx.try_send(skill_event);
                }
            },
            Config::default(),
        )
        .map_err(|err| SkillError::InvalidFrontmatter(format!("notify init failed: {err}")))?;

        for dir in dirs {
            if !dir.exists() {
                continue;
            }
            watcher
                .watch(&dir, RecursiveMode::Recursive)
                .map_err(|err| {
                    SkillError::InvalidFrontmatter(format!(
                        "notify watch({}) failed: {err}",
                        dir.display()
                    ))
                })?;
        }

        Ok(Self {
            _watcher: watcher,
            events: rx,
        })
    }
}

fn classify_event(kind: &EventKind, path: PathBuf) -> Option<SkillFileEvent> {
    // Only the canonical `SKILL.md` body file participates in the
    // index. Per-skill sidecars (`replay.json`, etc.) and `.tx/`
    // journal entries are filtered out.
    if path.file_name().and_then(|n| n.to_str()) != Some(SKILL_MD) {
        return None;
    }
    match kind {
        EventKind::Create(_) => Some(SkillFileEvent::Created(path)),
        EventKind::Modify(ModifyKind::Data(_)) | EventKind::Modify(ModifyKind::Any) => {
            Some(SkillFileEvent::Modified(path))
        }
        EventKind::Modify(ModifyKind::Name(_)) => {
            // Atomic-rename targets surface as Name modifications.
            // Treat them as Created — the consumer reads the file
            // fresh either way.
            Some(SkillFileEvent::Created(path))
        }
        EventKind::Remove(_) => Some(SkillFileEvent::Deleted(path)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    use tokio::time::timeout;

    use super::*;

    /// FSEvents on macOS resolves `/var/...` to `/private/var/...`
    /// when reporting paths. Canonicalize both sides so equality
    /// works regardless of which form `tempfile` returns.
    fn canon(p: &Path) -> std::path::PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }

    /// Drain events until we see one matching `expected_canon` whose
    /// kind is in `kinds`, or the timeout elapses. Notify backends
    /// often emit stray metadata-modify events around real changes;
    /// ignoring the non-matching ones keeps the test deterministic.
    async fn await_event_for(
        watcher: &mut SkillWatcher,
        expected_canon: &Path,
        kinds: &[&str],
    ) -> Option<SkillFileEvent> {
        loop {
            let event = timeout(Duration::from_secs(3), watcher.events.recv())
                .await
                .ok()??;
            let (path, kind) = match &event {
                SkillFileEvent::Created(p) => (p.clone(), "Created"),
                SkillFileEvent::Modified(p) => (p.clone(), "Modified"),
                SkillFileEvent::Deleted(p) => (p.clone(), "Deleted"),
            };
            if canon(&path) == expected_canon && kinds.contains(&kind) {
                return Some(event);
            }
        }
    }

    #[tokio::test]
    async fn observes_create_modify_delete_for_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();

        let mut watcher = SkillWatcher::spawn(vec![dir.clone()]).unwrap();
        // FSEvents has a small startup delay; without this, the first
        // write occasionally lands before the watcher subscribes.
        tokio::time::sleep(Duration::from_millis(150)).await;

        let skill_dir = dir.join("skl-create-modify-delete");
        fs::create_dir_all(&skill_dir).unwrap();
        let path = skill_dir.join(SKILL_MD);
        let canon_path = {
            fs::write(&path, "---\n---\nbody\n").unwrap();
            canon(&path)
        };
        assert!(
            await_event_for(&mut watcher, &canon_path, &["Created", "Modified"])
                .await
                .is_some(),
            "expected Created/Modified for fresh file"
        );

        // Small breath between writes — APFS coalesces simultaneous
        // events into a single notification otherwise.
        tokio::time::sleep(Duration::from_millis(80)).await;
        fs::write(&path, "---\n---\nupdated body\n").unwrap();
        assert!(
            await_event_for(&mut watcher, &canon_path, &["Created", "Modified"])
                .await
                .is_some(),
            "expected Modified for existing file"
        );

        tokio::time::sleep(Duration::from_millis(80)).await;
        fs::remove_file(&path).unwrap();
        assert!(
            await_event_for(&mut watcher, &canon_path, &["Deleted", "Modified"])
                .await
                .is_some(),
            "expected Deleted (or Modified) for removed file"
        );
    }

    #[tokio::test]
    async fn ignores_non_skill_md_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();

        let mut watcher = SkillWatcher::spawn(vec![dir.clone()]).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;

        let skill_dir = dir.join("skl-non-skill-md");
        fs::create_dir_all(&skill_dir).unwrap();
        // Sidecars (`replay.json`) and arbitrary `.md` files at the
        // project root must not surface to the consumer.
        fs::write(dir.join("notes.md"), "hi").unwrap();
        fs::write(skill_dir.join("replay.json"), "{}").unwrap();
        let result = timeout(Duration::from_millis(400), watcher.events.recv()).await;
        assert!(
            result.is_err(),
            "expected no events for non-SKILL.md files, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn missing_directory_is_silently_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().to_path_buf();
        let missing_dir = tmp.path().join("does-not-exist");

        let mut watcher = SkillWatcher::spawn(vec![missing_dir, real_dir.clone()]).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;

        let skill_dir = real_dir.join("skl-missing-dir");
        fs::create_dir_all(&skill_dir).unwrap();
        let path = skill_dir.join(SKILL_MD);
        fs::write(&path, "---\n---\nx\n").unwrap();
        let canon_path = canon(&path);
        assert!(
            await_event_for(&mut watcher, &canon_path, &["Created", "Modified"])
                .await
                .is_some(),
            "watcher should still emit events on the real dir"
        );
    }
}
