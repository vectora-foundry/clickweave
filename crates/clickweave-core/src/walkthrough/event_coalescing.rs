use uuid::Uuid;

use super::types::{WalkthroughAction, WalkthroughActionKind};

/// Idle gap threshold for text coalescing (milliseconds).
pub(crate) const TEXT_IDLE_GAP_MS: u64 = 2000;

/// Maximum gap between scroll events to coalesce (milliseconds).
pub(crate) const SCROLL_COALESCE_GAP_MS: u64 = 300;

/// Maximum gap between identical key presses to coalesce (milliseconds).
pub(crate) const KEY_COALESCE_GAP_MS: u64 = 150;

/// Flush accumulated text buffer into a single TypeText action.
pub(crate) fn flush_text(
    buf: &mut Vec<(Uuid, u64, String)>,
    actions: &mut Vec<WalkthroughAction>,
    current_app: &Option<String>,
) {
    if buf.is_empty() {
        return;
    }
    let text: String = buf.iter().map(|(_, _, t)| t.as_str()).collect();
    let source_ids: Vec<Uuid> = buf.iter().map(|(id, _, _)| *id).collect();
    actions.push(WalkthroughAction::new(
        WalkthroughActionKind::TypeText { text },
        current_app.clone(),
        source_ids,
    ));
    buf.clear();
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::make_event;
    use crate::MouseButton;
    use crate::walkthrough::types::*;

    #[test]
    fn test_contiguous_text_coalesced() {
        use crate::walkthrough::synthesis::normalize_events;

        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::TextCommitted { text: "h".into() },
            ),
            make_event(
                1050,
                WalkthroughEventKind::TextCommitted { text: "e".into() },
            ),
            make_event(
                1100,
                WalkthroughEventKind::TextCommitted { text: "l".into() },
            ),
            make_event(
                1150,
                WalkthroughEventKind::TextCommitted { text: "l".into() },
            ),
            make_event(
                1200,
                WalkthroughEventKind::TextCommitted { text: "o".into() },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0].kind,
            WalkthroughActionKind::TypeText { text } if text == "hello"
        ));
    }

    #[test]
    fn test_text_broken_by_click() {
        use crate::walkthrough::synthesis::normalize_events;

        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::TextCommitted { text: "ab".into() },
            ),
            make_event(
                2000,
                WalkthroughEventKind::MouseClicked {
                    x: 100.0,
                    y: 200.0,
                    button: MouseButton::Left,
                    click_count: 1,
                    modifiers: vec![],
                },
            ),
            make_event(
                3000,
                WalkthroughEventKind::TextCommitted { text: "cd".into() },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 3); // TypeText, Click, TypeText
    }

    #[test]
    fn test_text_broken_by_idle_gap() {
        use crate::walkthrough::synthesis::normalize_events;

        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::TextCommitted { text: "ab".into() },
            ),
            make_event(
                4000,
                WalkthroughEventKind::TextCommitted { text: "cd".into() },
            ), // >2s gap
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 2);
    }

    #[test]
    fn test_rapid_scrolls_coalesced() {
        use crate::walkthrough::synthesis::normalize_events;

        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::Scrolled {
                    delta_y: -2.0,
                    x: Some(100.0),
                    y: Some(200.0),
                },
            ),
            make_event(
                1050,
                WalkthroughEventKind::Scrolled {
                    delta_y: -3.0,
                    x: Some(100.0),
                    y: Some(200.0),
                },
            ),
            make_event(
                1100,
                WalkthroughEventKind::Scrolled {
                    delta_y: -1.0,
                    x: Some(100.0),
                    y: Some(200.0),
                },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0].kind,
            WalkthroughActionKind::Scroll { delta_y } if *delta_y == -6.0
        ));
    }

    #[test]
    fn test_consecutive_identical_presskey_coalesced() {
        use crate::walkthrough::synthesis::normalize_events;

        let events: Vec<_> = (0..6)
            .map(|i| {
                make_event(
                    1000 + i * 100,
                    WalkthroughEventKind::KeyPressed {
                        key: "tab".into(),
                        modifiers: vec!["command".into()],
                    },
                )
            })
            .collect();
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 1, "6 rapid cmd+tab should coalesce to 1");
        assert!(matches!(
            &actions[0].kind,
            WalkthroughActionKind::PressKey { key, modifiers }
            if key == "tab" && modifiers == &["command"]
        ));
        assert_eq!(actions[0].source_event_ids.len(), 6);
    }

    #[test]
    fn test_different_presskey_not_coalesced() {
        use crate::walkthrough::synthesis::normalize_events;

        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::KeyPressed {
                    key: "tab".into(),
                    modifiers: vec!["command".into()],
                },
            ),
            make_event(
                1100,
                WalkthroughEventKind::KeyPressed {
                    key: "a".into(),
                    modifiers: vec!["command".into()],
                },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 2, "different keys should not coalesce");
    }

    #[test]
    fn test_presskey_coalescing_respects_time_gap() {
        use crate::walkthrough::synthesis::normalize_events;

        let events = vec![
            make_event(
                1000,
                WalkthroughEventKind::KeyPressed {
                    key: "tab".into(),
                    modifiers: vec!["command".into()],
                },
            ),
            make_event(
                2000,
                WalkthroughEventKind::KeyPressed {
                    key: "tab".into(),
                    modifiers: vec!["command".into()],
                },
            ),
        ];
        let (actions, _) = normalize_events(&events);
        assert_eq!(actions.len(), 2, "gap >500ms should not coalesce");
    }
}
