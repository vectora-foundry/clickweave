use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One-line summary of a run variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantEntry {
    pub execution_dir: String,
    pub diverged_at_step: Option<usize>,
    pub divergence_summary: String,
    pub success: bool,
}

/// Lightweight variant index — always loaded into agent context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VariantIndex {
    pub entries: Vec<VariantEntry>,
}

impl VariantIndex {
    /// Load from JSONL file.
    pub fn load(path: &Path) -> Self {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };
        let entries = content
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        Self { entries }
    }

    /// Append entry to JSONL file.
    pub fn append(path: &Path, entry: &VariantEntry) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::storage::append_jsonl(path, entry)
    }

    /// Format as compact text for agent context.
    pub fn as_context_text(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }
        let mut lines = vec!["## Past run variants".to_string()];
        for entry in &self.entries {
            let status = if entry.success { "ok" } else { "failed" };
            let diverged = entry
                .diverged_at_step
                .map(|s| format!(" (diverged at step {})", s))
                .unwrap_or_default();
            lines.push(format!(
                "- {}: {} [{}]{}",
                entry.execution_dir, entry.divergence_summary, status, diverged
            ));
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("variant_index.jsonl");
        let entry = VariantEntry {
            execution_dir: "2026-04-10_14-00-00_abc".to_string(),
            diverged_at_step: Some(3),
            divergence_summary: "Modal appeared".to_string(),
            success: true,
        };
        VariantIndex::append(&path, &entry).unwrap();
        let loaded = VariantIndex::load(&path);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].divergence_summary, "Modal appeared");
    }

    #[test]
    fn load_missing_file_returns_default() {
        let loaded = VariantIndex::load(std::path::Path::new("/nonexistent/path.jsonl"));
        assert!(loaded.entries.is_empty());
    }

    #[test]
    fn as_context_text_empty() {
        let index = VariantIndex::default();
        assert_eq!(index.as_context_text(), "");
    }

    #[test]
    fn as_context_text_formats_entries() {
        let index = VariantIndex {
            entries: vec![
                VariantEntry {
                    execution_dir: "2026-04-10_14-00-00_abc".to_string(),
                    diverged_at_step: Some(3),
                    divergence_summary: "Modal appeared".to_string(),
                    success: true,
                },
                VariantEntry {
                    execution_dir: "2026-04-10_15-00-00_def".to_string(),
                    diverged_at_step: None,
                    divergence_summary: "Followed reference trajectory".to_string(),
                    success: false,
                },
            ],
        };
        let text = index.as_context_text();
        assert!(text.starts_with("## Past run variants"));
        assert!(text.contains("[ok]"));
        assert!(text.contains("[failed]"));
        assert!(text.contains("(diverged at step 3)"));
    }

    #[test]
    fn append_multiple_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("variant_index.jsonl");

        for i in 0..3 {
            let entry = VariantEntry {
                execution_dir: format!("exec_{}", i),
                diverged_at_step: None,
                divergence_summary: format!("Run {}", i),
                success: i % 2 == 0,
            };
            VariantIndex::append(&path, &entry).unwrap();
        }

        let loaded = VariantIndex::load(&path);
        assert_eq!(loaded.entries.len(), 3);
        assert_eq!(loaded.entries[0].execution_dir, "exec_0");
        assert_eq!(loaded.entries[2].execution_dir, "exec_2");
    }
}
