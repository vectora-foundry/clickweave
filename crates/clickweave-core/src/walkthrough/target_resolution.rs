use super::types::{ActionConfidence, OcrAnnotation, TargetCandidate};

/// Maximum distance (pixels) for matching OCR text to a click point.
pub const OCR_PROXIMITY_PX: f64 = 50.0;

/// CDP element data resolved at record time via the Chrome DevTools Protocol.
pub(crate) struct CdpElementData {
    pub name: String,
    pub role: Option<String>,
    pub href: Option<String>,
    pub parent_role: Option<String>,
    pub parent_name: Option<String>,
}

/// Enrichment data collected during the click lookahead pass.
///
/// Aggregated from the enrichment events that follow a `MouseClicked` event
/// before the next action event.
pub(crate) struct ClickEnrichment<'a> {
    /// Accessibility label and role.
    pub ax_label: Option<(String, Option<String>)>,
    /// VLM-resolved label.
    pub vlm_label: Option<String>,
    /// OCR annotations from the screenshot.
    pub ocr_annotations: Option<&'a Vec<OcrAnnotation>>,
    /// Click crop image (path, base64).
    pub crop_candidate: Option<(String, String)>,
    /// CDP-resolved element.
    pub cdp_resolved: Option<CdpElementData>,
    /// Click coordinates.
    pub click_x: f64,
    pub click_y: f64,
}

/// Build target candidates from click enrichment data.
///
/// Returns candidates in priority order: CDP > accessibility label > VLM > OCR > image crop > coordinates.
pub(crate) fn build_target_candidates(enrichment: &ClickEnrichment<'_>) -> Vec<TargetCandidate> {
    let mut candidates = Vec::new();

    // CDP element from click listener is the highest-priority target.
    if let Some(cdp) = &enrichment.cdp_resolved {
        candidates.push(TargetCandidate::CdpElement {
            name: cdp.name.clone(),
            role: cdp.role.clone(),
            href: cdp.href.clone(),
            parent_role: cdp.parent_role.clone(),
            parent_name: cdp.parent_name.clone(),
        });
    }

    // Accessibility label is the most reliable target.
    // Skip empty labels — these come from elements with a subrole but
    // no display text (e.g. unlabeled buttons), and would suppress VLM
    // fallback without providing a usable target string.
    if let Some((label, role)) = &enrichment.ax_label
        && !label.is_empty()
    {
        candidates.push(TargetCandidate::AccessibilityLabel {
            label: label.clone(),
            role: role.clone(),
        });
    }

    // VLM label as second-best target (after actionable AX labels).
    if let Some(label) = &enrichment.vlm_label {
        candidates.push(TargetCandidate::VlmLabel {
            label: label.clone(),
        });
    }

    // OCR text as fallback.
    if let Some(annotations) = enrichment.ocr_annotations {
        let mut nearest: Option<(&OcrAnnotation, f64)> = None;
        for ann in annotations.iter() {
            let dist = ((ann.x - enrichment.click_x).powi(2)
                + (ann.y - enrichment.click_y).powi(2))
            .sqrt();
            if dist <= OCR_PROXIMITY_PX && (nearest.is_none() || dist < nearest.unwrap().1) {
                nearest = Some((ann, dist));
            }
        }
        if let Some((ann, _)) = nearest {
            candidates.push(TargetCandidate::OcrText {
                text: ann.text.clone(),
            });
        }
    }

    // Image crop as fallback (before coordinates).
    if let Some((crop_path, crop_b64)) = &enrichment.crop_candidate {
        candidates.push(TargetCandidate::ImageCrop {
            path: crop_path.clone(),
            image_b64: crop_b64.clone(),
        });
    }

    // Always add coordinates as fallback.
    candidates.push(TargetCandidate::Coordinates {
        x: enrichment.click_x,
        y: enrichment.click_y,
    });

    candidates
}

