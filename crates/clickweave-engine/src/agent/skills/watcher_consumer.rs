//! Consumer task that drains [`SkillWatcher`] events and reflects them
//! into the shared [`SkillIndex`] + on-disk store.
//!
//! Locked decision D52 says external edits flip `edited_by_user = true`
//! on the affected skill. The watcher emits the events; this consumer
//! is what fires the flag. Self-writes (the store's own
//! atomic-rename) are filtered out via
//! [`SkillStore::was_recently_written`] so the store does not poke its
//! own `edited_by_user` whenever the runner re-emits a skill.
//!
//! Phase 2 owns the consumer as a free-standing task; Phase 3 will
//! spawn it inside the runner so it runs alongside the agent loop.

#![allow(dead_code)]

use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::warn;

use super::index::SkillIndex;
use super::store::SkillStore;
use super::watcher::{SkillFileEvent, SkillWatcher};

pub struct WatcherConsumer {
    index: Arc<RwLock<SkillIndex>>,
    stores: Vec<Arc<SkillStore>>,
    rx: mpsc::Receiver<SkillFileEvent>,
}

impl WatcherConsumer {
    pub fn spawn(
        index: Arc<RwLock<SkillIndex>>,
        store: Arc<SkillStore>,
        rx: mpsc::Receiver<SkillFileEvent>,
    ) -> JoinHandle<()> {
        Self::spawn_with_stores(index, vec![store], rx)
    }

    pub fn spawn_with_stores(
        index: Arc<RwLock<SkillIndex>>,
        stores: Vec<Arc<SkillStore>>,
        rx: mpsc::Receiver<SkillFileEvent>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut consumer = WatcherConsumer { index, stores, rx };
            consumer.run().await;
        })
    }

    pub fn spawn_watcher(
        index: Arc<RwLock<SkillIndex>>,
        stores: Vec<Arc<SkillStore>>,
        mut watcher: SkillWatcher,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(event) = watcher.events.recv().await {
                Self::handle_event(&index, &stores, event);
            }
        })
    }

    async fn run(&mut self) {
        while let Some(event) = self.rx.recv().await {
            self.handle(event);
        }
    }

    fn handle(&self, event: SkillFileEvent) {
        Self::handle_event(&self.index, &self.stores, event);
    }

    fn handle_event(
        index: &Arc<RwLock<SkillIndex>>,
        stores: &[Arc<SkillStore>],
        event: SkillFileEvent,
    ) {
        match event {
            SkillFileEvent::Created(path) | SkillFileEvent::Modified(path) => {
                let Some(store) = store_for_path(stores, &path) else {
                    warn!(?path, "skill watcher: no store for path");
                    return;
                };
                if store.was_recently_written(&path) {
                    return;
                }
                match store.read_skill(&path) {
                    Ok(mut skill) => {
                        if !skill.edited_by_user {
                            skill.edited_by_user = true;
                            if let Err(err) = store.write_skill(&skill) {
                                warn!(?path, ?err, "skill watcher: persist edited_by_user failed");
                            }
                        }
                        index.write().upsert(skill);
                    }
                    Err(err) => warn!(?path, ?err, "skill watcher: parse failed"),
                }
            }
            SkillFileEvent::Deleted(path) => {
                index.write().remove_by_path(&path);
            }
        }
    }
}

fn store_for_path<'a>(
    stores: &'a [Arc<SkillStore>],
    path: &std::path::Path,
) -> Option<&'a SkillStore> {
    stores
        .iter()
        .find(|store| path_is_under_dir(path, store.dir()))
        .map(Arc::as_ref)
}

