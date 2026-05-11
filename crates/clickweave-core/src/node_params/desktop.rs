use super::*;

// --- Parameter structs ---

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AiStepParams {
    pub prompt: String,
    pub button_text: Option<String>,
    pub template_image: Option<String>,
    pub max_tool_calls: Option<u32>,
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TakeScreenshotParams {
    pub mode: ScreenshotMode,
    pub target: Option<String>,
    pub include_ocr: bool,
}

impl Default for TakeScreenshotParams {
    fn default() -> Self {
        Self {
            mode: ScreenshotMode::Screen,
            target: None,
            include_ocr: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ScreenshotMode {
    Screen,
    Window,
    Region,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct FindTextParams {
    #[serde(default)]
    pub search_text: String,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct FindImageParams {
    pub template_image: Option<String>,
    pub threshold: f64,
    pub max_results: u32,
}

impl Default for FindImageParams {
    fn default() -> Self {
        Self {
            template_image: None,
            threshold: 0.88,
            max_results: 3,
        }
    }
}

/// macOS window control button (traffic light) actions.
///
/// At execution time these are resolved to window-relative clicks by
/// querying the focused window's position and applying a fixed pixel offset.
/// This is more reliable than keyboard shortcuts — e.g. Cmd+W closes a tab
/// in tabbed apps, not the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum WindowControlAction {
    Close,
    Minimize,
    Maximize,
    /// Green button in "zoom" mode — resizes/maximizes the window without
    /// entering full screen. Same physical button as Maximize but different
    /// macOS subrole (`AXZoomButton` vs `AXFullScreenButton`).
    Zoom,
}

/// Standard macOS traffic light button Y offset (center of button from window top).
/// Based on the default NSWindow title bar. Custom title bars (e.g. Electron apps
/// with `titleBarStyle: 'hidden'`) may use different positions.
const TRAFFIC_LIGHT_Y: f64 = 14.0;
/// X offsets for close / minimize / maximize button centers from window left edge.
const TRAFFIC_LIGHT_CLOSE_X: f64 = 14.0;
const TRAFFIC_LIGHT_MINIMIZE_X: f64 = 34.0;
const TRAFFIC_LIGHT_MAXIMIZE_X: f64 = 54.0;

impl WindowControlAction {
    /// Pixel offset from the window's top-left corner to the button center.
    ///
    /// These are standard macOS traffic light positions for the default `NSWindow`
    /// title bar. They are stable across macOS versions (10.x through 15.x) but
    /// may be incorrect for apps with custom title bars or hidden traffic lights.
    pub fn window_offset(self) -> (f64, f64) {
        match self {
            Self::Close => (TRAFFIC_LIGHT_CLOSE_X, TRAFFIC_LIGHT_Y),
            Self::Minimize => (TRAFFIC_LIGHT_MINIMIZE_X, TRAFFIC_LIGHT_Y),
            Self::Maximize | Self::Zoom => (TRAFFIC_LIGHT_MAXIMIZE_X, TRAFFIC_LIGHT_Y),
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Close => "Close window",
            Self::Minimize => "Minimize window",
            Self::Maximize => "Maximize window",
            Self::Zoom => "Zoom window",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum ClickTarget {
    Text { text: String },
    Coordinates { x: f64, y: f64 },
    WindowControl { action: WindowControlAction },
}

impl ClickTarget {
    pub fn text(&self) -> &str {
        match self {
            Self::Text { text } => text,
            Self::Coordinates { .. } => "",
            Self::WindowControl { action } => action.display_name(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ClickParams {
    #[serde(default)]
    pub target: Option<ClickTarget>,
    pub button: MouseButton,
    pub click_count: u32,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(
    ClickParams {
        target: Option<ClickTarget> = None,
        button: MouseButton = MouseButton::Left,
        click_count: u32 = 1,
    }
);

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct HoverParams {
    #[serde(default)]
    pub target: Option<ClickTarget>,
    pub dwell_ms: u64,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl Default for HoverParams {
    fn default() -> Self {
        Self {
            target: None,
            dwell_ms: 500,
            verification: VerificationConfig::default(),
        }
    }
}

impl_verification_deser!(
    HoverParams {
        target: Option<ClickTarget> = None,
        dwell_ms: u64 = 500,
    }
);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Center,
}

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TypeTextParams {
    pub text: String,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(TypeTextParams {
    text: String = String::new(),
});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PressKeyParams {
    pub key: String,
    #[serde(default)]
    pub modifiers: Vec<String>,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(
    PressKeyParams {
        key: String = String::new(),
        modifiers: Vec<String> = Vec::new(),
    }
);

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ScrollParams {
    pub delta_y: i32,
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(
    ScrollParams {
        delta_y: i32 = 0,
        x: Option<f64> = None,
        y: Option<f64> = None,
    }
);

/// Typed target for `FocusWindowParams`.
///
/// Replaces the previous stringly-typed `method: FocusMethod` + `value: Option<String>`
/// pair. Serialized as a flattened tagged enum so the on-disk shape keeps the
/// `method` / `value` keys at the top level of `FocusWindowParams`.
///
/// Legacy workflow files that stored the window id or pid as a stringified
/// number still deserialize correctly via the custom `Deserialize` impl on
/// `FocusWindowParams`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "method", content = "value")]
pub enum FocusTarget {
    AppName(String),
    WindowId(u64),
    Pid(u32),
}

impl Default for FocusTarget {
    /// Unconfigured focus targets default to an empty app-name string. This
    /// keeps the UI editor — which enumerates `AppName | WindowId | Pid` —
    /// and the `focus_window` tool mapping (empty name produces `args: {}`)
    /// behavior-compatible with the legacy `FocusMethod::AppName + value: None`
    /// shape.
    fn default() -> Self {
        FocusTarget::AppName(String::new())
    }
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct FocusWindowParams {
    #[serde(flatten)]
    pub target: FocusTarget,
    pub bring_to_front: bool,
    #[serde(default)]
    pub app_kind: AppKind,
    #[serde(default)]
    pub chrome_profile_id: Option<String>,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl Default for FocusWindowParams {
    fn default() -> Self {
        Self {
            target: FocusTarget::default(),
            bring_to_front: true,
            app_kind: AppKind::Native,
            chrome_profile_id: None,
            verification: VerificationConfig::default(),
        }
    }
}

impl<'de> Deserialize<'de> for FocusWindowParams {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            // Legacy + new: method + value carry the target. `value` can be
            // either a string (AppName, or legacy stringified u64/u32) or a
            // number (new shape for WindowId/Pid).
            #[serde(default)]
            method: Option<String>,
            #[serde(default)]
            value: serde_json::Value,
            #[serde(default = "bring_to_front_default")]
            bring_to_front: bool,
            #[serde(default)]
            app_kind: AppKind,
            #[serde(default)]
            chrome_profile_id: Option<String>,
            #[serde(default)]
            verification_method: Option<VerificationMethod>,
            #[serde(default)]
            verification_assertion: Option<String>,
        }

        fn bring_to_front_default() -> bool {
            true
        }

        let raw = Raw::deserialize(deserializer)?;

        let target = parse_focus_target(raw.method.as_deref(), &raw.value)
            .map_err(serde::de::Error::custom)?;

        let verification = VerificationConfig {
            verification_method: raw.verification_method,
            verification_assertion: raw.verification_assertion,
        };

        Ok(FocusWindowParams {
            target,
            bring_to_front: raw.bring_to_front,
            app_kind: raw.app_kind,
            chrome_profile_id: raw.chrome_profile_id,
            verification,
        })
    }
}

/// Parse a `(method, value)` pair from the on-disk representation into a
/// `FocusTarget`. Accepts both the new typed numeric shape for `WindowId`/`Pid`
/// and the legacy stringified-number shape.
fn parse_focus_target(
    method: Option<&str>,
    value: &serde_json::Value,
) -> Result<FocusTarget, String> {
    // Legacy unconfigured nodes: no method, or method=AppName with null/empty
    // value. Map to the default (empty-string AppName) so the UI editor
    // receives `method: "AppName"` as it always has.
    let Some(method) = method else {
        return Ok(FocusTarget::default());
    };
    match method {
        "AppName" => match value {
            serde_json::Value::Null => Ok(FocusTarget::default()),
            serde_json::Value::String(s) if s.is_empty() => Ok(FocusTarget::default()),
            serde_json::Value::String(s) => Ok(FocusTarget::AppName(s.clone())),
            other => Err(format!(
                "FocusWindowParams: expected string 'value' for AppName, got {other}"
            )),
        },
        "WindowId" => coerce_u64(value)
            .map(FocusTarget::WindowId)
            .map_err(|msg| format!("FocusWindowParams.value (WindowId): {msg}")),
        "Pid" => coerce_u32(value)
            .map(FocusTarget::Pid)
            .map_err(|msg| format!("FocusWindowParams.value (Pid): {msg}")),
        // Legacy {"method":"None"} workflow files — still migrate to the
        // empty AppName default rather than a dedicated None variant.
        "None" => Ok(FocusTarget::default()),
        other => Err(format!("FocusWindowParams: unknown method '{other}'")),
    }
}

fn coerce_u64(value: &serde_json::Value) -> Result<u64, String> {
    match value {
        serde_json::Value::Null => Err("missing".into()),
        serde_json::Value::Number(n) => n.as_u64().ok_or_else(|| format!("not a u64: {n}")),
        serde_json::Value::String(s) => s
            .parse::<u64>()
            .map_err(|e| format!("not parseable as u64: {s} ({e})")),
        other => Err(format!("expected number or string, got {other}")),
    }
}

fn coerce_u32(value: &serde_json::Value) -> Result<u32, String> {
    coerce_u64(value)
        .and_then(|n| u32::try_from(n).map_err(|_| format!("value {n} does not fit in u32")))
}

impl HasVerification for FocusWindowParams {
    fn verification(&self) -> Option<&VerificationConfig> {
        if self.verification.is_empty() {
            None
        } else {
            Some(&self.verification)
        }
    }
}

// --- New native node params ---

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct DragParams {
    #[serde(default)]
    pub from_x: Option<f64>,
    #[serde(default)]
    pub from_y: Option<f64>,
    #[serde(default)]
    pub to_x: Option<f64>,
    #[serde(default)]
    pub to_y: Option<f64>,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(
    DragParams {
        from_x: Option<f64> = None,
        from_y: Option<f64> = None,
        to_x: Option<f64> = None,
        to_y: Option<f64> = None,
    }
);

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct LaunchAppParams {
    pub app_name: String,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(LaunchAppParams {
    app_name: String = String::new(),
});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct QuitAppParams {
    pub app_name: String,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(QuitAppParams {
    app_name: String = String::new(),
});

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct FindAppParams {
    pub search: String,
}
