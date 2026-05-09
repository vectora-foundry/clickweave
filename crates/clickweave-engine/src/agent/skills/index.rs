//! In-memory skill index.
//!
//! Built once per agent run from the on-disk store and held behind an
//! `Arc<RwLock<_>>` shared between the runner, the file watcher
//! consumer, and the LLM-proposal task. Two lookup paths matter:
//! `(id, version)` for replay dispatch and `subgoal_signature` for
//! retrieval at `push_subgoal` boundaries.
//!
//! Phase 2 lands the build + lookup surface with a placeholder scoring
//! formula (`1.0` on signature match, `0.0` otherwise). Phase 3
//! replaces the scorer with the rich cross-tier merge that consumes
//! the embedder field on the index.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tracing::warn;

use super::retrieval::{ScoringWeights, is_retrieval_eligible, merge_tiers, score};
use super::store::{SkillStore, legacy_basename};
use super::types::{
    ApplicabilitySignature, RetrievedSkill, Skill, SkillContext, SkillError, SkillState,
    SubgoalSignature,
};
use crate::agent::episodic::HashedShingleEmbedder;
use crate::agent::episodic::embedder::Embedder;

pub struct SkillIndex {
    by_id: HashMap<(String, u32), Arc<Skill>>,
    by_subgoal_signature: HashMap<SubgoalSignature, Vec<(String, u32)>>,
    embedder: Arc<HashedShingleEmbedder>,
    project_dir: PathBuf,
    global_dir: Option<PathBuf>,
}

impl std::fmt::Debug for SkillIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillIndex")
            .field("len", &self.by_id.len())
            .field("project_dir", &self.project_dir)
            .field("global_dir", &self.global_dir)
            .finish()
    }
}

impl SkillIndex {
    pub fn build(
        ctx: &SkillContext,
        embedder: Arc<HashedShingleEmbedder>,
    ) -> Result<Self, SkillError> {
        let mut idx = Self::empty_with_paths(
            embedder,
            ctx.project_skills_dir.clone(),
            ctx.global_skills_dir.clone(),
        );
        if let Some(global) = ctx.global_skills_dir.as_ref()
            && global.exists()
        {
            idx.load_dir(global);
        }
        if ctx.project_skills_dir.exists() {
            idx.load_dir(&ctx.project_skills_dir);
        }
        Ok(idx)
    }

    pub fn empty(embedder: Arc<HashedShingleEmbedder>) -> Self {
        Self::empty_with_paths(embedder, PathBuf::new(), None)
    }

