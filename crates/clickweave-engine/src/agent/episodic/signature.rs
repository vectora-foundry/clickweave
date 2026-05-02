//! Compute a stable 16-char hex fingerprint of a WorldModel's structural
//! shape (D22, D37). Feeds the primary SQL retrieval index.

#![allow(dead_code)]

use blake3::Hasher;

use crate::agent::episodic::types::PreStateSignature;
use crate::agent::task_state::WatchSlotName;
use crate::agent::world_model::{ObservedElement, WorldModel};

/// Compute the signature that goes into SQL and into `EpisodeRecord::pre_state_signature`.
///
/// Inputs (in this exact order, to keep the hash stable across refactors):
/// 1. `focused_app.name` (or "" if None)
/// 2. `cdp_page` host parsed out of the URL (or "" if None / unparseable)
/// 3. `modal_present` as "1"/"0" (or "?" if None)
/// 4. `dialog_present` as "1"/"0" (or "?" if None)
/// 5. Top-5 element roles by count (sorted descending by count, then
///    ascending by role name for tie-break), joined with ":"
/// 6. Active watch-slot names sorted ascending, joined with ","
pub fn compute_pre_state_signature(
    world_model: &WorldModel,
    active_watch_slots: &[WatchSlotName],
) -> PreStateSignature {
    let mut h = Hasher::new();

    // FocusedApp fields are { name, kind, pid } (see
    // `crates/clickweave-engine/src/agent/world_model.rs`). Only `name`
    // contributes here; `kind` is host-derived and `pid` is run-local.
    let focused_app = world_model
        .focused_app
        .as_ref()
        .map(|f| f.value.name.as_str())
        .unwrap_or("");
    h.update(focused_app.as_bytes());
    h.update(b"\x1f");

    // CdpPageState.url is `String` (no Option wrapper). Parse out the
    // host so two URLs that point at the same origin produce the same
    // signature.
    let host = world_model
        .cdp_page
        .as_ref()
        .and_then(|p| url::Url::parse(p.value.url.as_str()).ok())
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_default();
    h.update(host.as_bytes());
    h.update(b"\x1f");

    for opt in [&world_model.modal_present, &world_model.dialog_present] {
        let s = match opt {
            Some(f) if f.value => "1",
            Some(_) => "0",
            None => "?",
        };
        h.update(s.as_bytes());
        h.update(b"\x1f");
    }

    let elements_top5 = element_role_histogram_top5(world_model);
    for (role, count) in &elements_top5 {
        h.update(role.as_bytes());
        h.update(b"=");
        h.update(count.to_string().as_bytes());
        h.update(b":");
    }
    h.update(b"\x1f");

    let mut slots: Vec<&str> = active_watch_slots.iter().map(watch_slot_name).collect();
    slots.sort_unstable();
    for s in slots {
        h.update(s.as_bytes());
        h.update(b",");
    }

    let hex = h.finalize().to_hex();
    PreStateSignature(hex.as_str()[..16].to_string())
}

fn watch_slot_name(n: &WatchSlotName) -> &'static str {
    match n {
        WatchSlotName::PendingModal => "pending_modal",
        WatchSlotName::PendingAuth => "pending_auth",
        WatchSlotName::PendingFocusShift => "pending_focus_shift",
    }
}

