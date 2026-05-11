//! B3: Privacy-off run golden test.
//!
//! With `RunStorage::set_persistent(false)` (the "Store run traces" kill switch),
//! running a skill via `create_skill_run` / `save_skill_run` leaves zero
//! artifacts in `<project>/.clickweave/skills/<id>/runs/`.

use clickweave_core::storage::RunStorage;
use std::fs;

#[test]
fn privacy_off_creates_no_run_artifacts_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let skill_id = "skl_privacy_off";

    // Create storage with persistence disabled (privacy kill switch).
    let mut storage = RunStorage::new(tmp.path(), "privacy-off-project");
    storage.set_persistent(false);

    assert!(!storage.is_persistent(), "persistence should be off");

    // create_skill_run returns a valid in-memory SkillRun but writes nothing.
    let run = storage.create_skill_run(skill_id).unwrap();
    assert_eq!(run.skill_id, skill_id, "run carries correct skill_id");

    // save_skill_run is also a no-op.
    storage.save_skill_run(&run).unwrap();

    // The runs directory must not exist — no artifacts created.
    let runs_dir = tmp
        .path()
        .join(".clickweave")
        .join("skills")
        .join(skill_id)
        .join("runs");

    assert!(
        !runs_dir.exists(),
        "runs directory must not be created when persistence is off; found: {}",
        runs_dir.display()
    );

    // More broadly, the entire skills directory should be absent.
    let skills_dir = tmp.path().join(".clickweave").join("skills");
    let no_artifacts = !skills_dir.exists()
        || fs::read_dir(&skills_dir)
            .map(|mut d| d.next().is_none())
            .unwrap_or(true);

    assert!(
        no_artifacts,
        "skills directory should contain no artifacts when privacy is off"
    );
}

/// Verify that persistence-on (the default) does create run artifacts.
/// This confirms the privacy-off test above is not vacuously passing
/// because run artifacts are never written.
#[test]
fn persistence_on_creates_run_artifacts_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let skill_id = "skl_privacy_on";

    let storage = RunStorage::new(tmp.path(), "privacy-on-project");
    assert!(
        storage.is_persistent(),
        "persistence should be on by default"
    );

    let run = storage.create_skill_run(skill_id).unwrap();
    storage.save_skill_run(&run).unwrap();

    let runs_dir = tmp
        .path()
        .join(".clickweave")
        .join("skills")
        .join(skill_id)
        .join("runs");

    assert!(
        runs_dir.exists(),
        "runs directory must exist when persistence is on"
    );
    let run_file = runs_dir.join(format!("{}.json", run.run_id));
    assert!(
        run_file.exists(),
        "run JSON file must exist when persistence is on"
    );
}
