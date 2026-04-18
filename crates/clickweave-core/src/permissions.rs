//! Cross-crate permission metadata.
//!
//! Lists tools that require user confirmation because they have visible
//! side effects (launching applications, taking control of a window, etc.).
//! Owned by `clickweave-core` so the engine and UI can share the same catalog
//! without pulling in the LLM crate.

/// Tools that require user confirmation due to side effects.
/// Each entry is `(tool_name, human-readable description)`.
pub const CONFIRMABLE_TOOLS: &[(&str, &str)] = &[
    ("quit_app", "Closes a running application"),
    ("launch_app", "Opens an application"),
    (
        "cdp_connect",
        "Connects to app via Chrome DevTools Protocol",
    ),
];
