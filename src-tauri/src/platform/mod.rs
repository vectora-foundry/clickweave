#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

pub use clickweave_core::MouseButton;

/// Raw capture event produced by the platform event tap.
///
/// Lightweight event sent from the OS event tap thread to the async processing
/// loop. The processing loop enriches these (via MCP accessibility/screenshot
/// calls) before wrapping them into `WalkthroughEvent` values.
#[derive(Debug, Clone)]
pub struct CaptureEvent {
    pub kind: CaptureEventKind,
    /// PID of the process that the event targets.
    pub target_pid: i32,
    /// Milliseconds since Unix epoch.
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub enum CaptureEventKind {
    MouseClick {
        x: f64,
        y: f64,
        button: MouseButton,
        click_count: u32,
        modifiers: Vec<String>,
    },
    KeyDown {
        /// Human-readable key name (e.g. "Return", "Tab", "a").
        key_name: String,
        /// Unicode characters produced by the key event, if any.
        characters: Option<String>,
        modifiers: Vec<String>,
    },
    ScrollWheel {
        delta_y: f64,
        x: f64,
        y: f64,
    },
}

/// Commands sent from the async processing loop to the event tap thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureCommand {
    Pause,
    Resume,
    Stop,
}

/// Half-size of the cursor region capture in screen points.
/// 32pt -> 64pt total region around the cursor (128px on Retina).
pub const CURSOR_REGION_HALF_PT: f64 = 32.0;

/// A small screen region captured around the cursor position.
///
/// Stores raw RGBA pixels. The captured region IS the click crop template —
/// no secondary crop step is needed.
#[derive(Clone)]
pub struct CursorRegionCapture {
    /// Raw RGBA pixel data (4 bytes per pixel, row-major, top-down).
    pub rgba_bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[cfg(target_os = "macos")]
pub use macos::{capture_cursor_region, get_cursor_position};
#[cfg(target_os = "windows")]
pub use windows::{capture_cursor_region, get_cursor_position};