/// Score the confidence of a click action based on its target candidates.
pub(crate) fn score_confidence(candidates: &[TargetCandidate]) -> ActionConfidence {
    let has_image_crop = candidates
        .iter()
        .any(|c| matches!(c, TargetCandidate::ImageCrop { .. }));

    if candidates
        .iter()
        .any(|c| matches!(c, TargetCandidate::CdpElement { .. }))
        || candidates.iter().any(|c| c.is_actionable_ax_label())
    {
        ActionConfidence::High
    } else if candidates.iter().any(|c| {
        matches!(
            c,
            TargetCandidate::VlmLabel { .. } | TargetCandidate::OcrText { .. }
        )
    }) || has_image_crop
    {
        ActionConfidence::Medium
    } else {
        ActionConfidence::Low
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::make_event;
    use super::*;
    use crate::MouseButton;
    use crate::walkthrough::types::*;
    use uuid::Uuid;

    #[test]
    fn test_click_with_nearby_ocr_gets_medium_confidence() {
        use crate::walkthrough::synthesis::normalize_events;

        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::MouseClicked {
                    x: 100.0,
                    y: 200.0,
                    button: MouseButton::Left,
                    click_count: 1,
                    modifiers: vec![],
                },
            ),
            make_event(
                1100,
                WalkthroughEventKind::OcrCaptured {
                    annotations: vec![
                        OcrAnnotation {
                            text: "Submit".into(),
                            x: 102.0,
                            y: 198.0,
                        },
                        OcrAnnotation {
                            text: "Cancel".into(),
                            x: 300.0,
                            y: 198.0,
                        },
                    ],
                    click_x: 100.0,
                    click_y: 200.0,
                },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0].kind,
            WalkthroughActionKind::Click { .. }
        ));
        assert!(
            actions[0]
                .target_candidates
                .iter()
                .any(|c| matches!(c, TargetCandidate::OcrText { text } if text == "Submit"))
        );
        assert_eq!(actions[0].confidence, ActionConfidence::Medium);
    }

    #[test]
    fn test_click_without_ocr_gets_low_confidence() {
        use crate::walkthrough::synthesis::normalize_events;

        let events = vec![make_event(
            1000,
            WalkthroughEventKind::MouseClicked {
                x: 100.0,
                y: 200.0,
                button: MouseButton::Left,
                click_count: 1,
                modifiers: vec![],
            },
        )];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].confidence, ActionConfidence::Low);
        assert!(
            actions[0]
                .target_candidates
                .iter()
                .any(|c| matches!(c, TargetCandidate::Coordinates { .. }))
        );
    }

    #[test]
    fn test_click_with_vlm_label_gets_medium_confidence() {
        use crate::walkthrough::synthesis::normalize_events;

        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::MouseClicked {
                    x: 100.0,
                    y: 200.0,
                    button: MouseButton::Left,
                    click_count: 1,
                    modifiers: vec![],
                },
            ),
            make_event(
                1000,
                WalkthroughEventKind::VlmLabelResolved {
                    label: "Send".to_string(),
                },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        assert!(
            actions[0]
                .target_candidates
                .iter()
                .any(|c| matches!(c, TargetCandidate::VlmLabel { label } if label == "Send"))
        );
        // VLM label alone should be at least Medium confidence
        assert!(actions[0].confidence != ActionConfidence::Low);
    }

    #[test]
    fn test_click_with_ax_label_and_vlm_label_keeps_ax_first() {
        use crate::walkthrough::synthesis::normalize_events;

        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::MouseClicked {
                    x: 100.0,
                    y: 200.0,
                    button: MouseButton::Left,
                    click_count: 1,
                    modifiers: vec![],
                },
            ),
            make_event(
                1000,
                WalkthroughEventKind::AccessibilityElementCaptured {
                    label: "Submit".to_string(),
                    role: Some("AXButton".to_string()),
                    subrole: None,
                },
            ),
            make_event(
                1000,
                WalkthroughEventKind::VlmLabelResolved {
                    label: "Submit Button".to_string(),
                },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        // AX label should be first candidate
        assert!(matches!(
            &actions[0].target_candidates[0],
            TargetCandidate::AccessibilityLabel { label, .. } if label == "Submit"
        ));
        // VLM label should be second
        assert!(matches!(
            &actions[0].target_candidates[1],
            TargetCandidate::VlmLabel { label } if label == "Submit Button"
        ));
        // Actionable AX label means High confidence
        assert_eq!(actions[0].confidence, ActionConfidence::High);
    }

    #[test]
    fn test_empty_ax_label_not_added_as_candidate() {
        use crate::walkthrough::synthesis::normalize_events;

        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::MouseClicked {
                    x: 100.0,
                    y: 200.0,
                    button: MouseButton::Left,
                    click_count: 1,
                    modifiers: vec![],
                },
            ),
            make_event(
                1000,
                WalkthroughEventKind::AccessibilityElementCaptured {
                    label: String::new(),
                    role: Some("AXButton".to_string()),
                    subrole: Some("AXSortButton".to_string()),
                },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        // Empty label should NOT become an AccessibilityLabel candidate
        assert!(
            !actions[0]
                .target_candidates
                .iter()
                .any(|c| matches!(c, TargetCandidate::AccessibilityLabel { .. })),
            "empty AX label should not be a candidate"
        );
    }

    #[test]
    fn test_cdp_click_resolved_creates_cdp_element_candidate() {
        use crate::walkthrough::synthesis::normalize_events;

        let click_id = Uuid::new_v4();
        let events = vec![
            WalkthroughEvent {
                id: click_id,
                timestamp: 1000,
                kind: WalkthroughEventKind::MouseClicked {
                    x: 45.0,
                    y: 473.0,
                    button: MouseButton::Left,
                    click_count: 1,
                    modifiers: vec![],
                },
            },
            WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: 1000,
                kind: WalkthroughEventKind::CdpClickResolved {
                    name: "Direct Messages".to_string(),
                    role: Some("treeitem".to_string()),
                    href: None,
                    parent_role: None,
                    parent_name: None,
                    click_event_id: click_id,
                },
            },
            // AX label is the window title — non-actionable.
            make_event(
                1000,
                WalkthroughEventKind::AccessibilityElementCaptured {
                    label: "MyApp - Main Window".to_string(),
                    role: Some("AXWindow".to_string()),
                    subrole: None,
                },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        // CDP element should be the first candidate (highest priority).
        assert!(
            matches!(
                &actions[0].target_candidates[0],
                TargetCandidate::CdpElement { name, role, .. }
                    if name == "Direct Messages" && role.as_deref() == Some("treeitem")
            ),
            "Expected CdpElement as first candidate, got: {:?}",
            &actions[0].target_candidates
        );
        // CDP element presence means High confidence.
        assert_eq!(actions[0].confidence, ActionConfidence::High);
    }

    // --- Unit tests for the extracted functions ---

    #[test]
    fn test_build_target_candidates_priority_order() {
        let enrichment = ClickEnrichment {
            ax_label: Some(("Submit".into(), Some("AXButton".into()))),
            vlm_label: Some("Submit Button".into()),
            ocr_annotations: None,
            crop_candidate: None,
            cdp_resolved: Some(CdpElementData {
                name: "Submit".into(),
                role: Some("button".into()),
                href: None,
                parent_role: None,
                parent_name: None,
            }),
            click_x: 100.0,
            click_y: 200.0,
        };
        let candidates = build_target_candidates(&enrichment);
        // CDP first, then AX, then VLM, then coordinates
        assert!(matches!(&candidates[0], TargetCandidate::CdpElement { .. }));
        assert!(matches!(
            &candidates[1],
            TargetCandidate::AccessibilityLabel { .. }
        ));
        assert!(matches!(&candidates[2], TargetCandidate::VlmLabel { .. }));
        assert!(matches!(
            &candidates[3],
            TargetCandidate::Coordinates { .. }
        ));
    }

    #[test]
    fn test_build_target_candidates_empty_ax_label_skipped() {
        let enrichment = ClickEnrichment {
            ax_label: Some((String::new(), Some("AXButton".into()))),
            vlm_label: None,
            ocr_annotations: None,
            crop_candidate: None,
            cdp_resolved: None,
            click_x: 100.0,
            click_y: 200.0,
        };
        let candidates = build_target_candidates(&enrichment);
        assert!(
            !candidates
                .iter()
                .any(|c| matches!(c, TargetCandidate::AccessibilityLabel { .. }))
        );
    }

    #[test]
    fn test_score_confidence_high_with_cdp() {
        let candidates = vec![
            TargetCandidate::CdpElement {
                name: "x".into(),
                role: None,
                href: None,
                parent_role: None,
                parent_name: None,
            },
            TargetCandidate::Coordinates { x: 0.0, y: 0.0 },
        ];
        assert_eq!(score_confidence(&candidates), ActionConfidence::High);
    }

    #[test]
    fn test_score_confidence_medium_with_vlm() {
        let candidates = vec![
            TargetCandidate::VlmLabel { label: "x".into() },
            TargetCandidate::Coordinates { x: 0.0, y: 0.0 },
        ];
        assert_eq!(score_confidence(&candidates), ActionConfidence::Medium);
    }

    #[test]
    fn test_score_confidence_low_coordinates_only() {
        let candidates = vec![TargetCandidate::Coordinates { x: 0.0, y: 0.0 }];
        assert_eq!(score_confidence(&candidates), ActionConfidence::Low);
    }
}
