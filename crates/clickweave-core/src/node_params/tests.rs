use super::*;

#[test]
fn click_target_text_serde_roundtrip() {
    let target = ClickTarget::Text {
        text: "Submit".into(),
    };
    let json = serde_json::to_string(&target).unwrap();
    assert!(json.contains("\"type\":\"Text\""));
    let back: ClickTarget = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, ClickTarget::Text { text } if text == "Submit"));
}

#[test]
fn click_target_coordinates_serde_roundtrip() {
    let target = ClickTarget::Coordinates { x: 100.0, y: 200.0 };
    let json = serde_json::to_string(&target).unwrap();
    assert!(json.contains("\"type\":\"Coordinates\""));
    let back: ClickTarget = serde_json::from_str(&json).unwrap();
    assert!(
        matches!(back, ClickTarget::Coordinates { x, y } if (x - 100.0).abs() < f64::EPSILON && (y - 200.0).abs() < f64::EPSILON)
    );
}

#[test]
fn click_target_text_method() {
    let text = ClickTarget::Text {
        text: "Submit".into(),
    };
    assert_eq!(text.text(), "Submit");

    let coords = ClickTarget::Coordinates { x: 10.0, y: 20.0 };
    assert_eq!(coords.text(), "");
}

#[test]
fn click_params_default_has_no_verification() {
    let params = ClickParams::default();
    assert!(params.verification.is_empty());
    assert!(HasVerification::verification(&params).is_none());
}

#[test]
fn cdp_wait_params_default_timeout() {
    let params = CdpWaitParams::default();
    assert_eq!(params.timeout_ms, 10_000);
}

#[test]
fn cdp_handle_dialog_params_default_accept() {
    let params = CdpHandleDialogParams::default();
    assert!(params.accept);
    assert!(params.prompt_text.is_none());
}

#[test]
fn cdp_target_serde_roundtrip() {
    let target = CdpTarget::ExactLabel("Friends".into());
    let json = serde_json::to_string(&target).unwrap();
    assert!(json.contains("\"kind\":\"ExactLabel\""));
    let back: CdpTarget = serde_json::from_str(&json).unwrap();
    assert_eq!(back, CdpTarget::ExactLabel("Friends".into()));
}

#[test]
fn cdp_target_as_str() {
    assert_eq!(CdpTarget::ExactLabel("a".into()).as_str(), "a");
    assert_eq!(CdpTarget::Intent("b".into()).as_str(), "b");
    assert_eq!(CdpTarget::ResolvedUid("c".into()).as_str(), "c");
}

#[test]
fn cdp_click_params_new_format_roundtrip() {
    let params = CdpClickParams {
        target: CdpTarget::Intent("message input".into()),
        ..Default::default()
    };
    let json = serde_json::to_string(&params).unwrap();
    let back: CdpClickParams = serde_json::from_str(&json).unwrap();
    assert_eq!(back.target, CdpTarget::Intent("message input".into()));
}

#[test]
fn cdp_click_params_legacy_uid_deserializes_as_exact_label() {
    let json = r#"{"uid": "Friends"}"#;
    let params: CdpClickParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.target, CdpTarget::ExactLabel("Friends".into()));
}

#[test]
fn cdp_hover_params_legacy_uid_deserializes_as_exact_label() {
    let json = r#"{"uid": "Submit"}"#;
    let params: CdpHoverParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.target, CdpTarget::ExactLabel("Submit".into()));
}

#[test]
fn cdp_click_params_legacy_preserves_verification_fields() {
    let json = r#"{"uid": "OK", "verification_method": "Vlm", "verification_assertion": "button visible"}"#;
    let params: CdpClickParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.target, CdpTarget::ExactLabel("OK".into()));
    let resolved =
        HasVerification::resolved_verification(&params).expect("verification should resolve");
    assert_eq!(resolved.method, VerificationMethod::Vlm);
    assert_eq!(resolved.assertion, "button visible");
}

