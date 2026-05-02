//! Stable 16-char hex signatures over `WorldModel` slices, used to key
//! skills by subgoal context, applicability, and post-state. Mirrors
//! the same blake3-prefix pattern that the episodic memory layer uses
//! so the two layers project the same world state through compatible
//! fingerprints.

#![allow(dead_code)]

use blake3::Hasher;

use super::types::{ApplicabilitySignature, SubgoalSignature};
use crate::agent::world_model::WorldModel;

const SIGNATURE_LEN: usize = 16;

/// Subgoal-keyed signature: subgoal text (whitespace-trimmed +
/// lowercased) plus the focused app name and CDP page host (also
/// normalized). The signature is stable across runs that share the
/// same subgoal text and surface context.
pub fn compute_subgoal_signature(subgoal_text: &str, world_model: &WorldModel) -> SubgoalSignature {
    compute_subgoal_signature_from_parts(
        subgoal_text,
        focused_app_name(world_model),
        &cdp_host(world_model),
    )
}

pub fn compute_subgoal_signature_from_parts(
    subgoal_text: &str,
    focused_app_name: &str,
    cdp_host: &str,
) -> SubgoalSignature {
    let mut h = Hasher::new();
    h.update(subgoal_text.trim().to_lowercase().as_bytes());
    h.update(b"|");
    h.update(focused_app_name.trim().to_lowercase().as_bytes());
    h.update(b"|");
    h.update(cdp_host.trim().to_lowercase().as_bytes());
    SubgoalSignature(prefix_hex(&h, SIGNATURE_LEN))
}

/// Applicability signature: focused app + CDP page host only. Excludes
/// subgoal text so two unrelated subgoals running on the same surface
/// share an applicability signature, which is what retrieval scoring
/// uses for the cross-subgoal "applicable here" merge.
pub fn compute_applicability_signature(world_model: &WorldModel) -> ApplicabilitySignature {
    compute_applicability_signature_from_parts(
        focused_app_name(world_model),
        &cdp_host(world_model),
    )
}

pub fn compute_applicability_signature_from_parts(
    focused_app_name: &str,
    cdp_host: &str,
) -> ApplicabilitySignature {
    let mut h = Hasher::new();
    h.update(focused_app_name.trim().to_lowercase().as_bytes());
    h.update(b"|");
    h.update(cdp_host.trim().to_lowercase().as_bytes());
    ApplicabilitySignature(prefix_hex(&h, SIGNATURE_LEN))
}

/// Post-state signature used by the outcome predicate's
/// `post_state_world_model_signature` slot. Includes the CDP page
/// fingerprint (page-content-derived) so two pages that look the same
/// to the user but differ structurally produce different signatures.
pub fn compute_post_state_signature(world_model: &WorldModel) -> String {
    let mut h = Hasher::new();
    h.update(focused_app_name(world_model).as_bytes());
    h.update(b"|");
    h.update(cdp_host(world_model).as_bytes());
    h.update(b"|");
    h.update(cdp_page_fingerprint(world_model).as_bytes());
    prefix_hex(&h, SIGNATURE_LEN)
}

fn focused_app_name(wm: &WorldModel) -> &str {
    wm.focused_app
        .as_ref()
        .map(|f| f.value.name.as_str())
        .unwrap_or("")
}

fn cdp_host(wm: &WorldModel) -> String {
    wm.cdp_page
        .as_ref()
        .and_then(|p| url::Url::parse(p.value.url.as_str()).ok())
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

fn cdp_page_fingerprint(wm: &WorldModel) -> &str {
    wm.cdp_page
        .as_ref()
        .map(|p| p.value.page_fingerprint.as_str())
        .unwrap_or("")
}

fn prefix_hex(h: &Hasher, len: usize) -> String {
    let hex = h.finalize().to_hex();
    hex.as_str()[..len].to_string()
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::agent::world_model::{
        AppKind, CdpPageState, FocusedApp, Fresh, FreshnessSource, WorldModel,
    };

    fn fresh<T>(value: T) -> Fresh<T> {
        Fresh {
            value,
            written_at: 0,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        }
    }

    fn wm(app_name: Option<&str>, page_url: Option<&str>) -> WorldModel {
        let mut w = WorldModel::default();
        if let Some(name) = app_name {
            w.focused_app = Some(fresh(FocusedApp {
                name: name.to_string(),
                kind: AppKind::Native,
                pid: 0,
            }));
        }
        if let Some(url) = page_url {
            w.cdp_page = Some(fresh(CdpPageState {
                url: url.to_string(),
                page_fingerprint: String::new(),
                element_inventory: Vec::new(),
            }));
        }
        w
    }

    #[test]
    fn subgoal_signature_is_deterministic() {
        let s1 = compute_subgoal_signature("Open chat", &wm(Some("Telegram"), None));
        let s2 = compute_subgoal_signature("Open chat", &wm(Some("Telegram"), None));
        assert_eq!(s1, s2);
    }

    #[test]
    fn subgoal_signature_differs_on_app_change() {
        let a = compute_subgoal_signature("Open chat", &wm(Some("Telegram"), None));
        let b = compute_subgoal_signature("Open chat", &wm(Some("Signal"), None));
        assert_ne!(a, b);
    }

    #[test]
    fn applicability_signature_excludes_subgoal_text() {
        let a = compute_applicability_signature(&wm(Some("Telegram"), None));
        let b = compute_applicability_signature(&wm(Some("Telegram"), None));
        assert_eq!(a, b);
    }

    #[test]
    fn subgoal_signature_normalizes_whitespace_and_case() {
        let a = compute_subgoal_signature("  Open Chat  ", &wm(Some("Telegram"), None));
        let b = compute_subgoal_signature("open chat", &wm(Some("telegram"), None));
        assert_eq!(a, b);
    }

    #[test]
    fn cdp_host_uses_url_host_only_not_full_url() {
        let a = compute_subgoal_signature(
            "search",
            &wm(Some("Safari"), Some("https://example.com/foo")),
        );
        let b = compute_subgoal_signature(
            "search",
            &wm(Some("Safari"), Some("https://example.com/bar")),
        );
        assert_eq!(a, b);
    }
}
