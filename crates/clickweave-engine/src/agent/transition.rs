// Page-transition detection. Reachable from `StateRunner::run` once the
// observe step starts comparing consecutive element lists; mute the
// dead_code warning on the helpers that are not yet wired in.
#![allow(dead_code)]

use std::collections::BTreeSet;

use blake3::Hasher;
use clickweave_core::cdp::{CdpElementInventory, CdpFindElementMatch};

/// Generate a stable, non-reversible fingerprint for a single element.
///
/// The hashed input includes the element's `uid`, role/tag, legacy label,
/// visible/accessibility evidence, and parent info so that elements with
/// stale accessibility names still change when rendered text changes. The
/// returned value intentionally omits the raw text because
/// `page_fingerprint` is rendered into prompts and durable traces.
pub fn element_fingerprint(el: &CdpFindElementMatch) -> String {
    let parent = match (&el.parent_role, &el.parent_name) {
        (Some(role), Some(name)) => format!("{}:{}", role, name),
        (Some(role), None) => role.clone(),
        _ => String::new(),
    };
    let mut hasher = Hasher::new();
    hasher.update(el.uid.as_bytes());
    hasher.update(b"\0");
    hasher.update(el.role.as_bytes());
    hasher.update(b"\0");
    hasher.update(el.label.as_bytes());
    hasher.update(b"\0");
    hasher.update(el.accessible_name.as_bytes());
    hasher.update(b"\0");
    hasher.update(el.visible_text.as_bytes());
    hasher.update(b"\0");
    hasher.update(el.value.as_bytes());
    hasher.update(b"\0");
    hasher.update(el.placeholder.as_bytes());
    hasher.update(b"\0");
    hasher.update(el.title.as_bytes());
    hasher.update(b"\0");
    hasher.update(el.alt_text.as_bytes());
    hasher.update(b"\0");
    hasher.update(el.test_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(el.tag.as_bytes());
    hasher.update(b"\0");
    hasher.update(parent.as_bytes());
    let hex = hasher.finalize().to_hex();
    hex[..16].to_string()
}

/// Detect whether a page transition occurred between two observations.
///
/// A transition is detected when the element sets differ significantly.
/// Returns `true` if more than 50% of elements changed between the two
/// observations, indicating a navigation or page load.
pub fn detect_transition(
    prev_elements: &[CdpFindElementMatch],
    curr_elements: &[CdpFindElementMatch],
) -> bool {
    if prev_elements.is_empty() && curr_elements.is_empty() {
        return false;
    }
    if prev_elements.is_empty() || curr_elements.is_empty() {
        return true;
    }

    let prev_fps: BTreeSet<String> = prev_elements.iter().map(element_fingerprint).collect();
    let curr_fps: BTreeSet<String> = curr_elements.iter().map(element_fingerprint).collect();

    let intersection_count = prev_fps.intersection(&curr_fps).count();
    let union_count = prev_fps.union(&curr_fps).count();

    if union_count == 0 {
        return false;
    }

    // Jaccard similarity < 0.5 means more than half of elements changed
    let similarity = intersection_count as f64 / union_count as f64;
    similarity < 0.5
}

/// Generate a combined fingerprint for an entire page of elements.
///
/// This is used in the world model and skill applicability signatures to
/// decide whether the observed page still matches a prior plan.
pub fn page_fingerprint(elements: &[CdpFindElementMatch]) -> String {
    let mut fps: Vec<String> = elements.iter().map(element_fingerprint).collect();
    fps.sort();
    let mut hasher = Hasher::new();
    for fp in &fps {
        hasher.update(fp.as_bytes());
        hasher.update(b"\0");
    }
    let hex = hasher.finalize().to_hex();
    format!("count={};hash={}", elements.len(), &hex[..16])
}

/// Generate a stable, non-reversible fingerprint from the compact CDP page
/// summary. This lets Clickweave track a CDP page without injecting a
/// page-wide element list into every model turn.
pub fn page_inventory_fingerprint(url: &str, inventory: &[CdpElementInventory]) -> String {
    let mut rows: Vec<&CdpElementInventory> = inventory.iter().collect();
    rows.sort_by(|a, b| a.role.cmp(&b.role).then(a.count.cmp(&b.count)));

    let mut hasher = Hasher::new();
    hasher.update(url.as_bytes());
    hasher.update(b"\0");
    for row in &rows {
        hasher.update(row.role.as_bytes());
        hasher.update(b"\0");
        hasher.update(row.count.to_string().as_bytes());
        hasher.update(b"\0");
        for label in &row.sample_labels {
            hasher.update(label.as_bytes());
            hasher.update(b"\0");
        }
        hasher.update(b"\0");
    }
    let total: usize = inventory.iter().map(|row| row.count).sum();
    let hex = hasher.finalize().to_hex();
    format!(
        "roles={};elements={};hash={}",
        inventory.len(),
        total,
        &hex[..16]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_element(uid: &str, role: &str, label: &str, tag: &str) -> CdpFindElementMatch {
        CdpFindElementMatch {
            uid: uid.to_string(),
            role: role.to_string(),
            label: label.to_string(),
            tag: tag.to_string(),
            disabled: false,
            parent_role: None,
            parent_name: None,
            ..Default::default()
        }
    }

    #[test]
    fn element_fingerprint_is_deterministic() {
        let el = make_element("1_0", "button", "Submit", "button");
        let fp1 = element_fingerprint(&el);
        let fp2 = element_fingerprint(&el);
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 16);
        assert!(!fp1.contains("button"));
        assert!(!fp1.contains("Submit"));
    }

    #[test]
    fn element_fingerprint_includes_parent() {
        let el = CdpFindElementMatch {
            uid: "1_0".to_string(),
            role: "button".to_string(),
            label: "Submit".to_string(),
            tag: "button".to_string(),
            disabled: false,
            parent_role: Some("form".to_string()),
            parent_name: Some("Login".to_string()),
            ..Default::default()
        };
        let fp = element_fingerprint(&el);
        assert_eq!(fp.len(), 16);
        assert!(!fp.contains("form:Login"));
        assert_ne!(
            fp,
            element_fingerprint(&make_element("1_0", "button", "Submit", "button"))
        );
    }

    #[test]
    fn element_fingerprint_includes_visible_text() {
        let mut before = make_element("d1", "button", "Chat with Alice", "button");
        before.visible_text = "Foo Bar Baz preview one".to_string();
        let mut after = before.clone();
        after.visible_text = "Foo Bar Baz preview two".to_string();

        assert_ne!(
            element_fingerprint(&before),
            element_fingerprint(&after),
            "visible text changes must be visible to page fingerprints even when label is stable"
        );
    }

    #[test]
    fn detect_transition_same_elements() {
        let elements = vec![
            make_element("1_0", "button", "Submit", "button"),
            make_element("1_1", "textbox", "Email", "input"),
        ];
        assert!(!detect_transition(&elements, &elements));
    }

    #[test]
    fn detect_transition_completely_different() {
        let prev = vec![
            make_element("1_0", "button", "Submit", "button"),
            make_element("1_1", "textbox", "Email", "input"),
        ];
        let curr = vec![
            make_element("2_0", "heading", "Dashboard", "h1"),
            make_element("2_1", "link", "Settings", "a"),
        ];
        assert!(detect_transition(&prev, &curr));
    }

    #[test]
    fn detect_transition_empty_to_elements() {
        let elements = vec![make_element("1_0", "button", "Submit", "button")];
        assert!(detect_transition(&[], &elements));
    }

    #[test]
    fn detect_transition_both_empty() {
        assert!(!detect_transition(&[], &[]));
    }

    #[test]
    fn detect_transition_partial_overlap() {
        // 2 out of 3 elements are the same — should NOT be a transition (67% overlap)
        let prev = vec![
            make_element("1_0", "button", "Submit", "button"),
            make_element("1_1", "textbox", "Email", "input"),
            make_element("1_2", "link", "Forgot Password", "a"),
        ];
        let curr = vec![
            make_element("1_0", "button", "Submit", "button"),
            make_element("1_1", "textbox", "Email", "input"),
            make_element("1_3", "textbox", "Password", "input"),
        ];
        // 2 shared out of 4 unique = 50% similarity → not a transition (threshold is < 0.5)
        assert!(!detect_transition(&prev, &curr));
    }

    #[test]
    fn page_fingerprint_is_order_independent() {
        let elements_a = vec![
            make_element("1_0", "button", "Submit", "button"),
            make_element("1_1", "textbox", "Email", "input"),
        ];
        let elements_b = vec![
            make_element("1_1", "textbox", "Email", "input"),
            make_element("1_0", "button", "Submit", "button"),
        ];
        assert_eq!(page_fingerprint(&elements_a), page_fingerprint(&elements_b));
        assert!(!page_fingerprint(&elements_a).contains("Submit"));
        assert!(!page_fingerprint(&elements_a).contains("Email"));
    }

    #[test]
    fn page_fingerprint_differs_for_different_pages() {
        let page_a = vec![make_element("1_0", "button", "Submit", "button")];
        let page_b = vec![make_element("2_0", "heading", "Dashboard", "h1")];
        assert_ne!(page_fingerprint(&page_a), page_fingerprint(&page_b));
    }

    #[test]
    fn page_inventory_fingerprint_is_order_independent() {
        let inventory_a = vec![
            CdpElementInventory {
                role: "button".to_string(),
                count: 2,
                sample_labels: vec!["Submit".to_string()],
            },
            CdpElementInventory {
                role: "textbox".to_string(),
                count: 1,
                sample_labels: vec!["Email".to_string()],
            },
        ];
        let mut inventory_b = inventory_a.clone();
        inventory_b.reverse();

        assert_eq!(
            page_inventory_fingerprint("https://example.com/", &inventory_a),
            page_inventory_fingerprint("https://example.com/", &inventory_b)
        );
        assert!(
            !page_inventory_fingerprint("https://example.com/", &inventory_a).contains("Submit")
        );
    }

    #[test]
    fn element_fingerprint_differs_by_uid() {
        // Two elements with the same role, label, and tag but different uids
        // should produce different fingerprints.
        let el_a = make_element("1_0", "button", "Submit", "button");
        let el_b = make_element("2_5", "button", "Submit", "button");
        assert_ne!(
            element_fingerprint(&el_a),
            element_fingerprint(&el_b),
            "Elements with different uids should have different fingerprints"
        );
    }
}
