use super::*;

pub(crate) fn get_recording_bar_rect(app: &tauri::AppHandle) -> Option<(f64, f64, f64, f64)> {
    let win = app.get_webview_window(RECORDING_BAR_LABEL)?;
    let scale = win.scale_factor().ok()?;
    let pos = win.outer_position().ok()?;
    let size = win.outer_size().ok()?;
    Some((
        pos.x as f64 / scale,
        pos.y as f64 / scale,
        size.width as f64 / scale,
        size.height as f64 / scale,
    ))
}

/// Strip the last click event if it lands inside the recording bar window.
///
/// Delegates to `session_lib::strip_recording_bar_click` in the library crate.
pub(crate) fn strip_recording_bar_click(
    events: &mut Vec<WalkthroughEvent>,
    bar_rect: (f64, f64, f64, f64),
) {
    session_lib::strip_recording_bar_click(events, bar_rect);
}

pub(super) fn persist_and_emit(
    app: &tauri::AppHandle,
    storage: &WalkthroughStorage,
    session_dir: &std::path::Path,
    event: &WalkthroughEvent,
) {
    let _ = storage.append_event(session_dir, event);
    emit_event(app, event);
}

fn emit_event(app: &tauri::AppHandle, event: &WalkthroughEvent) {
    let _ = app.emit(
        "walkthrough://event",
        crate::commands::types::WalkthroughEventPayload {
            event: event.clone(),
        },
    );
}

// ---------------------------------------------------------------------------
// CDP helpers
// ---------------------------------------------------------------------------
