use serde::{Deserialize, Serialize};

/// Classification of an app's UI framework, used to decide whether
/// Chrome DevTools Protocol (CDP) tools can provide better automation.
///
/// - `Native`: standard native app — use accessibility-based automation
/// - `ChromeBrowser`: Chrome-family browser — CDP gives DOM access
/// - `ElectronApp`: Electron-based app — native AX is unreliable, CDP preferred
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum AppKind {
    #[default]
    Native,
    ChromeBrowser,
    ElectronApp,
}

impl AppKind {
    /// Parse from a string value (e.g. from JSON tool arguments).
    /// Returns `None` for unrecognized values.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Native" => Some(Self::Native),
            "ChromeBrowser" => Some(Self::ChromeBrowser),
            "ElectronApp" => Some(Self::ElectronApp),
            _ => None,
        }
    }

    /// Whether this app kind uses Chrome DevTools Protocol for automation.
    pub fn uses_cdp(self) -> bool {
        matches!(self, Self::ChromeBrowser | Self::ElectronApp)
    }
}
