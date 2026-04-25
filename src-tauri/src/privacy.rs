//! Privacy settings helpers shared between app startup (run-trace
//! cleanup) and the per-run `store_traces` kill switch.
//!
//! The UI persists privacy settings through `tauri-plugin-store` into a
//! `settings.json` file under the app config dir. For the macOS /
//! Windows / Linux app-data-dir conventions this project uses, that
//! path coincides with `app_data_dir()` — the same root `runs/` lives
//! under — so the helpers here read the raw JSON directly instead of
//! pulling in the plugin runtime before the Tauri event loop is up.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Default retention window when the UI has not written a value yet or
/// the JSON file is missing. Matches the UI default in
/// `ui/src/store/settings.ts`.
pub const DEFAULT_TRACE_RETENTION_DAYS: u64 = 30;

/// Privacy fields the Rust side cares about at startup.
/// Mirrors the subset of `PersistedSettings` declared in the UI.
///
/// Field names are camelCase to match how the UI's Tauri plugin-store
/// serialises them in `settings.json`. Only fields the Rust side reads
/// at startup are modelled here — the `storeTraces` kill switch is
/// shipped per-run through `RunRequest` / `AgentRunRequest`, so it
/// lives with the IPC payloads rather than here.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PersistedPrivacy {
    #[serde(default)]
    pub trace_retention_days: Option<u64>,
}

/// Location of the plugin-store's `settings.json` on disk.
fn settings_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("settings.json")
}

/// Read the privacy-related subset of persisted settings from disk.
///
/// Missing file, unreadable file, or malformed JSON all resolve to the
/// default (empty) struct — the setting falls through to the caller's
/// compiled-in default. Failing closed to "no cleanup" is the safe
/// behaviour when settings can't be parsed.
pub fn load_privacy_settings(app_data_dir: &Path) -> PersistedPrivacy {
    let path = settings_path(app_data_dir);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return PersistedPrivacy::default();
    };
    serde_json::from_str(&raw).unwrap_or_else(|e| {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "Failed to parse settings.json for privacy lookup — using defaults",
        );
        PersistedPrivacy::default()
    })
}

/// Synchronous sweep helper. Exposed for tests and callers that need
/// a deterministic wait; production code should invoke the spawned
/// version so app startup is not blocked on filesystem I/O.
fn sweep_expired_runs_sync(app_data_dir: &Path) {
    let privacy = load_privacy_settings(app_data_dir);
    let retention_days = privacy
        .trace_retention_days
        .unwrap_or(DEFAULT_TRACE_RETENTION_DAYS);
    if retention_days == 0 {
        tracing::debug!("Trace retention disabled (0 days) — skipping cleanup sweep");
        return;
    }
    let runs_root = app_data_dir.join("runs");
    let now = chrono::Utc::now();
    match clickweave_core::storage::cleanup_expired_runs(&runs_root, retention_days, now) {
        Ok(removed) if removed.is_empty() => {
            tracing::debug!(
                runs_root = %runs_root.display(),
                retention_days,
                "Trace cleanup found no expired execution dirs",
            );
        }
        Ok(removed) => {
            tracing::info!(
                runs_root = %runs_root.display(),
                retention_days,
                removed_count = removed.len(),
                "Expired run traces cleaned up",
            );
        }
        Err(e) => {
            tracing::warn!(
                runs_root = %runs_root.display(),
                error = %e,
                "Trace cleanup sweep failed",
            );
        }
    }

    // Spec 2 D36: extend retention to episodic stores. Both helpers are
    // best-effort; failures log a warning and never block the trace
    // cleanup itself.
    sweep_episodic_workflow_local(&runs_root, retention_days);
    sweep_episodic_global(app_data_dir, retention_days);
}

/// Walk every workflow directory under `runs/` and apply the
/// retention sweep to its `episodic.sqlite` if one exists.
///
/// The sweep does two things per database:
/// 1. **Age cap (b)** — drop rows older than `retention_days`.
/// 2. **Orphan-ref sweep (a)** — drop rows whose every entry in
///    `step_record_refs_json` resolves to a file that no longer exists
///    (e.g., the `events.jsonl` that fed the row was just deleted by
///    the trace cleanup above).
fn sweep_episodic_workflow_local(runs_root: &Path, retention_days: u64) {
    let rd = match std::fs::read_dir(runs_root) {
        Ok(rd) => rd,
        Err(e) => {
            tracing::debug!(
                runs_root = %runs_root.display(),
                error = %e,
                "episodic: workflow-local sweep skipped — runs root unreadable",
            );
            return;
        }
    };
    for entry in rd.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let workflow_dir = entry.path();
        let db_path = workflow_dir.join("episodic.sqlite");
        if !db_path.exists() {
            continue;
        }
        if let Err(e) = sweep_workflow_local_db(&db_path, &workflow_dir, retention_days) {
            tracing::warn!(
                error = %e,
                path = %db_path.display(),
                "episodic: workflow-local retention sweep failed",
            );
        }
    }
}

