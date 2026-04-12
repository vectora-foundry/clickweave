mod client;
mod image_prep;
mod types;

pub use client::*;
pub use image_prep::*;
pub use types::*;

/// Confirmable tool metadata for the permissions UI.
/// Tools that require user confirmation due to side effects.
pub const CONFIRMABLE_TOOLS: &[(&str, &str)] = &[
    ("quit_app", "Closes a running application"),
    ("launch_app", "Opens an application"),
    (
        "cdp_connect",
        "Connects to app via Chrome DevTools Protocol",
    ),
];