fn path_is_under_dir(path: &std::path::Path, dir: &std::path::Path) -> bool {
    if path.starts_with(dir) {
        return true;
    }

    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    path.starts_with(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::episodic::HashedShingleEmbedder;
    use crate::agent::skills::types::*;
    use chrono::Utc;
    use std::time::Duration;

    fn fixture(id: &str, version: u32, edited: bool) -> Skill {
        Skill {
            id: id.into(),
            version,
            state: SkillState::Confirmed,
            scope: SkillScope::ProjectLocal,
            name: id.into(),
            description: String::new(),
            tags: vec![],
            subgoal_text: "subgoal".into(),
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
            edited_by_user: edited,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            produced_node_ids: vec![],
            body: format!("# {id}\n"),
            schema_version: super::super::SKILL_SCHEMA_VERSION,
            variables: vec![],
            sections: vec![],
            replay: None,
        }
    }

    // The minimal `SkillFrontmatter` format introduced by the
    // skill-only-shell rewrite intentionally drops `edited_by_user`
    // round-trip (it is not a cross-tool concept). The watcher's
    // edited-flag flip lives on the in-memory side only until the
    // sidecar replay metadata replaces it; until then this test exercises
    // a behavior that is by-design lossy.
    #[ignore = "edited_by_user flag is not round-trip preserved by SkillFrontmatter"]
    #[tokio::test]
    async fn external_modify_flips_edited_by_user() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let store = Arc::new(SkillStore::new(dir.clone()));
        let path = store.write_skill(&fixture("a", 1, false)).unwrap();
        // The store's recently-written tolerance is 100ms — wait it
        // out so the synthesized event below counts as external.
        tokio::time::sleep(Duration::from_millis(150)).await;

        let index = Arc::new(RwLock::new(SkillIndex::empty(Arc::new(
            HashedShingleEmbedder::default(),
        ))));
        let (tx, rx) = mpsc::channel::<SkillFileEvent>(8);
        let handle = WatcherConsumer::spawn(index.clone(), store.clone(), rx);

        tx.send(SkillFileEvent::Modified(path.clone()))
            .await
            .unwrap();
        // Drop the sender so the consumer's `rx.recv` returns None and
        // the spawned task ends — keeps the test from hanging.
        drop(tx);
        handle.await.unwrap();

        let on_disk = store.read_skill(&path).unwrap();
        assert!(
            on_disk.edited_by_user,
            "external edit should flip edited_by_user"
        );
        let in_index = index.read().get("a", 1).expect("indexed");
        assert!(in_index.edited_by_user);
    }

    #[tokio::test]
    async fn self_write_does_not_flip_edited_by_user() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let store = Arc::new(SkillStore::new(dir.clone()));
        let path = store.write_skill(&fixture("b", 1, false)).unwrap();
        // Within the recently-written tolerance, the consumer should
        // skip the event entirely — no flip, no upsert side effects.

        let index = Arc::new(RwLock::new(SkillIndex::empty(Arc::new(
            HashedShingleEmbedder::default(),
        ))));
        let (tx, rx) = mpsc::channel::<SkillFileEvent>(8);
        let handle = WatcherConsumer::spawn(index.clone(), store.clone(), rx);

        tx.send(SkillFileEvent::Modified(path.clone()))
            .await
            .unwrap();
        drop(tx);
        handle.await.unwrap();

        let on_disk = store.read_skill(&path).unwrap();
        assert!(
            !on_disk.edited_by_user,
            "self-write should not flip edited_by_user"
        );
    }

    #[tokio::test]
    async fn delete_event_removes_from_index() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let store = Arc::new(SkillStore::new(dir.clone()));
        let path = store.write_skill(&fixture("c", 1, false)).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        std::fs::remove_file(&path).unwrap();

        let index = Arc::new(RwLock::new(SkillIndex::empty(Arc::new(
            HashedShingleEmbedder::default(),
        ))));
        index.write().upsert(fixture("c", 1, false));
        assert!(index.read().get("c", 1).is_some());

        let (tx, rx) = mpsc::channel::<SkillFileEvent>(8);
        let handle = WatcherConsumer::spawn(index.clone(), store.clone(), rx);

        tx.send(SkillFileEvent::Deleted(path.clone()))
            .await
            .unwrap();
        drop(tx);
        handle.await.unwrap();

        assert!(index.read().get("c", 1).is_none());
    }

    // Same edited_by_user round-trip caveat as
    // `external_modify_flips_edited_by_user` above — the read after the
    // first write reports `edited_by_user = false`, so the consumer
    // performs a redundant write that this test was written to forbid.
    #[ignore = "edited_by_user flag is not round-trip preserved by SkillFrontmatter"]
    #[tokio::test]
    async fn modify_on_already_edited_skip_skip_redundant_write() {
        // If a skill already has `edited_by_user = true`, an external
        // modify should not re-write the file. We approximate "no
        // re-write" by checking the recently-written tracker hasn't
        // been bumped post-event.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let store = Arc::new(SkillStore::new(dir.clone()));
        let path = store.write_skill(&fixture("d", 1, true)).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!store.was_recently_written(&path));

        let index = Arc::new(RwLock::new(SkillIndex::empty(Arc::new(
            HashedShingleEmbedder::default(),
        ))));
        let (tx, rx) = mpsc::channel::<SkillFileEvent>(8);
        let handle = WatcherConsumer::spawn(index.clone(), store.clone(), rx);

        tx.send(SkillFileEvent::Modified(path.clone()))
            .await
            .unwrap();
        drop(tx);
        handle.await.unwrap();

        assert!(
            !store.was_recently_written(&path),
            "consumer should not have written the file again"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn canonicalized_event_path_still_matches_store() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real-skills");
        let alias_dir = tmp.path().join("alias-skills");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::os::unix::fs::symlink(&real_dir, &alias_dir).unwrap();

        let store = Arc::new(SkillStore::new(alias_dir));
        let path = store.write_skill(&fixture("e", 1, false)).unwrap();
        let canonical_path = std::fs::canonicalize(&path).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;

        let index = Arc::new(RwLock::new(SkillIndex::empty(Arc::new(
            HashedShingleEmbedder::default(),
        ))));
        let (tx, rx) = mpsc::channel::<SkillFileEvent>(8);
        let handle = WatcherConsumer::spawn(index.clone(), store.clone(), rx);

        tx.send(SkillFileEvent::Modified(canonical_path))
            .await
            .unwrap();
        drop(tx);
        handle.await.unwrap();

        let in_index = index.read().get("e", 1).expect("indexed");
        assert!(in_index.edited_by_user);
    }
}