#[test]
fn cdp_click_params_missing_both_fields_defaults_to_intent() {
    let json = r#"{}"#;
    let params: CdpClickParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.target, CdpTarget::default());
    assert!(matches!(params.target, CdpTarget::Intent(ref s) if s.is_empty()));
}

// ── AxTarget + AX params ────────────────────────────────────────────

#[test]
fn ax_target_descriptor_serde_roundtrip() {
    let target = AxTarget::Descriptor {
        role: "AXButton".into(),
        name: "Submit".into(),
        parent_name: None,
    };
    let json = serde_json::to_string(&target).unwrap();
    assert!(json.contains("\"kind\":\"Descriptor\""));
    let back: AxTarget = serde_json::from_str(&json).unwrap();
    assert_eq!(back, target);
}

#[test]
fn ax_target_as_str_prefers_name_over_uid() {
    assert_eq!(
        AxTarget::Descriptor {
            role: "AXButton".into(),
            name: "OK".into(),
            parent_name: None,
        }
        .as_str(),
        "OK"
    );
    assert_eq!(AxTarget::ResolvedUid("a42g3".into()).as_str(), "a42g3");
}

#[test]
fn ax_target_default_is_empty_resolved_uid() {
    assert_eq!(AxTarget::default(), AxTarget::ResolvedUid(String::new()));
}

#[test]
fn ax_click_params_descriptor_roundtrip() {
    let params = AxClickParams {
        target: AxTarget::Descriptor {
            role: "AXButton".into(),
            name: "Continue".into(),
            parent_name: Some("Toolbar".into()),
        },
        ..Default::default()
    };
    let json = serde_json::to_string(&params).unwrap();
    let back: AxClickParams = serde_json::from_str(&json).unwrap();
    assert_eq!(back.target, params.target);
}

#[test]
fn ax_click_params_legacy_uid_deserializes_as_resolved_uid() {
    // Agent-generated workflow nodes start with ResolvedUid captured from
    // the live snapshot; old persisted workflows may use the even-older
    // top-level `{"uid": "..."}` shape.
    let json = r#"{"uid": "a42g3"}"#;
    let params: AxClickParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.target, AxTarget::ResolvedUid("a42g3".into()));
}

#[test]
fn ax_set_value_params_roundtrip_with_value() {
    let params = AxSetValueParams {
        target: AxTarget::Descriptor {
            role: "AXTextField".into(),
            name: "Search".into(),
            parent_name: None,
        },
        value: "hello world".into(),
        ..Default::default()
    };
    let json = serde_json::to_string(&params).unwrap();
    let back: AxSetValueParams = serde_json::from_str(&json).unwrap();
    assert_eq!(back.value, "hello world");
    assert_eq!(back.target, params.target);
}

#[test]
fn ax_set_value_params_legacy_uid_preserves_value() {
    let json = r#"{"uid": "a12g1", "value": "typed"}"#;
    let params: AxSetValueParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.target, AxTarget::ResolvedUid("a12g1".into()));
    assert_eq!(params.value, "typed");
}

#[test]
fn ax_select_params_descriptor_roundtrip() {
    let params = AxSelectParams {
        target: AxTarget::Descriptor {
            role: "AXRow".into(),
            name: "Wi-Fi".into(),
            parent_name: Some("Sidebar".into()),
        },
        ..Default::default()
    };
    let json = serde_json::to_string(&params).unwrap();
    let back: AxSelectParams = serde_json::from_str(&json).unwrap();
    assert_eq!(back.target, params.target);
}

#[test]
fn ax_params_empty_json_defaults_to_resolved_uid_empty() {
    let params: AxClickParams = serde_json::from_str("{}").unwrap();
    assert_eq!(params.target, AxTarget::ResolvedUid(String::new()));
}

#[test]
fn focus_window_params_legacy_string_window_id_deserializes_as_u64() {
    let json = r#"{"method":"WindowId","value":"42","bring_to_front":true,"app_kind":"Native"}"#;
    let params: FocusWindowParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.target, FocusTarget::WindowId(42));
    assert!(params.bring_to_front);
}

