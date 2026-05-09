//! Integration tests for `SkillStore`. Exercise the public surface the
//! runner (Phase 3) and the file watcher (Task 2.3.5) depend on:
//! atomic-rename writes, lossless round-trips through the markdown
//! frontmatter, and recently-written self-write tracking.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use chrono::Utc;
use clickweave_engine::agent::skills::{
    ApplicabilityHints, ApplicabilitySignature, OutcomePredicate, Skill, SkillError, SkillScope,
    SkillState, SkillStats, SkillStore, SubgoalSignature,
};

fn sample_skill(id: &str, version: u32) -> Skill {
    Skill {
        id: id.into(),
        version,
        state: SkillState::Draft,
        scope: SkillScope::ProjectLocal,
        name: format!("test skill {id}"),
        description: "round-trip fixture".into(),
        tags: vec!["fixture".into()],
        subgoal_text: "open chat".into(),
        subgoal_signature: SubgoalSignature("subgoal-sig".into()),
        applicability: ApplicabilityHints {
            apps: vec!["TestApp".into()],
            hosts: vec![],
            signature: ApplicabilitySignature("applicability-sig".into()),
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
        body: format!("# {id}\n\nbody for {id}\n"),
        schema_version: clickweave_engine::agent::skills::SKILL_SCHEMA_VERSION,
        variables: vec![],
        sections: vec![],
        replay: None,
    }
}

#[test]
fn write_then_list_then_read_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(tmp.path().to_path_buf());

    let original = sample_skill("alpha", 1);
    let written = store.write_skill(&original).unwrap();
    assert!(written.exists());

    let files = store.list_files().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0], written);

    let parsed = store.read_skill(&files[0]).unwrap();
    assert_eq!(parsed.id, original.id);
    assert_eq!(parsed.version, original.version);
    assert_eq!(parsed.body.trim(), original.body.trim());
}

#[test]
fn writing_two_versions_produces_two_files() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(tmp.path().to_path_buf());

    let v1 = sample_skill("beta", 1);
    let v2 = sample_skill("beta", 2);

    store.write_skill(&v1).unwrap();
    store.write_skill(&v2).unwrap();

    let mut files = store.list_files().unwrap();
    files.sort();
    assert_eq!(files.len(), 2);
    let names: Vec<String> = files
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"beta-v1.md".into()));
    assert!(names.contains(&"beta-v2.md".into()));
}

#[test]
fn write_uses_tmp_file_then_atomic_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(tmp.path().to_path_buf());

    let skill = sample_skill("gamma", 1);
    let final_path = store.write_skill(&skill).unwrap();

    // Post-condition: only the final file exists; no `.tmp` straggler.
    let entries: Vec<PathBuf> = fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], final_path);
    assert!(!final_path.to_string_lossy().ends_with(".tmp"));
}

#[test]
fn rename_in_place_replaces_old_file_with_new_name() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(tmp.path().to_path_buf());

    let original = sample_skill("delta-old", 1);
    let original_path = store.write_skill(&original).unwrap();

    let mut renamed = original.clone();
    renamed.id = "delta-new".into();
    let new_path = store
        .rename_skill_in_place(&original_path, &renamed)
        .unwrap();

    assert!(new_path.exists());
    assert!(!original_path.exists());

    let files = store.list_files().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0], new_path);
}

#[test]
fn malformed_file_errors_but_other_files_still_load() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(tmp.path().to_path_buf());

    let good = sample_skill("eps", 1);
    let good_path = store.write_skill(&good).unwrap();

    let bad_path = tmp.path().join("malformed-v1.md");
    fs::write(&bad_path, "no frontmatter here\n").unwrap();

    let files = store.list_files().unwrap();
    assert_eq!(files.len(), 2);

    let bad_err = store.read_skill(&bad_path).unwrap_err();
    assert!(matches!(
        bad_err,
        SkillError::MissingFrontmatterDelimiter(_)
    ));

    let good_again = store.read_skill(&good_path).unwrap();
    assert_eq!(good_again.id, "eps");
}

#[test]
fn was_recently_written_tracks_self_writes_within_tolerance() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(tmp.path().to_path_buf());

    let skill = sample_skill("zeta", 1);
    let path = store.write_skill(&skill).unwrap();

    assert!(store.was_recently_written(&path));

    // After the 100ms tolerance window the entry stops counting as a
    // self-write — the watcher consumer would treat a fresh event on
    // the same path as an external edit again.
    thread::sleep(Duration::from_millis(150));
    assert!(!store.was_recently_written(&path));
}

#[test]
fn delete_skill_removes_file_and_marks_recently_written() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(tmp.path().to_path_buf());

    let skill = sample_skill("eta", 1);
    let path = store.write_skill(&skill).unwrap();
    store.delete_skill(&path).unwrap();

    assert!(!path.exists());
    assert!(store.was_recently_written(&path));
}

#[test]
fn list_files_on_empty_dir_returns_empty_vec() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(tmp.path().to_path_buf());
    assert!(store.list_files().unwrap().is_empty());
}