/// Drop rows whose `created_at` is older than `retention_days`. Uses
/// `datetime(...)` on both sides so SQLite parses the RFC3339 timestamp
/// correctly regardless of sub-second precision.
///
/// Mirrors the clamp `clickweave_core::storage::cleanup_expired_runs`
/// applies to the same setting: a hand-edited `settings.json` with a
/// huge `traceRetentionDays` would otherwise wrap into a negative
/// `Duration::days` cast and move the cutoff into the future, deleting
/// every fresh row. 10 years is past any legitimate retention window
/// (the UI clamp is also 3650 days), so saturating here is
/// indistinguishable from "retain forever" in practice.
fn delete_rows_older_than(
    conn: &rusqlite::Connection,
    retention_days: u64,
) -> Result<usize, rusqlite::Error> {
    const MAX_RETENTION_DAYS: u64 = 3650;
    let retention_days = retention_days.min(MAX_RETENTION_DAYS);
    let cutoff = chrono::Utc::now() - chrono::Duration::days(retention_days as i64);
    conn.execute(
        "DELETE FROM episodes WHERE datetime(created_at) < datetime(?1)",
        rusqlite::params![cutoff.to_rfc3339()],
    )
}

fn sweep_workflow_local_db(
    db: &Path,
    workflow_dir: &Path,
    retention_days: u64,
) -> Result<(), String> {
    use rusqlite::{Connection, params};
    let conn = Connection::open(db).map_err(|e| e.to_string())?;

    if let Err(e) = delete_rows_older_than(&conn, retention_days) {
        tracing::warn!(
            error = %e,
            path = %db.display(),
            "episodic: workflow-local age-cap delete failed",
        );
    }

    // (a) orphan-ref sweep. Rows with an empty refs list are skipped
    // (no way to tell whether their events.jsonl is gone or never
    // existed — defaulting to "keep" matches the failure-isolation
    // posture of Spec 2 D32).
    let mut stmt = conn
        .prepare("SELECT episode_id, step_record_refs_json FROM episodes")
        .map_err(|e| e.to_string())?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    for (ep_id, refs_json) in rows {
        let refs: Vec<String> = serde_json::from_str(&refs_json).unwrap_or_default();
        if refs.is_empty() {
            continue;
        }
        // Refs are stored as absolute paths (the runner resolves
        // `events.jsonl` through `RunStorage::base_path()`) but we
        // also tolerate workflow-relative paths for forward
        // compatibility.
        let any_alive = refs.iter().any(|r| {
            let p = std::path::Path::new(r);
            if p.is_absolute() {
                p.exists()
            } else {
                workflow_dir.join(r).exists()
            }
        });
        if !any_alive {
            let _ = conn.execute("DELETE FROM episodes WHERE episode_id = ?1", params![ep_id]);
        }
    }
    Ok(())
}

/// Apply the absolute age cap to the global episodic SQLite store. The
/// global store has no events.jsonl backing files, so the orphan-ref
/// sweep does not apply.
fn sweep_episodic_global(app_data_dir: &Path, retention_days: u64) {
    let db = app_data_dir.join("episodic.sqlite");
    if !db.exists() {
        return;
    }
    let conn = match rusqlite::Connection::open(&db) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %db.display(),
                "episodic: failed to open global store for retention sweep",
            );
            return;
        }
    };
    if let Err(e) = delete_rows_older_than(&conn, retention_days) {
        tracing::warn!(
            error = %e,
            path = %db.display(),
            "episodic: global age-cap delete failed",
        );
    }
}