#[test]
fn focus_window_params_legacy_string_pid_deserializes_as_u32() {
    let json = r#"{"method":"Pid","value":"1234","bring_to_front":true,"app_kind":"Native"}"#;
    let params: FocusWindowParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.target, FocusTarget::Pid(1234));
}

#[test]
fn focus_window_params_numeric_window_id_roundtrips() {
    let params = FocusWindowParams {
        target: FocusTarget::WindowId(42),
        bring_to_front: true,
        ..Default::default()
    };
    let json = serde_json::to_string(&params).unwrap();
    assert!(json.contains("\"method\":\"WindowId\""));
    assert!(json.contains("\"value\":42"));
    let back: FocusWindowParams = serde_json::from_str(&json).unwrap();
    assert_eq!(back.target, FocusTarget::WindowId(42));
}

#[test]
fn focus_window_params_app_name_roundtrips() {
    let params = FocusWindowParams {
        target: FocusTarget::AppName("Safari".into()),
        bring_to_front: true,
        ..Default::default()
    };
    let json = serde_json::to_string(&params).unwrap();
    let back: FocusWindowParams = serde_json::from_str(&json).unwrap();
    assert_eq!(back.target, FocusTarget::AppName("Safari".into()));
}

#[test]
fn focus_window_params_legacy_null_value_preserves_app_name_method() {
    // Legacy shape: {method:"AppName", value:null} — the UI editor only
    // admits {AppName,WindowId,Pid}, so unconfigured nodes must decode
    // back to an empty-string AppName. Regression guard against an earlier
    // refactor that mapped them to a dedicated None variant (UI-unsafe).
    let json = r#"{"method":"AppName","value":null,"bring_to_front":true}"#;
    let params: FocusWindowParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.target, FocusTarget::AppName(String::new()));
}

#[test]
fn focus_window_params_legacy_none_method_becomes_app_name_default() {
    // Legacy shape: {method:"None"} — we removed the None variant from
    // the tagged enum, but workflow files that used it must still load.
    let json = r#"{"method":"None","bring_to_front":true}"#;
    let params: FocusWindowParams = serde_json::from_str(json).unwrap();
    assert_eq!(params.target, FocusTarget::AppName(String::new()));
}

#[test]
fn click_params_legacy_split_verification_migrates_to_config() {
    // Legacy disk shape: verification_method / verification_assertion as
    // two separate sibling fields on the action params struct.
    let json = r#"{
            "target": null,
            "button": "Left",
            "click_count": 1,
            "verification_method": "Vlm",
            "verification_assertion": "button clicked"
        }"#;
    let params: ClickParams = serde_json::from_str(json).unwrap();
    let resolved =
        HasVerification::resolved_verification(&params).expect("verification should resolve");
    assert_eq!(resolved.method, VerificationMethod::Vlm);
    assert_eq!(resolved.assertion, "button clicked");
}

#[test]
fn click_params_partial_verification_does_not_resolve() {
    // Only one half of the verification pair present — treat as "not
    // configured" rather than half-filling the config. The half that
    // *was* present still round-trips so users don't silently lose
    // their partial input.
    let json = r#"{
            "target": null,
            "button": "Left",
            "click_count": 1,
            "verification_assertion": "orphaned assertion"
        }"#;
    let params: ClickParams = serde_json::from_str(json).unwrap();
    assert!(HasVerification::resolved_verification(&params).is_none());
    assert_eq!(
        params.verification.verification_assertion.as_deref(),
        Some("orphaned assertion")
    );
}

