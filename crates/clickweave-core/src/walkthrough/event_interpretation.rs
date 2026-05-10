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

    /// Keyboard shortcut key. Returns `None` for Close (Cmd+W closes tabs, not windows).
    pub(crate) fn shortcut(self) -> Option<(&'static str, Vec<String>)> {
        match self {
            Self::Minimize => Some(("m", vec!["command".into()])),
            Self::Maximize => Some(("f", vec!["command".into(), "control".into()])),
            Self::Close | Self::Zoom => None,
        }
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

}