/// Kick off the expired-trace sweep on a detached OS thread so app
/// startup is not blocked while the directory walk runs. Silent
/// best-effort — any I/O error is logged through tracing and swallowed
/// inside the worker. Thread spawn failure itself is also non-fatal;
/// the sweep simply doesn't run this session.
pub fn spawn_expired_app_data_runs_sweep(app_data_dir: PathBuf) {
    let spawn_result = std::thread::Builder::new()
        .name("clickweave-trace-cleanup".into())
        .spawn(move || sweep_expired_runs_sync(&app_data_dir));
    if let Err(e) = spawn_result {
        tracing::warn!(error = %e, "Failed to spawn trace cleanup thread; skipping sweep");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        std::env::temp_dir()
            .join("clickweave_privacy_test")
            .join(uuid::Uuid::new_v4().to_string())
    }

    #[test]
    fn load_privacy_settings_missing_file_returns_default() {
        let dir = tmp();
        std::fs::create_dir_all(&dir).unwrap();
        let p = load_privacy_settings(&dir);
        assert!(p.trace_retention_days.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_privacy_settings_malformed_json_returns_default() {
        let dir = tmp();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), b"not json").unwrap();
        let p = load_privacy_settings(&dir);
        assert!(p.trace_retention_days.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_privacy_settings_reads_retention_camel_case_field() {
        let dir = tmp();
        std::fs::create_dir_all(&dir).unwrap();
        let payload = serde_json::json!({
            "traceRetentionDays": 7,
            "somethingElse": "ignored",
        });
        std::fs::write(dir.join("settings.json"), payload.to_string()).unwrap();
        let p = load_privacy_settings(&dir);
        assert_eq!(p.trace_retention_days, Some(7));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sweep_expired_runs_sync_retention_zero_leaves_everything_in_place() {
        // End-to-end check of the privacy plumbing: settings.json with
        // `traceRetentionDays: 0` should skip the cleanup even when
        // there are ancient run dirs on disk.
        let dir = tmp();
        std::fs::create_dir_all(dir.join("runs/workflow-a/2020-01-01_00-00-00_aaaaaaaaaaaa"))
            .unwrap();
        let payload = serde_json::json!({ "traceRetentionDays": 0 });
        std::fs::write(dir.join("settings.json"), payload.to_string()).unwrap();

        sweep_expired_runs_sync(&dir);

        assert!(
            dir.join("runs/workflow-a/2020-01-01_00-00-00_aaaaaaaaaaaa")
                .exists(),
            "retention=0 must leave all traces alone",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod episodic_retention_tests {
    //! Spec 2 D36: the trace-retention sweep extends to the workflow-local
    //! and global episodic SQLite stores. Tests verify that real rows are
    //! deleted (not just that the helpers return Ok), so a regression
    //! that silently dropped the DELETE would surface immediately.

    use super::*;
    use rusqlite::{Connection, params};

    fn tmp_dir() -> PathBuf {
        let p = std::env::temp_dir()
            .join("clickweave_privacy_episodic_test")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Schema bootstrap mirroring `SqliteEpisodicStore::new`. Tests
    /// populate rows via direct SQL to avoid pulling the full engine
    /// crate into the Tauri test binary.
    fn create_episodes_schema(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS episodes (
                episode_id TEXT PRIMARY KEY,
                scope TEXT NOT NULL,
                workflow_hash TEXT NOT NULL,
                pre_state_signature TEXT NOT NULL,
                goal TEXT NOT NULL,
                subgoal_text TEXT,
                failure_signature_json TEXT NOT NULL,
                recovery_actions_json TEXT NOT NULL,
                recovery_actions_hash TEXT NOT NULL,
                outcome_summary TEXT NOT NULL,
                pre_state_snapshot_json TEXT NOT NULL,
                embedding_blob BLOB NOT NULL,
                embedding_impl_id TEXT NOT NULL,
                occurrence_count INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                last_seen_at TEXT NOT NULL,
                last_retrieved_at TEXT,
                step_record_refs_json TEXT NOT NULL
            );",
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_row(
        conn: &Connection,
        episode_id: &str,
        scope: &str,
        workflow_hash: &str,
        pre_state_signature: &str,
        recovery_actions_hash: &str,
        created_at: &str,
        step_record_refs_json: &str,
    ) {
        conn.execute(
            "INSERT INTO episodes (
                episode_id, scope, workflow_hash, pre_state_signature, goal,
                subgoal_text, failure_signature_json, recovery_actions_json,
                recovery_actions_hash, outcome_summary, pre_state_snapshot_json,
                embedding_blob, embedding_impl_id, occurrence_count,
                created_at, last_seen_at, last_retrieved_at, step_record_refs_json
            ) VALUES (?1, ?2, ?3, ?4, 'goal', NULL, '{}', '[]', ?5, '', '{}',
                      X'00', 'test', 1, ?6, ?6, NULL, ?7)",
            params![
                episode_id,
                scope,
                workflow_hash,
                pre_state_signature,
                recovery_actions_hash,
                created_at,
                step_record_refs_json,
            ],
        )
        .unwrap();
    }

    fn count_rows(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn orphan_refs_are_swept_from_workflow_local() {
        // Arrange: create a workflow dir with a fresh events.jsonl file
        // and an episodic.sqlite with two rows — one referencing the
        // existing file, one referencing a long-deleted file.
        let dir = tmp_dir();
        let runs_root = dir.join("runs");
        let workflow_dir = runs_root.join("workflow-a");
        std::fs::create_dir_all(&workflow_dir).unwrap();
        let events_path = workflow_dir.join("exec_2026-04-25/events.jsonl");
        std::fs::create_dir_all(events_path.parent().unwrap()).unwrap();
        std::fs::write(&events_path, b"{}\n").unwrap();
        let missing_path = workflow_dir.join("exec_1999-01-01/events.jsonl");

        let db_path = workflow_dir.join("episodic.sqlite");
        let now = chrono::Utc::now().to_rfc3339();
        {
            let conn = Connection::open(&db_path).unwrap();
            create_episodes_schema(&conn);
            insert_row(
                &conn,
                "ep_alive",
                "workflow_local",
                "wf-a",
                "sig-1",
                "rah-1",
                &now,
                &serde_json::json!([events_path.to_string_lossy()]).to_string(),
            );
            insert_row(
                &conn,
                "ep_orphan",
                "workflow_local",
                "wf-a",
                "sig-2",
                "rah-2",
                &now,
                &serde_json::json!([missing_path.to_string_lossy()]).to_string(),
            );
            assert_eq!(count_rows(&conn), 2, "fixture should seed two rows");
        }

        // Act: sweep with a generous retention window so age-cap doesn't
        // touch the rows. Only the orphan-ref check should fire.
        sweep_episodic_workflow_local(&runs_root, 365);

        // Assert: orphan deleted, alive row preserved.
        let conn = Connection::open(&db_path).unwrap();
        let alive_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM episodes WHERE episode_id = 'ep_alive'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let orphan_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM episodes WHERE episode_id = 'ep_orphan'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(alive_count, 1, "row with live events.jsonl ref must remain");
        assert_eq!(orphan_count, 0, "row with orphaned ref must be deleted");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn global_store_ages_rows_out_at_retention_cap() {
        // Arrange: create a global episodic.sqlite with one ancient row
        // (created two years ago) and one fresh row (just now).
        let dir = tmp_dir();
        let db_path = dir.join("episodic.sqlite");
        let ancient = (chrono::Utc::now() - chrono::Duration::days(730)).to_rfc3339();
        let fresh = chrono::Utc::now().to_rfc3339();
        {
            let conn = Connection::open(&db_path).unwrap();
            create_episodes_schema(&conn);
            insert_row(
                &conn,
                "ep_ancient",
                "global",
                "wf-x",
                "sig-old",
                "rah-old",
                &ancient,
                "[]",
            );
            insert_row(
                &conn, "ep_fresh", "global", "wf-y", "sig-new", "rah-new", &fresh, "[]",
            );
            assert_eq!(count_rows(&conn), 2, "fixture should seed two global rows");
        }

        // Act: sweep with a 30-day retention window — the ancient row
        // is 730 days old and must be removed.
        sweep_episodic_global(&dir, 30);

        // Assert: ancient row gone, fresh row preserved.
        let conn = Connection::open(&db_path).unwrap();
        let ancient_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM episodes WHERE episode_id = 'ep_ancient'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let fresh_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM episodes WHERE episode_id = 'ep_fresh'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            ancient_count, 0,
            "row older than retention window must be deleted",
        );
        assert_eq!(fresh_count, 1, "fresh row must be preserved");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn global_store_sweep_no_op_when_no_db_exists() {
        // Arrange: empty app data dir, no episodic.sqlite.
        let dir = tmp_dir();
        // Act + Assert: sweep returns silently — no panic, no error.
        sweep_episodic_global(&dir, 30);
        assert!(
            !dir.join("episodic.sqlite").exists(),
            "sweep must not create an empty database",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