#[test]
fn click_params_new_shape_roundtrips() {
    let params = ClickParams {
        target: None,
        button: MouseButton::Left,
        click_count: 1,
        verification: VerificationConfig::new(VerificationMethod::Vlm, "assertion text"),
    };
    let json = serde_json::to_string(&params).unwrap();
    assert!(json.contains("\"verification_method\":\"Vlm\""));
    assert!(json.contains("\"verification_assertion\":\"assertion text\""));
    let back: ClickParams = serde_json::from_str(&json).unwrap();
    let resolved =
        HasVerification::resolved_verification(&back).expect("verification should resolve");
    assert_eq!(resolved.method, VerificationMethod::Vlm);
    assert_eq!(resolved.assertion, "assertion text");
}

#[test]
fn trace_event_kind_snake_case_serialization() {
    let json = serde_json::to_string(&TraceEventKind::ToolCall).unwrap();
    assert_eq!(json, "\"tool_call\"");
    let back: TraceEventKind = serde_json::from_str(&json).unwrap();
    assert_eq!(back, TraceEventKind::ToolCall);
}

#[test]
fn trace_event_kind_unknown_string_parses_as_unknown() {
    let back: TraceEventKind = serde_json::from_str("\"some_future_event\"").unwrap();
    assert_eq!(back, TraceEventKind::Unknown);
}

#[test]
fn trace_event_kind_legacy_strings_all_parse() {
    let strings = [
        "node_started",
        "tool_call",
        "tool_result",
        "step_completed",
        "step_failed",
        "branch_evaluated",
        "loop_iteration",
        "target_resolved",
        "action_verification",
        "ambiguity_resolved",
        "element_resolved",
        "match_disambiguated",
        "app_resolved",
        "cdp_connected",
        "cdp_click",
        "cdp_hover",
        "cdp_fill",
        "vision_summary",
        "variable_set",
        "retry",
        "supervision_retry",
    ];
    for s in strings {
        let json = format!("\"{s}\"");
        let kind: TraceEventKind = serde_json::from_str(&json).expect(s);
        assert_ne!(
            kind,
            TraceEventKind::Unknown,
            "'{s}' should parse to a known variant"
        );
        assert_eq!(kind.as_str(), s, "as_str should round-trip for '{s}'");
    }
}

#[test]
fn artifact_kind_legacy_values_parse_as_other() {
    for legacy in ["\"Ocr\"", "\"TemplateMatch\"", "\"Log\""] {
        let kind: ArtifactKind = serde_json::from_str(legacy).unwrap();
        assert_eq!(kind, ArtifactKind::Other);
    }
}

/// Lock the `From<&str>` match table to `as_str` so a new variant
/// added to only one half surfaces as a test failure instead of
/// silently routing through `Unknown`.
#[test]
fn trace_event_kind_from_str_round_trips_as_str() {
    let all = [
        TraceEventKind::NodeStarted,
        TraceEventKind::ToolCall,
        TraceEventKind::ToolResult,
        TraceEventKind::StepCompleted,
        TraceEventKind::StepFailed,
        TraceEventKind::BranchEvaluated,
        TraceEventKind::LoopIteration,
        TraceEventKind::TargetResolved,
        TraceEventKind::ActionVerification,
        TraceEventKind::AmbiguityResolved,
        TraceEventKind::ElementResolved,
        TraceEventKind::MatchDisambiguated,
        TraceEventKind::AppResolved,
        TraceEventKind::CdpConnected,
        TraceEventKind::CdpClick,
        TraceEventKind::CdpHover,
        TraceEventKind::CdpFill,
        TraceEventKind::AxClick,
        TraceEventKind::AxSetValue,
        TraceEventKind::AxSelect,
        TraceEventKind::VisionSummary,
        TraceEventKind::VariableSet,
        TraceEventKind::Retry,
        TraceEventKind::SupervisionRetry,
    ];
    for kind in all {
        let s = kind.as_str();
        let round_tripped: TraceEventKind = s.into();
        assert_eq!(
            round_tripped, kind,
            "From<&str> and as_str disagree for '{s}'",
        );
    }
    // Unknown strings must land in Unknown.
    let unknown: TraceEventKind = "some_future_event".into();
    assert_eq!(unknown, TraceEventKind::Unknown);
}
