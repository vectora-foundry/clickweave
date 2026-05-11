//! Section-history helpers for the `replay.json` sidecar.
//!
//! Tracks the retirement chain of section IDs so that historical run records
//! (stored as `runs/<run_id>.json`) can continue to resolve section references
//! even after a chat-driven section split changes the IDs.
//!
//! The primary type lives in `replay::SectionHistoryEntry`; this module
//! provides the pure resolution logic used at read time.

#![allow(dead_code)]

use super::replay::SectionHistoryEntry;

/// Resolve a possibly-retired `section_id` to the set of current section IDs
/// it maps to. If the id is not in the retirement chain it is returned as-is
/// (single-element vec). If it was retired and split into multiple successors,
/// all successors are returned (recursively resolved so multi-step splits
/// flatten correctly).
pub fn resolve_section_id<'a>(
    section_id: &'a str,
    history: &'a [SectionHistoryEntry],
) -> Vec<&'a str> {
    // Find the most recent retirement entry for this id.
    if let Some(entry) = history.iter().rfind(|e| e.retired == section_id) {
        // Recursively resolve each successor.
        entry
            .split_into
            .iter()
            .flat_map(|s| resolve_section_id(s, history))
            .collect()
    } else {
        vec![section_id]
    }
}

/// True when `candidate_id` is a current descendant of `ancestor_id`
/// through zero or more splits recorded in `history`.
pub fn is_descendant_of(
    candidate_id: &str,
    ancestor_id: &str,
    history: &[SectionHistoryEntry],
) -> bool {
    let resolved = resolve_section_id(ancestor_id, history);
    resolved.contains(&candidate_id)
}

/// Cap a section history list to at most `max_entries` entries, evicting
/// the oldest (FIFO). Mirrors the repair-history cap in `replay.rs`.
pub fn cap_section_history(history: &mut Vec<SectionHistoryEntry>, max_entries: usize) {
    while history.len() > max_entries {
        history.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::skills::replay::SectionHistoryEntry;

    fn entry(retired: &str, split_into: &[&str], at_version: u32) -> SectionHistoryEntry {
        SectionHistoryEntry {
            retired: retired.into(),
            split_into: split_into.iter().map(|s| s.to_string()).collect(),
            at_version,
            at: chrono::Utc::now(),
        }
    }

    #[test]
    fn resolve_returns_id_unchanged_when_not_in_history() {
        let history = vec![entry("sec_old", &["sec_a", "sec_b"], 2)];
        let resolved = resolve_section_id("sec_new", &history);
        assert_eq!(resolved, vec!["sec_new"]);
    }

    #[test]
    fn resolve_maps_retired_id_to_successors() {
        let history = vec![entry("sec_old", &["sec_a", "sec_b"], 2)];
        let resolved = resolve_section_id("sec_old", &history);
        assert_eq!(resolved, vec!["sec_a", "sec_b"]);
    }

    #[test]
    fn resolve_is_recursive_for_multi_step_splits() {
        let history = vec![
            entry("sec_root", &["sec_a", "sec_b"], 2),
            entry("sec_a", &["sec_a1", "sec_a2"], 3),
        ];
        let resolved = resolve_section_id("sec_root", &history);
        assert_eq!(resolved, vec!["sec_a1", "sec_a2", "sec_b"]);
    }

    #[test]
    fn is_descendant_of_direct_child() {
        let history = vec![entry("sec_old", &["sec_a", "sec_b"], 2)];
        assert!(is_descendant_of("sec_a", "sec_old", &history));
        assert!(!is_descendant_of("sec_c", "sec_old", &history));
    }

    #[test]
    fn cap_section_history_evicts_oldest() {
        let mut h = vec![
            entry("a", &["a1"], 1),
            entry("b", &["b1"], 2),
            entry("c", &["c1"], 3),
        ];
        cap_section_history(&mut h, 2);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].retired, "b");
    }
}
