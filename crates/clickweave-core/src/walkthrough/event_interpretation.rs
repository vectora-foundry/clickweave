use crate::WindowControlAction;

/// macOS window control button actions detected from accessibility data.
///
/// Traffic light buttons (close, minimize, zoom/full-screen) report
/// `role: AXButton` with an AXDescription label like "close button".
/// This enum is the single source of truth for the mapping between
/// accessibility labels, keyboard shortcuts, and display names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowControl {
    Close,
    Minimize,
    Maximize,
    Zoom,
}

impl WindowControl {
    /// Detect from accessibility label, role, and subrole.
    ///
    /// Subrole (`AXCloseButton`, `AXMinimizeButton`, `AXZoomButton`,
    /// `AXFullScreenButton`) is the most reliable signal — set by the
    /// macOS window server for all apps including Electron. Falls back
    /// to label matching for older MCP versions that don't return subrole.
    pub(crate) fn from_accessibility(
        label: &str,
        role: Option<&str>,
        subrole: Option<&str>,
    ) -> Option<Self> {
        // Subrole is definitive — works even for Electron apps with no label.
        if let Some(sr) = subrole {
            return match sr {
                "AXCloseButton" => Some(Self::Close),
                "AXMinimizeButton" => Some(Self::Minimize),
                "AXZoomButton" => Some(Self::Zoom),
                "AXFullScreenButton" => Some(Self::Maximize),
                _ => None,
            };
        }

        // Fallback: label matching (native apps with AXDescription).
        if role != Some("AXButton") {
            return None;
        }
        if label.eq_ignore_ascii_case("close button") {
            Some(Self::Close)
        } else if label.eq_ignore_ascii_case("minimize button") {
            Some(Self::Minimize)
        } else if label.eq_ignore_ascii_case("full screen button") {
            Some(Self::Maximize)
        } else if label.eq_ignore_ascii_case("zoom button") {
            Some(Self::Zoom)
        } else {
            None
        }
    }

    /// Reverse-detect from a PressKey's key + modifiers.
    /// Note: Close is NOT here — Cmd+W closes a tab, not the window.
    /// Close is only detected via accessibility (subrole/label).
    pub(crate) fn from_shortcut(key: &str, modifiers: &[String]) -> Option<Self> {
        // Sort modifiers so matching is order-independent — the order from
        // flags_to_modifiers() depends on flag-check order, not user intent.
        let mut sorted: Vec<&str> = modifiers.iter().map(|s| s.as_str()).collect();
        sorted.sort_unstable();
        match (key, sorted.as_slice()) {
            ("m", ["command"]) => Some(Self::Minimize),
            ("f", ["command", "control"]) => Some(Self::Maximize),
            _ => None,
        }
    }

    /// Keyboard shortcut key. Returns `None` for Close (Cmd+W closes tabs, not windows).
    pub(crate) fn shortcut(self) -> Option<(&'static str, Vec<String>)> {
        match self {
            Self::Minimize => Some(("m", vec!["command".into()])),
            Self::Maximize => Some(("f", vec!["command".into(), "control".into()])),
            Self::Close | Self::Zoom => None,
        }
    }

    pub(crate) fn display_name(self) -> &'static str {
        self.to_action().display_name()
    }

    /// Convert to the public `WindowControlAction` for use in target candidates
    /// and click targets.
    pub(crate) fn to_action(self) -> WindowControlAction {
        match self {
            Self::Close => WindowControlAction::Close,
            Self::Minimize => WindowControlAction::Minimize,
            Self::Maximize => WindowControlAction::Maximize,
            Self::Zoom => WindowControlAction::Zoom,
        }
    }
}

/// Human-readable names for well-known keyboard shortcuts that aren't
/// window control buttons (those are handled by `WindowControl::from_shortcut`).
pub(crate) fn shortcut_display_name(key: &str, modifiers: &[String]) -> Option<String> {
    let mut sorted: Vec<&str> = modifiers.iter().map(|s| s.as_str()).collect();
    sorted.sort_unstable();
    match (key, sorted.as_slice()) {
        ("w", ["command"]) => Some("Close tab".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_control_from_subrole() {
        assert_eq!(
            WindowControl::from_accessibility("", None, Some("AXCloseButton")),
            Some(WindowControl::Close)
        );
        assert_eq!(
            WindowControl::from_accessibility("", None, Some("AXMinimizeButton")),
            Some(WindowControl::Minimize)
        );
        assert_eq!(
            WindowControl::from_accessibility("", None, Some("AXZoomButton")),
            Some(WindowControl::Zoom)
        );
        assert_eq!(
            WindowControl::from_accessibility("", None, Some("AXFullScreenButton")),
            Some(WindowControl::Maximize)
        );
    }

    #[test]
    fn test_window_control_from_label_fallback() {
        // No subrole — falls back to label matching.
        assert_eq!(
            WindowControl::from_accessibility("close button", Some("AXButton"), None),
            Some(WindowControl::Close)
        );
        assert_eq!(
            WindowControl::from_accessibility("minimize button", Some("AXButton"), None),
            Some(WindowControl::Minimize)
        );
        assert_eq!(
            WindowControl::from_accessibility("full screen button", Some("AXButton"), None),
            Some(WindowControl::Maximize)
        );
        assert_eq!(
            WindowControl::from_accessibility("zoom button", Some("AXButton"), None),
            Some(WindowControl::Zoom)
        );
    }

    #[test]
    fn test_window_control_wrong_role_returns_none() {
        assert!(WindowControl::from_accessibility("close button", Some("AXGroup"), None).is_none());
        assert!(WindowControl::from_accessibility("close button", None, None).is_none());
    }

    #[test]
    fn test_window_control_unrelated_button_returns_none() {
        assert!(WindowControl::from_accessibility("Submit", Some("AXButton"), None).is_none());
    }

    #[test]
    fn test_window_control_unrelated_subrole_returns_none() {
        assert!(
            WindowControl::from_accessibility("", Some("AXButton"), Some("AXSortButton")).is_none()
        );
    }

    #[test]
    fn test_window_control_shortcut_roundtrip() {
        // Close has no shortcut — only Minimize and Maximize round-trip.
        assert!(WindowControl::Close.shortcut().is_none());
        for wc in [WindowControl::Minimize, WindowControl::Maximize] {
            let (key, modifiers) = wc.shortcut().unwrap();
            let recovered = WindowControl::from_shortcut(key, &modifiers);
            assert_eq!(recovered, Some(wc), "roundtrip failed for {wc:?}");
        }
    }

    #[test]
    fn test_window_control_shortcut_order_independent() {
        // control+command (reversed) should still match Maximize.
        let modifiers = vec!["control".to_string(), "command".to_string()];
        assert_eq!(
            WindowControl::from_shortcut("f", &modifiers),
            Some(WindowControl::Maximize)
        );
    }
}