fn element_role_histogram_top5(wm: &WorldModel) -> Vec<(String, u32)> {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    if let Some(f) = &wm.elements {
        for el in &f.value {
            // Both CdpFindElementMatch.role and AxElement.role are `String`
            // (see crates/clickweave-core/src/cdp.rs and
            // crates/clickweave-engine/src/agent/world_model.rs).
            let role = match el {
                ObservedElement::Cdp(c) => c.role.clone(),
                ObservedElement::Ax(a) => a.role.clone(),
                ObservedElement::Ocr(_) => "ocr_text".to_string(),
            };
            *counts.entry(role).or_default() += 1;
        }
    }
    let mut v: Vec<(String, u32)> = counts.into_iter().collect();
    // sort descending by count, ascending by role name for tie-break
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.truncate(5);
    v
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // Tests build the WorldModel field-by-field for readability, matching the pattern in `world_model.rs`'s own tests.
mod tests {
    use super::*;
    use crate::agent::world_model::{
        AppKind, AxElement, CdpPageState, FocusedApp, Fresh, FreshnessSource, ObservedElement,
    };
    use clickweave_core::cdp::CdpFindElementMatch;

    fn stale_now<T>(value: T) -> Fresh<T> {
        Fresh {
            value,
            written_at: 0,
            source: FreshnessSource::DirectObservation,
            ttl_steps: None,
        }
    }

    fn base_wm() -> WorldModel {
        // FocusedApp fields are { name, kind, pid }. AxElement does not
        // implement Default, so each field is listed explicitly.
        let mut wm = WorldModel::default();
        wm.focused_app = Some(stale_now(FocusedApp {
            name: "Safari".to_string(),
            kind: AppKind::Native,
            pid: 1234,
        }));
        wm.cdp_page = Some(stale_now(CdpPageState {
            url: "https://accounts.google.com/signin".to_string(),
            page_fingerprint: "fp_signin".to_string(),
            element_inventory: Vec::new(),
        }));
        wm.modal_present = Some(stale_now(true));
        wm.dialog_present = Some(stale_now(false));
        let cdp_match = CdpFindElementMatch {
            uid: "d-42".to_string(),
            role: "button".to_string(),
            label: "Continue".to_string(),
            tag: "button".to_string(),
            disabled: false,
            parent_role: None,
            parent_name: None,
            ..Default::default()
        };
        wm.elements = Some(stale_now(vec![
            ObservedElement::Cdp(cdp_match),
            ObservedElement::Ax(AxElement {
                uid: "a42g3".to_string(),
                role: "button".to_string(),
                name: Some("Continue".to_string()),
                value: None,
                depth: 0,
                focused: false,
                disabled: false,
                parent_name: None,
            }),
        ]));
        wm
    }

    #[test]
    fn identical_world_models_produce_identical_signatures() {
        let a = compute_pre_state_signature(&base_wm(), &[]);
        let b = compute_pre_state_signature(&base_wm(), &[]);
        assert_eq!(a, b);
    }

    #[test]
    fn differing_app_differs_signature() {
        let mut wm2 = base_wm();
        wm2.focused_app = Some(stale_now(FocusedApp {
            name: "Chrome".to_string(),
            kind: AppKind::ChromeBrowser,
            pid: 999,
        }));
        let a = compute_pre_state_signature(&base_wm(), &[]);
        let b = compute_pre_state_signature(&wm2, &[]);
        assert_ne!(a, b);
    }

    #[test]
    fn differing_host_differs_signature() {
        let mut wm2 = base_wm();
        wm2.cdp_page = Some(stale_now(CdpPageState {
            url: "https://login.microsoft.com".to_string(),
            page_fingerprint: "fp_msft".to_string(),
            element_inventory: Vec::new(),
        }));
        let a = compute_pre_state_signature(&base_wm(), &[]);
        let b = compute_pre_state_signature(&wm2, &[]);
        assert_ne!(a, b);
    }

    #[test]
    fn differing_modal_present_differs_signature() {
        let mut wm2 = base_wm();
        wm2.modal_present = Some(stale_now(false));
        let a = compute_pre_state_signature(&base_wm(), &[]);
        let b = compute_pre_state_signature(&wm2, &[]);
        assert_ne!(a, b);
    }

    #[test]
    fn differing_watch_slots_differs_signature() {
        let a = compute_pre_state_signature(&base_wm(), &[]);
        let b = compute_pre_state_signature(&base_wm(), &[WatchSlotName::PendingModal]);
        assert_ne!(a, b);
    }

    #[test]
    fn watch_slot_order_does_not_affect_signature() {
        let a = compute_pre_state_signature(
            &base_wm(),
            &[WatchSlotName::PendingModal, WatchSlotName::PendingAuth],
        );
        let b = compute_pre_state_signature(
            &base_wm(),
            &[WatchSlotName::PendingAuth, WatchSlotName::PendingModal],
        );
        assert_eq!(a, b);
    }

    #[test]
    fn signature_is_16_char_hex() {
        let sig = compute_pre_state_signature(&base_wm(), &[]);
        assert_eq!(sig.0.len(), 16);
        assert!(sig.0.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
