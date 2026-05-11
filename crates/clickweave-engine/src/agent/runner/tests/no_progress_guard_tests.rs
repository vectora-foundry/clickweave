use super::*;
use crate::agent::world_model::{CdpPageState, Fresh, FreshnessSource, OcrMatch};
use clickweave_core::cdp::CdpFindElementMatch;
use serde_json::json;

fn sig(tool_name: &str, arguments: serde_json::Value, context: &str) -> ActionProgressSignature {
    ActionProgressSignature {
        tool_name: tool_name.to_string(),
        arguments,
        context_signature: context.to_string(),
    }
}

#[test]
fn detects_two_action_cycle_in_same_stable_context() {
    let recent = VecDeque::from(vec![
        sig(
            "cdp_fill",
            json!({"uid": "d1", "value": "synthetic"}),
            "ctx",
        ),
        sig("cdp_click", json!({"uid": "d2"}), "ctx"),
        sig(
            "cdp_fill",
            json!({"uid": "d1", "value": "synthetic"}),
            "ctx",
        ),
        sig("cdp_click", json!({"uid": "d2"}), "ctx"),
    ]);

    assert_eq!(
        detect_repeated_action_cycle(&recent),
        Some(vec!["cdp_fill".to_string(), "cdp_click".to_string()])
    );
}

#[test]
fn detects_three_action_cycle_in_same_stable_context() {
    let recent = VecDeque::from(vec![
        sig(
            "cdp_fill",
            json!({"uid": "d-search", "value": "synthetic"}),
            "ctx",
        ),
        sig("cdp_click", json!({"uid": "d-filter"}), "ctx"),
        sig("cdp_click", json!({"uid": "d-cancel"}), "ctx"),
        sig(
            "cdp_fill",
            json!({"uid": "d-search", "value": "synthetic"}),
            "ctx",
        ),
        sig("cdp_click", json!({"uid": "d-filter"}), "ctx"),
        sig("cdp_click", json!({"uid": "d-cancel"}), "ctx"),
    ]);

    assert_eq!(
        detect_repeated_action_cycle(&recent),
        Some(vec![
            "cdp_fill".to_string(),
            "cdp_click".to_string(),
            "cdp_click".to_string(),
        ])
    );
}

#[test]
fn ignores_same_pair_after_context_progress() {
    let recent = VecDeque::from(vec![
        sig(
            "cdp_fill",
            json!({"uid": "d1", "value": "synthetic"}),
            "ctx-a",
        ),
        sig("cdp_click", json!({"uid": "d2"}), "ctx-a"),
        sig(
            "cdp_fill",
            json!({"uid": "d1", "value": "synthetic"}),
            "ctx-b",
        ),
        sig("cdp_click", json!({"uid": "d2"}), "ctx-b"),
    ]);

    assert_eq!(detect_repeated_action_cycle(&recent), None);
}

#[test]
fn stable_context_falls_back_to_page_fingerprint_without_elements() {
    let mut wm = WorldModel::default();
    wm.cdp_page = Some(Fresh {
        value: CdpPageState {
            url: "app://synthetic/page".to_string(),
            page_fingerprint: "count=1;hash=a".to_string(),
            element_inventory: Vec::new(),
        },
        written_at: 1,
        source: FreshnessSource::DirectObservation,
        ttl_steps: Some(2),
    });
    let before = stable_no_progress_context_signature(&wm);

    wm.cdp_page.as_mut().unwrap().value.page_fingerprint = "count=2;hash=b".to_string();
    let after = stable_no_progress_context_signature(&wm);

    assert_ne!(
        before, after,
        "CDP element-surface progress must reset no-progress tracking"
    );
}

fn cdp(uid: &str, role: &str, label: &str, tag: &str) -> ObservedElement {
    ObservedElement::Cdp(CdpFindElementMatch {
        uid: uid.to_string(),
        role: role.to_string(),
        label: label.to_string(),
        tag: tag.to_string(),
        disabled: false,
        parent_role: None,
        parent_name: None,
        ..Default::default()
    })
}

fn wm_with_cdp_elements(page_fingerprint: &str, elements: Vec<ObservedElement>) -> WorldModel {
    let mut wm = WorldModel::default();
    wm.cdp_page = Some(Fresh {
        value: CdpPageState {
            url: "app://synthetic/page".to_string(),
            page_fingerprint: page_fingerprint.to_string(),
            element_inventory: Vec::new(),
        },
        written_at: 1,
        source: FreshnessSource::DirectObservation,
        ttl_steps: Some(2),
    });
    wm.elements = Some(Fresh {
        value: elements,
        written_at: 1,
        source: FreshnessSource::DirectObservation,
        ttl_steps: Some(2),
    });
    wm
}

