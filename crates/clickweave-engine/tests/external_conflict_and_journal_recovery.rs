//! B7: ExternalConflict test.
//! B8: Journal crash-recovery case (c).
//!
//! These tests are grouped in one file because both exercise the
//! `SkillStore`'s atomic-write protocol and its conflict-detection guard.

use std::fs;
use std::time::Duration;

use chrono::Utc;
use clickweave_engine::agent::skills::{
    ApplicabilityHints, ApplicabilitySignature, OutcomePredicate, SKILL_SCHEMA_VERSION, Skill,
    SkillError, SkillScope, SkillState, SkillStats, SkillStore, SubgoalSignature,
};

fn sample_skill(id: &str) -> Skill {
    Skill {
        id: id.to_string(),
        version: 1,
        state: SkillState::Draft,
        scope: SkillScope::ProjectLocal,
        name: "Sample".to_string(),
        description: "fixture".to_string(),
        tags: vec![],
        subgoal_text: "do something".to_string(),
        subgoal_signature: SubgoalSignature("sig".to_string()),
        applicability: ApplicabilityHints {
            apps: vec![],
            hosts: vec![],
            signature: ApplicabilitySignature("appsig".to_string()),
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
        body: "# Sample\n\nbody text\n".to_string(),
        schema_version: SKILL_SCHEMA_VERSION,
        variables: vec![],
        sections: vec![],
        replay: None,
    }
}

// ── B7: ExternalConflict ────────────────────────────────────────────────────

/// Two concurrent writers: in-flight patch holds the mtime at write time;
/// external editor saves SKILL.md in between.
///
/// Sequence:
/// 1. First writer reads the skill and records its mtime.
/// 2. External editor (simulated with a sleep + `fs::write`) updates SKILL.md,
///    advancing its mtime.
/// 3. First writer tries to apply its patch via `write_skill_atomic`
///    with the stale expected mtime.
/// 4. Must return `SkillError::ExternalConflict` and leave the on-disk
///    file at the externally-edited content.
#[test]
fn external_editor_save_while_patch_in_flight_returns_external_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(tmp.path().to_path_buf());

    // 1. Write the initial skill.
    let skill = sample_skill("skl_conflict");
    store.write_skill(&skill).unwrap();

    let final_path = store.skill_md_path(&skill.id);

    // 2. Record the mtime that a chat patch would capture at read time.
    let pre_edit_mtime = fs::metadata(&final_path).unwrap().modified().unwrap();

    // Sleep to advance filesystem time past the 2 ms tolerance window
    // inside `mtime_matches`, then write externally-edited content.
    std::thread::sleep(Duration::from_millis(20));
    let external_edit_content = b"---\nname: External Edit\ndescription: edited externally\nid: skl_conflict\nversion: 2\nschema_version: 1\n---\n\n# External\n\n```json action_sketch\n[]\n```\n";
    fs::write(&final_path, external_edit_content).unwrap();

    // Verify the mtime actually changed so the conflict guard will fire.
    let post_edit_mtime = fs::metadata(&final_path).unwrap().modified().unwrap();
    // If the filesystem rounds mtime to 1-second granularity (e.g. HFS+
    // compatibility mode), the sleep may not have advanced the clock far
    // enough. Skip rather than fail — on HFS+ sub-second mtime is not
    // guaranteed until macOS Ventura+.
    if pre_edit_mtime == post_edit_mtime {
        eprintln!("SKIP: filesystem mtime granularity too coarse to test ExternalConflict");
        return;
    }

    // 3. First writer attempts to save with the stale expected mtime.
    let result = store.write_skill_atomic(&skill, Some(pre_edit_mtime));

    // 4. Must return ExternalConflict — the on-disk file is unchanged.
    assert!(
        matches!(result, Err(SkillError::ExternalConflict)),
        "expected ExternalConflict, got {result:?}"
    );

    // The on-disk content must still be the external edit, not the
    // first writer's version.
    let on_disk = fs::read(&final_path).unwrap();
    assert_eq!(
        on_disk, external_edit_content,
        "on-disk file must reflect the external edit, not the patch writer's version"
    );
}

