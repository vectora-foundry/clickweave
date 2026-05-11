use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use serde::Serialize;
use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

use crate::TraceEvent;
#[cfg(test)]
use crate::TraceEventKind;

mod fs;
mod retention;
mod run_storage;

pub use fs::{
    append_jsonl, format_timestamped_dirname, now_millis, parse_execution_dir_timestamp,
    sanitize_name, write_json_atomic, write_json_pretty,
};
pub use retention::cleanup_expired_runs;
pub use run_storage::RunStorage;

#[cfg(test)]
use fs::tmp_path_for;

#[cfg(test)]
mod tests;