#[test]
fn stable_context_ignores_cdp_order_and_uid_churn_when_elements_exist() {
    let before = wm_with_cdp_elements(
        "count=2;hash=uid-a",
        vec![
            cdp("d1", "textbox", "Search synthetic channels", "input"),
            cdp("d2", "button", "Cancel search", "button"),
        ],
    );
    let after = wm_with_cdp_elements(
        "count=2;hash=uid-b",
        vec![
            cdp("d9", "button", "Cancel search", "button"),
            cdp("d8", "textbox", "Search synthetic channels", "input"),
        ],
    );

    assert_eq!(
        stable_no_progress_context_signature(&before),
        stable_no_progress_context_signature(&after),
        "element order, uid churn, and derived page-fingerprint churn must not look like progress"
    );
}

#[test]
fn stable_context_changes_when_semantic_element_surface_changes() {
    let before = wm_with_cdp_elements(
        "count=1;hash=a",
        vec![cdp("d1", "button", "Open synthetic item", "button")],
    );
    let after = wm_with_cdp_elements(
        "count=1;hash=b",
        vec![cdp("d1", "button", "Synthetic item open", "button")],
    );

    assert_ne!(
        stable_no_progress_context_signature(&before),
        stable_no_progress_context_signature(&after),
        "semantic element changes must still reset no-progress tracking"
    );
}

#[test]
fn stable_context_changes_when_cdp_visible_text_changes() {
    let mut before_el = cdp("d1", "button", "Chat with Foo Bar Baz", "button");
    if let ObservedElement::Cdp(el) = &mut before_el {
        el.visible_text = "Foo Bar Baz preview one".to_string();
    }
    let mut after_el = before_el.clone();
    if let ObservedElement::Cdp(el) = &mut after_el {
        el.visible_text = "Foo Bar Baz preview two".to_string();
    }

    let before = wm_with_cdp_elements("count=1;hash=a", vec![before_el]);
    let after = wm_with_cdp_elements("count=1;hash=b", vec![after_el]);

    assert_ne!(
        stable_no_progress_context_signature(&before),
        stable_no_progress_context_signature(&after),
        "visible text changes must reset no-progress tracking even when the accessibility label is unchanged"
    );
}

#[test]
fn stable_context_ignores_ocr_confidence_jitter() {
    let mut before = WorldModel::default();
    before.elements = Some(Fresh {
        value: vec![ObservedElement::Ocr(OcrMatch {
            text: "Synthetic status".to_string(),
            x: 101,
            y: 202,
            width: 98,
            height: 19,
            confidence: 0.91,
        })],
        written_at: 1,
        source: FreshnessSource::DirectObservation,
        ttl_steps: Some(2),
    });
    let mut after = before.clone();
    if let Some(elements) = after.elements.as_mut()
        && let Some(ObservedElement::Ocr(match_)) = elements.value.first_mut()
    {
        match_.x = 104;
        match_.y = 206;
        match_.confidence = 0.73;
    }

    assert_eq!(
        stable_no_progress_context_signature(&before),
        stable_no_progress_context_signature(&after),
        "small OCR coordinate jitter and confidence changes must not reset the guard"
    );
}

#[test]
fn stale_cdp_uid_errors_are_recognized_and_wrapped() {
    assert!(is_stale_cdp_uid_error(
        "cdp_fill",
        "No node with given id found"
    ));
    assert!(!is_stale_cdp_uid_error(
        "ax_click",
        "No node with given id found"
    ));

    let nudge = build_stale_cdp_uid_nudge("No node with given id found");
    assert!(nudge.starts_with(STALE_CDP_UID_PREFIX));
    assert!(nudge.contains("Rediscover the target"));
    assert!(!nudge.contains("cdp_evaluate_script"));
}

#[test]
fn recovery_nudges_do_not_recommend_eval_script_for_discovery() {
    let repeated = build_no_progress_nudge("cdp_click", 2, "clicked");
    let cycle = build_action_cycle_nudge("cdp_find_elements -> cdp_click", "clicked");
    let post_text = build_post_text_submit_nudge(3, r#"{"matches":[]}"#);

    assert!(repeated.contains("cdp_find_elements"));
    assert!(cycle.contains("cdp_get_element_context"));
    assert!(post_text.contains("cdp_press_key"));
    assert!(!repeated.contains("cdp_evaluate_script"));
    assert!(!cycle.contains("cdp_evaluate_script"));
    assert!(!post_text.contains("cdp_evaluate_script"));
}

#[test]
fn post_text_send_search_helpers_detect_empty_send_searches() {
    assert!(is_send_submit_cdp_search(
        &serde_json::json!({"query":"Send", "role":"button"})
    ));
    assert!(is_send_submit_cdp_search(
        &serde_json::json!({"query":"send button"})
    ));
    assert!(is_send_submit_cdp_search(
        &serde_json::json!({"query":"Submit"})
    ));
    assert!(!is_send_submit_cdp_search(
        &serde_json::json!({"query":"Message", "role":"textbox"})
    ));

    assert_eq!(
        cdp_find_elements_has_matches(r#"{"matches":[],"inventory":[]}"#),
        Some(false)
    );
    assert_eq!(
        cdp_find_elements_has_matches(
            r#"{"matches":[{"uid":"d1","role":"button","label":"Send"}]}"#
        ),
        Some(true)
    );
}
