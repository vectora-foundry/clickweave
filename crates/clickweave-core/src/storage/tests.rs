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
    let project_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let storage = RunStorage::new_app_data(&app_data_dir, "My Workflow", project_id);
    assert_eq!(
        storage.base_path,
        PathBuf::from("/tmp/com.clickweave.app/runs/my-workflow_550e8400")
    );
    assert_eq!(
        storage.project_skills_path,
        PathBuf::from("/tmp/com.clickweave.app/skills/550e8400-e29b-41d4-a716-446655440000")
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
    assert_eq!(
        storage.project_skills_path,
        PathBuf::from("/tmp/my-project/.clickweave/skills")
    );
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
