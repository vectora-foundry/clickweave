use super::*;

/// Returns the current time as milliseconds since the Unix epoch.
pub fn now_millis() -> u64 {
    Utc::now().timestamp_millis() as u64
}

/// Formats a timestamped directory name as `YYYY-MM-DD_HH-MM-SS_<short_uuid>`.
pub fn format_timestamped_dirname(started_at_ms: u64, id: Uuid) -> String {
    let ts = i64::try_from(started_at_ms).ok();
    let dt = ts
        .and_then(DateTime::from_timestamp_millis)
        .unwrap_or_default();
    let short_id = &id.to_string()[..12];
    format!("{}_{short_id}", dt.format("%Y-%m-%d_%H-%M-%S"))
}

/// Serialize `value` as pretty-printed JSON to a temp file alongside `path`
/// and atomically rename it into place. A crash or power loss mid-write leaves
/// either the previous content or the new content on disk — never a truncated
/// mix. The temp file is removed on serialization failure so it does not
/// accumulate beside the destination.
pub fn write_json_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    let tmp_path = tmp_path_for(path);
    if let Err(e) = std::fs::write(&tmp_path, json.as_bytes()) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    match std::fs::rename(&tmp_path, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

/// Serialize `value` as pretty-printed JSON to `path`, crash-atomically.
///
/// Thin wrapper over [`write_json_atomic`] that converts `io::Error` into the
/// `anyhow::Error` flavor used by `RunStorage`'s public methods.
pub fn write_json_pretty<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    write_json_atomic(path, value).with_context(|| format!("Failed to write {}", path.display()))
}

pub(super) fn tmp_path_for(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

/// Appends a single JSON line to a file (newline-delimited JSON).
pub fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut line = serde_json::to_string(value).context("Failed to serialize JSONL entry")?;
    line.push('\n');

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .context("Failed to open JSONL file")?;
    file.write_all(line.as_bytes())
        .context("Failed to write JSONL entry")?;

    Ok(())
}

/// Sanitizes a name for use as a directory component.
///
/// Lowercases, replaces non-alphanumeric chars with `-`, collapses consecutive
/// dashes, and trims leading/trailing dashes.
pub fn sanitize_name(name: &str) -> String {
    crate::sanitize::sanitize_for_path(name)
}

/// Parses the `YYYY-MM-DD_HH-MM-SS` prefix of an execution directory name
/// back into a UTC datetime. Returns `None` when the prefix does not match
/// the expected format (e.g. unrelated directories under the runs root).
///
/// Pure function — no filesystem access — so it is covered by unit tests
/// without touching disk.
pub fn parse_execution_dir_timestamp(dir_name: &str) -> Option<DateTime<Utc>> {
    // The prefix is exactly `YYYY-MM-DD_HH-MM-SS` (19 chars) followed by an
    // underscore and the short uuid. Reject anything shorter.
    if dir_name.len() < 19 {
        return None;
    }
    let (prefix, rest) = dir_name.split_at(19);
    // Require the separator plus at least one short-uuid char so
    // partially-written names like `2026-04-16_10-00-00_` do not
    // pass as valid execution dirs.
    let suffix = rest.strip_prefix('_')?;
    if suffix.is_empty() {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(prefix, "%Y-%m-%d_%H-%M-%S").ok()?;
    Some(Utc.from_utc_datetime(&naive))
}
