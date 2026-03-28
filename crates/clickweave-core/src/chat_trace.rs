use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Append-only JSONL writer for chat conversation traces (debugging).
pub struct ChatTraceWriter {
    path: PathBuf,
}

impl ChatTraceWriter {
    pub fn new(base_dir: &Path, workflow_name: &str) -> Self {
        let sanitized = crate::sanitize::sanitize_for_path(workflow_name);
        let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        let dir = base_dir.join("chats").join(&sanitized);
        fs::create_dir_all(&dir).ok();
        let path = dir.join(format!("{}.jsonl", timestamp));
        Self { path }
    }

    pub fn append(&self, entry: &serde_json::Value) {
        if let Ok(line) = serde_json::to_string(entry)
            && let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
        {
            let _ = writeln!(file, "{}", line);
        }
    }
}
