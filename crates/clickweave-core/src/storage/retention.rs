use super::*;

/// Remove execution directories whose timestamp prefix is older than the
/// retention window. Only walks the two-level layout produced by
/// `RunStorage::new_app_data` — `runs/<workflow_dir>/<execution_dir>/` —
/// so sibling files (e.g. `decisions.json`) and any dir that doesn't look
/// like an execution dir are left alone.
///
/// * `runs_root` — the top-level `runs/` directory (e.g. under the app
///   data dir, or a saved project's `.clickweave/` dir).
/// * `retention_days` — maximum age in days. `0` disables cleanup and
///   returns immediately with an empty vec.
/// * `now` — current time, injected for deterministic testing.
///
/// Returns the list of execution directories that were successfully
/// removed. Individual failures are logged via `tracing::warn!` and do
/// not abort the sweep — per the privacy spec, cleanup is best-effort
/// and silent to the user.
pub fn cleanup_expired_runs(
    runs_root: &Path,
    retention_days: u64,
    now: DateTime<Utc>,
) -> Result<Vec<PathBuf>> {
    if retention_days == 0 {
        return Ok(Vec::new());
    }
    if !runs_root.exists() {
        return Ok(Vec::new());
    }

    // Clamp to a safe ceiling before the i64 cast so a hand-edited
    // `settings.json` with a huge `traceRetentionDays` cannot wrap into
    // a negative duration and push the cutoff into the future, which
    // would flag every existing run as expired and delete them all.
    // 10 years is well past any legitimate retention window — the UI
    // clamp is also 3650 days — so saturating here is indistinguishable
    // from "retain forever" in practice.
    const MAX_RETENTION_DAYS: u64 = 3650;
    let retention_days = retention_days.min(MAX_RETENTION_DAYS);

    let cutoff = now - chrono::Duration::days(retention_days as i64);
    let mut removed = Vec::new();

    let workflow_entries = std::fs::read_dir(runs_root)
        .with_context(|| format!("Failed to read runs root {}", runs_root.display()))?;

    for workflow_entry in workflow_entries {
        let workflow_entry = match workflow_entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "Skipping unreadable entry under runs root");
                continue;
            }
        };
        let Ok(file_type) = workflow_entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let workflow_dir = workflow_entry.path();

        let exec_entries = match std::fs::read_dir(&workflow_dir) {
            Ok(it) => it,
            Err(e) => {
                warn!(
                    path = %workflow_dir.display(),
                    error = %e,
                    "Skipping workflow dir whose contents could not be read",
                );
                continue;
            }
        };

        let mut removed_exec_names: Vec<String> = Vec::new();
        for exec_entry in exec_entries {
            let exec_entry = match exec_entry {
                Ok(e) => e,
                Err(e) => {
                    warn!(error = %e, "Skipping unreadable entry under workflow dir");
                    continue;
                }
            };
            let Ok(file_type) = exec_entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let exec_path = exec_entry.path();
            let Some(name) = exec_path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let name = name.to_string();
            let Some(ts) = parse_execution_dir_timestamp(&name) else {
                // Not an execution dir — leave it alone. Preserves
                // sibling files like `decisions.json` and future layout
                // additions we do not yet know about.
                continue;
            };
            if ts >= cutoff {
                continue;
            }
            match std::fs::remove_dir_all(&exec_path) {
                Ok(()) => {
                    removed_exec_names.push(name);
                    removed.push(exec_path);
                }
                Err(e) => warn!(
                    path = %exec_path.display(),
                    error = %e,
                    "Failed to remove expired execution dir",
                ),
            }
        }

        // Tombstone the workflow-level variant index for the exec dirs
        // we just removed. Scoping to `removed_exec_names` (instead of
        // filtering all entries against what exists on disk) keeps the
        // rewrite deterministic and safe even when the read-time
        // filter in `VariantIndex::load_existing` is also active:
        // fresh entries appended by a run starting during the sweep
        // reference current exec dirs, which cannot appear in
        // `removed_exec_names`, so the rewrite is a pure minus
        // operation on known-stale lines.
        //
        // This fills the gap for workflows the user may never reopen
        // — without this, expired `divergence_summary` text would
        // linger on disk indefinitely. The read-time filter in
        // `VariantIndex::load_existing` remains as the belt-and-braces
        // safety net for entries we did not see here (manual
        // cleanup, partial failures, etc.).
        if !removed_exec_names.is_empty() {
            let variant_path = workflow_dir.join("variant_index.jsonl");
            if let Err(e) = prune_variant_index_entries(&variant_path, &removed_exec_names) {
                warn!(
                    path = %variant_path.display(),
                    error = %e,
                    "Failed to tombstone variant index entries after cleanup sweep",
                );
            }
        }
    }

    Ok(removed)
}

/// Rewrite `variant_index.jsonl` with every line whose `execution_dir`
/// is **not** in `removed_names`. Preserves unparseable lines so a
/// schema mismatch cannot corrupt history. Uses a temp file + rename
/// so a crash mid-prune leaves either the old or the new content,
/// never a partial write. No-op when the file does not exist.
///
/// Only called from `cleanup_expired_runs` with exec dir names the
/// sweep just removed — the read-time filter in
/// `VariantIndex::load_existing` handles everything else.
fn prune_variant_index_entries(path: &Path, removed_names: &[String]) -> Result<()> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(
                anyhow::Error::from(e).context(format!("Failed to read {}", path.display()))
            );
        }
    };

    let removed_set: std::collections::HashSet<&str> =
        removed_names.iter().map(String::as_str).collect();

    let mut kept = String::with_capacity(content.len());
    let mut pruned_any = false;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let exec_dir_opt = serde_json::from_str::<Value>(line).ok().and_then(|v| {
            v.get("execution_dir")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        match exec_dir_opt {
            Some(name) if removed_set.contains(name.as_str()) => {
                pruned_any = true;
            }
            _ => {
                kept.push_str(line);
                kept.push('\n');
            }
        }
    }

    if !pruned_any {
        return Ok(());
    }

    if kept.is_empty() {
        std::fs::remove_file(path).with_context(|| {
            format!("Failed to remove emptied variant index {}", path.display())
        })?;
        return Ok(());
    }

    let tmp_path = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp_path, &kept)
        .with_context(|| format!("Failed to write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("Failed to rename over {}", path.display()))?;
    Ok(())
}