/// A `write_skill_atomic` with `expected_mtime = None` on a pre-existing
/// file also returns `ExternalConflict` (the "no preexisting file" promise
/// is violated).
#[test]
fn write_skill_atomic_with_none_mtime_conflicts_when_file_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(tmp.path().to_path_buf());

    let skill = sample_skill("skl_conflict_none");
    store.write_skill(&skill).unwrap();

    // Passing `None` as expected_mtime means "I expect no file to exist."
    // Since one does, this must conflict.
    let result = store.write_skill_atomic(&skill, None);
    assert!(
        matches!(result, Err(SkillError::ExternalConflict)),
        "expected ExternalConflict when file exists and mtime is None"
    );
}

// ── B8: Journal crash-recovery case (c) ────────────────────────────────────

/// Simulate a crash BETWEEN the SKILL.md rename and the replay.json rename.
///
/// State after the simulated crash:
/// - `SKILL.md` is in the live position (the `*.new` staging file is gone —
///   it was already renamed).
/// - `replay.json.new` is still in `pending/` (the rename did not happen).
/// - The `.tx/commit` marker exists (the transaction was committed before
///   the rename sequence started).
/// - The `manifest.json` lists both files.
///
/// Expected recovery:
/// - `recover_atomic_writes` applies the missing rename (`replay.json`).
/// - Both `SKILL.md` and `replay.json` reflect the post-patch content.
/// - The `.tx/` journal is cleaned up.
/// - Running recovery a second time is a no-op (idempotent).
#[test]
fn crash_between_skill_md_rename_and_replay_json_rename_replays_missing_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let store = SkillStore::new(tmp.path().to_path_buf());
    let skill_id = "skl_crash_c".to_string();
    let skill_dir = tmp.path().join(&skill_id);
    let tx_dir = skill_dir.join(".tx");
    let pending = tx_dir.join("pending");

    // ── Arrange ──────────────────────────────────────────────────────────────

    // Pre-crash live state: SKILL.md is already at the new content
    // (the rename succeeded for SKILL.md), replay.json is absent because
    // the crash happened before that rename.
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), b"post-patch SKILL.md content").unwrap();
    // replay.json does NOT exist yet — it was never renamed.

    // The .tx/pending directory still has replay.json.new (not renamed).
    fs::create_dir_all(&pending).unwrap();
    fs::write(
        pending.join("replay.json.new"),
        b"{\"skill_id\":\"skl_crash_c\",\"schema_version\":1,\"steps\":{},\"section_history\":[]}",
    )
    .unwrap();
    // SKILL.md.new is absent — it was already renamed (that rename succeeded).

    // The manifest lists both files.
    let manifest_json = serde_json::json!({
        "files": [
            {"relative": "SKILL.md"},
            {"relative": "replay.json"}
        ]
    });
    fs::write(
        tx_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest_json).unwrap(),
    )
    .unwrap();

    // The commit marker exists — the transaction was committed.
    fs::write(tx_dir.join("commit"), b"").unwrap();

    // ── Act: first recovery pass ──────────────────────────────────────────────

    store.recover_atomic_writes(&skill_id).unwrap();

    // ── Assert: both files are in their live positions ────────────────────────

    let skill_md = skill_dir.join("SKILL.md");
    assert!(skill_md.exists(), "SKILL.md must remain in live position");
    assert_eq!(
        fs::read(&skill_md).unwrap(),
        b"post-patch SKILL.md content",
        "SKILL.md content must be the post-patch version"
    );

    let replay_json = skill_dir.join("replay.json");
    assert!(
        replay_json.exists(),
        "replay.json must be renamed into live position by recovery"
    );
    let replay_bytes = fs::read(&replay_json).unwrap();
    let replay_val: serde_json::Value = serde_json::from_slice(&replay_bytes).unwrap();
    assert_eq!(
        replay_val["skill_id"].as_str().unwrap(),
        "skl_crash_c",
        "replay.json content must be the staged post-patch data"
    );

    // The journal must be cleaned up.
    assert!(
        !tx_dir.exists(),
        ".tx/ journal must be removed after recovery"
    );

    // ── Act: second recovery pass (idempotency) ───────────────────────────────

    // Journal is already gone; the second call should be a no-op.
    store.recover_atomic_writes(&skill_id).unwrap();

    // The live files must be unchanged.
    assert!(
        skill_md.exists(),
        "SKILL.md still present after second recovery"
    );
    assert!(
        replay_json.exists(),
        "replay.json still present after second recovery"
    );
    assert_eq!(
        fs::read(&skill_md).unwrap(),
        b"post-patch SKILL.md content",
        "SKILL.md content unchanged after second recovery"
    );
}