    fn empty_with_paths(
        embedder: Arc<HashedShingleEmbedder>,
        project_dir: PathBuf,
        global_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            by_id: HashMap::new(),
            by_subgoal_signature: HashMap::new(),
            embedder,
            project_dir,
            global_dir,
        }
    }

    fn load_dir(&mut self, dir: &PathBuf) {
        let store = SkillStore::new(dir.clone());
        match store.list_files() {
            Ok(paths) => {
                for path in paths {
                    match store.read_skill(&path) {
                        Ok(skill) => self.upsert(skill),
                        Err(err) => {
                            warn!(?path, ?err, "skill index: skipping malformed skill file");
                        }
                    }
                }
            }
            Err(err) => warn!(?dir, ?err, "skill index: list_files failed"),
        }
    }

    pub fn get(&self, id: &str, version: u32) -> Option<Arc<Skill>> {
        self.by_id.get(&(id.to_string(), version)).cloned()
    }

    pub fn upsert(&mut self, skill: Skill) {
        let key = (skill.id.clone(), skill.version);
        // Drop the previous (id, version)'s reverse-index entry so a
        // re-upsert (e.g. after the watcher consumer flips
        // edited_by_user) does not double-list under the same signature.
        let prev_sig = self.by_id.get(&key).map(|s| s.subgoal_signature.clone());
        if let Some(sig) = prev_sig {
            self.remove_subgoal_pointer(&sig, &key);
        }
        self.by_subgoal_signature
            .entry(skill.subgoal_signature.clone())
            .or_default()
            .push(key.clone());
        self.by_id.insert(key, Arc::new(skill));
    }

    pub fn remove(&mut self, id: &str, version: u32) {
        let key = (id.to_string(), version);
        if let Some(prev) = self.by_id.remove(&key) {
            let sig = prev.subgoal_signature.clone();
            self.remove_subgoal_pointer(&sig, &key);
        }
    }

    pub fn remove_by_path(&mut self, path: &Path) -> bool {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        let keys: Vec<_> = self
            .by_id
            .iter()
            .filter_map(|(key, skill)| {
                if legacy_basename(skill.as_ref()) == file_name {
                    Some(key.clone())
                } else {
                    None
                }
            })
            .collect();
        let removed = !keys.is_empty();
        for (id, version) in keys {
            self.remove(&id, version);
        }
        removed
    }

    fn remove_subgoal_pointer(&mut self, sig: &SubgoalSignature, key: &(String, u32)) {
        if let Some(entries) = self.by_subgoal_signature.get_mut(sig) {
            entries.retain(|k| k != key);
            if entries.is_empty() {
                self.by_subgoal_signature.remove(sig);
            }
        }
    }

    /// Return up to `k` skills whose `subgoal_signature` matches and
    /// whose state is `Confirmed` or `Promoted` (drafts are not
    /// retrieval-eligible). Scores combine signature match, text
    /// similarity (subgoal text via the shared embedder), occurrence
    /// boost, success rate, and time decay; the cross-tier merge then
    /// applies the project-local multiplier and global cap.
    pub fn lookup(
        &self,
        subgoal_sig: &SubgoalSignature,
        applicability_sig: &ApplicabilitySignature,
        k: usize,
    ) -> Vec<RetrievedSkill> {
        self.lookup_at(subgoal_sig, applicability_sig, "", k, Utc::now())
    }

    /// Lookup variant exposing the query subgoal text (so the text
    /// similarity component is meaningful) and an explicit `now` for
    /// deterministic test coverage of the time-decay term.
    pub fn lookup_at(
        &self,
        subgoal_sig: &SubgoalSignature,
        applicability_sig: &ApplicabilitySignature,
        query_subgoal_text: &str,
        k: usize,
        now: DateTime<Utc>,
    ) -> Vec<RetrievedSkill> {
        if k == 0 {
            return Vec::new();
        }
        let Some(keys) = self.by_subgoal_signature.get(subgoal_sig) else {
            return Vec::new();
        };
        let weights = ScoringWeights::default();
        let query_embedding = if query_subgoal_text.is_empty() {
            Vec::new()
        } else {
            self.embedder.embed(query_subgoal_text)
        };
        let scored: Vec<_> = keys
            .iter()
            .filter_map(|key| self.by_id.get(key))
            .filter(|skill| is_retrieval_eligible(skill))
            .filter(|skill| &skill.applicability.signature == applicability_sig)
            .map(|skill| {
                let skill_embedding = self.embedder.embed(&skill.subgoal_text);
                let raw = score(
                    skill,
                    subgoal_sig,
                    &query_embedding,
                    &skill_embedding,
                    &weights,
                    now,
                );
                let scope = skill.scope;
                (
                    scope,
                    raw,
                    RetrievedSkill {
                        skill: skill.clone(),
                        score: raw,
                    },
                )
            })
            .collect();
        merge_tiers(scored, k)
    }

    pub fn mark_invoked(&mut self, id: &str, version: u32, when: DateTime<Utc>) {
        let key = (id.to_string(), version);
        if let Some(entry) = self.by_id.get_mut(&key) {
            // `Arc<Skill>` shares state with retrieval consumers; clone
            // the inner value, mutate, and replace the Arc. Cheap
            // relative to the cost of the disk write that follows.
            let mut updated = (**entry).clone();
            updated.stats.last_invoked_at = Some(when);
            *entry = Arc::new(updated);
        }
    }

    /// Return every skill (regardless of state) whose `subgoal_signature`
    /// matches `sig`. Used by the extractor to detect a matching version
    /// family before deciding whether to merge or fork. Includes drafts —
    /// extraction merges into draft entries even though retrieval skips
    /// them.
    pub fn skills_with_signature(&self, sig: &SubgoalSignature) -> Vec<Arc<Skill>> {
        self.by_subgoal_signature
            .get(sig)
            .map(|keys| {
                keys.iter()
                    .filter_map(|key| self.by_id.get(key))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn skills_in_state(&self, state: SkillState) -> Vec<Arc<Skill>> {
        self.by_id
            .values()
            .filter(|skill| skill.state == state)
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn embedder(&self) -> &Arc<HashedShingleEmbedder> {
        &self.embedder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::skills::types::{
        ApplicabilityHints, ApplicabilitySignature, OutcomePredicate, Skill, SkillScope, SkillStats,
    };

    fn skill_with(id: &str, version: u32, sig: &str, state: SkillState) -> Skill {
        Skill {
            id: id.into(),
            version,
            state,
            scope: SkillScope::ProjectLocal,
            name: id.into(),
            description: String::new(),
            tags: vec![],
            subgoal_text: format!("subgoal for {id}"),
            subgoal_signature: SubgoalSignature(sig.into()),
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
            produced_node_ids: vec![],
            body: String::new(),
            schema_version: super::super::SKILL_SCHEMA_VERSION,
            variables: vec![],
            sections: vec![],
            replay: None,
        }
    }

    #[test]
    fn empty_index_returns_no_candidates() {
        let idx = SkillIndex::empty(Arc::new(HashedShingleEmbedder::default()));
        let sig = SubgoalSignature("missing".into());
        let app_sig = ApplicabilitySignature("appsig".into());
        assert!(idx.lookup(&sig, &app_sig, 5).is_empty());
    }

    #[test]
    fn build_over_temp_dir_loads_all_files() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let store = SkillStore::new(project_dir.clone());
        store
            .write_skill(&skill_with("a", 1, "sig-a", SkillState::Confirmed))
            .unwrap();
        store
            .write_skill(&skill_with("b", 1, "sig-b", SkillState::Confirmed))
            .unwrap();
        store
            .write_skill(&skill_with("c", 1, "sig-c", SkillState::Draft))
            .unwrap();

        let ctx = SkillContext {
            enabled: true,
            project_skills_dir: project_dir,
            global_skills_dir: None,
            project_id: "p".into(),
        };
        let idx = SkillIndex::build(&ctx, Arc::new(HashedShingleEmbedder::default())).unwrap();
        assert_eq!(idx.len(), 3);
        assert!(idx.get("a", 1).is_some());
        assert!(idx.get("b", 1).is_some());
        assert!(idx.get("c", 1).is_some());
    }

    #[test]
    fn build_prefers_project_local_when_global_copy_has_same_id_version() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().join("project");
        let global_dir = tmp.path().join("global");
        let project_store = SkillStore::new(project_dir.clone());
        let global_store = SkillStore::new(global_dir.clone());

        let mut local = skill_with("same", 1, "sig", SkillState::Confirmed);
        local.scope = SkillScope::ProjectLocal;
        local.description = "local".into();
        let mut global = skill_with("same", 1, "sig", SkillState::Promoted);
        global.scope = SkillScope::Global;
        global.description = "global".into();
        global_store.write_skill(&global).unwrap();
        project_store.write_skill(&local).unwrap();

        let ctx = SkillContext {
            enabled: true,
            project_skills_dir: project_dir,
            global_skills_dir: Some(global_dir),
            project_id: "p".into(),
        };
        let idx = SkillIndex::build(&ctx, Arc::new(HashedShingleEmbedder::default())).unwrap();
        let selected = idx.get("same", 1).expect("skill loaded");

        assert_eq!(selected.scope, SkillScope::ProjectLocal);
        assert_eq!(selected.description, "local");
    }

    #[test]
    fn lookup_excludes_draft_state() {
        let mut idx = SkillIndex::empty(Arc::new(HashedShingleEmbedder::default()));
        idx.upsert(skill_with("draft", 1, "sig-a", SkillState::Draft));
        idx.upsert(skill_with("conf", 1, "sig-a", SkillState::Confirmed));

        let hits = idx.lookup(
            &SubgoalSignature("sig-a".into()),
            &ApplicabilitySignature("appsig".into()),
            5,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].skill.id, "conf");
    }

    #[test]
    fn lookup_respects_k() {
        let mut idx = SkillIndex::empty(Arc::new(HashedShingleEmbedder::default()));
        for i in 0..5 {
            idx.upsert(skill_with(
                &format!("s{i}"),
                1,
                "sig-shared",
                SkillState::Confirmed,
            ));
        }
        let hits = idx.lookup(
            &SubgoalSignature("sig-shared".into()),
            &ApplicabilitySignature("appsig".into()),
            2,
        );
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn promoted_skills_are_retrievable() {
        let mut idx = SkillIndex::empty(Arc::new(HashedShingleEmbedder::default()));
        idx.upsert(skill_with("p", 1, "sig", SkillState::Promoted));
        let hits = idx.lookup(
            &SubgoalSignature("sig".into()),
            &ApplicabilitySignature("appsig".into()),
            5,
        );
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn remove_drops_the_skill_and_its_signature_pointer() {
        let mut idx = SkillIndex::empty(Arc::new(HashedShingleEmbedder::default()));
        idx.upsert(skill_with("a", 1, "sig", SkillState::Confirmed));
        idx.remove("a", 1);
        assert!(idx.get("a", 1).is_none());
        let hits = idx.lookup(
            &SubgoalSignature("sig".into()),
            &ApplicabilitySignature("appsig".into()),
            5,
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn upsert_with_changed_signature_repoints_reverse_index() {
        let mut idx = SkillIndex::empty(Arc::new(HashedShingleEmbedder::default()));
        idx.upsert(skill_with("a", 1, "sig-old", SkillState::Confirmed));
        // Same (id, version) re-insert under a new signature — the
        // reverse-index should drop the old entry, not double-list.
        idx.upsert(skill_with("a", 1, "sig-new", SkillState::Confirmed));

        assert!(
            idx.lookup(
                &SubgoalSignature("sig-old".into()),
                &ApplicabilitySignature("appsig".into()),
                5,
            )
            .is_empty()
        );
        assert_eq!(
            idx.lookup(
                &SubgoalSignature("sig-new".into()),
                &ApplicabilitySignature("appsig".into()),
                5,
            )
            .len(),
            1,
        );
    }

    #[test]
    fn lookup_filters_by_applicability_signature() {
        let mut idx = SkillIndex::empty(Arc::new(HashedShingleEmbedder::default()));
        let mut telegram = skill_with("telegram", 1, "sig-shared", SkillState::Confirmed);
        telegram.applicability.signature = ApplicabilitySignature("app-telegram".into());
        let mut slack = skill_with("slack", 1, "sig-shared", SkillState::Confirmed);
        slack.applicability.signature = ApplicabilitySignature("app-slack".into());
        idx.upsert(telegram);
        idx.upsert(slack);

        let hits = idx.lookup(
            &SubgoalSignature("sig-shared".into()),
            &ApplicabilitySignature("app-telegram".into()),
            5,
        );

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].skill.id, "telegram");
    }

    #[test]
    fn remove_by_path_drops_deleted_skill_from_index() {
        let mut idx = SkillIndex::empty(Arc::new(HashedShingleEmbedder::default()));
        idx.upsert(skill_with("delete-me", 3, "sig", SkillState::Confirmed));

        assert!(idx.remove_by_path(Path::new("delete-me-v3.md")));

        assert!(idx.get("delete-me", 3).is_none());
        assert!(
            idx.lookup(
                &SubgoalSignature("sig".into()),
                &ApplicabilitySignature("appsig".into()),
                5,
            )
            .is_empty()
        );
    }

    #[test]
    fn skills_in_state_filters_correctly() {
        let mut idx = SkillIndex::empty(Arc::new(HashedShingleEmbedder::default()));
        idx.upsert(skill_with("d1", 1, "s", SkillState::Draft));
        idx.upsert(skill_with("c1", 1, "s", SkillState::Confirmed));
        idx.upsert(skill_with("p1", 1, "s", SkillState::Promoted));

        assert_eq!(idx.skills_in_state(SkillState::Draft).len(), 1);
        assert_eq!(idx.skills_in_state(SkillState::Confirmed).len(), 1);
        assert_eq!(idx.skills_in_state(SkillState::Promoted).len(), 1);
    }
}
