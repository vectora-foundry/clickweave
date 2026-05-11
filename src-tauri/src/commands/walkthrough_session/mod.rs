use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use clickweave_core::AppKind;
use clickweave_core::app_detection::{bundle_path_from_pid, classify_app, classify_app_by_pid};
use clickweave_core::cdp::{
    current_selected_page_url as pick_current_selected_url, parse_cdp_page_list,
    pick_page_index_for_url,
};
use clickweave_core::walkthrough::enrichment::parse_cdp_click_data;
use clickweave_core::walkthrough::session::{
    self as session_lib, CDP_CHECK_AND_REINJECT_JS, CDP_CLICK_LISTENER_JS, CDP_HOVER_LISTENER_JS,
    CDP_RETRIEVE_CLICK_JS, CDP_RETRIEVE_HOVERS_JS, CDP_STOP_HOVER_JS, CachedApp,
};
use clickweave_core::walkthrough::{
    ScreenshotKind, ScreenshotMeta, WalkthroughEvent, WalkthroughEventKind,
    WalkthroughSessionRuntime, WalkthroughStatus, WalkthroughStorage,
};
use clickweave_mcp::McpClient;
use tauri::{Emitter, Manager};
use uuid::Uuid;

use super::walkthrough::{
    CDP_SNAPSHOT_TIMEOUT, CdpAppConfig, CdpSetupProgress, CdpSetupStatus, RECORDING_BAR_LABEL,
    SELF_APP_NAME, VLM_CALL_TIMEOUT,
};
use super::walkthrough_enrichment::{
    enrich_click, execute_vlm_click_request, prepare_vlm_click_request,
};
use crate::platform::{CaptureCommand, CaptureEvent, CaptureEventKind};

// CDP JavaScript constants (CDP_CLICK_LISTENER_JS, CDP_RETRIEVE_CLICK_JS,
// CDP_CHECK_AND_REINJECT_JS, CDP_HOVER_LISTENER_JS, CDP_RETRIEVE_HOVERS_JS,
// CDP_STOP_HOVER_JS) are imported from clickweave_core::walkthrough::session.

use crate::platform::CursorRegionCapture;

#[cfg(target_os = "macos")]
use crate::platform::macos::MacOSEventTap;
#[cfg(target_os = "windows")]
use crate::platform::windows::WindowsEventHook;

#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::sync::RwLock;

/// Shared buffer holding the most recent cursor region capture (64×64pt around
/// the cursor, polled every 100ms). Used as the click crop template — always
/// reflects the screen before hover effects from the click itself.
///
/// Inner `Arc` avoids cloning the pixel data when reading on click — only an
/// `Arc` pointer bump instead of a 64 KB memcpy.
#[cfg(any(target_os = "macos", target_os = "windows"))]
type ScreenshotBuffer = Arc<RwLock<Option<Arc<CursorRegionCapture>>>>;

/// Per-app CDP session info captured during `setup_cdp_apps` and consumed by
/// the walkthrough event loop when reconnecting to retrieve click/hover data.
///
/// `selected_page_url` is the URL of the page on which the click/hover JS
/// listeners were injected. Future reconnects call `cdp_select_page` with the
/// matching index so retrieval always hits the right tab even when the
/// browser has multiple tabs open or added new ones during recording.
#[derive(Debug, Clone)]
struct CdpAppState {
    port: u16,
    selected_page_url: Option<String>,
}

struct CaptureServices {
    mcp: Option<Arc<McpClient>>,
    cdp_state: HashMap<String, CdpAppState>,
    vlm_backend: Option<Arc<clickweave_llm::LlmClient>>,
}

struct CdpClickRequest {
    port: u16,
    /// URL of the tab the click/hover listeners were injected into —
    /// restored after reconnect so retrieval hits the same page even if
    /// the browser has multiple tabs open.
    selected_page_url: Option<String>,
    click_event_id: Uuid,
    click_timestamp: u64,
}

#[derive(Default)]
struct CaptureFocusState {
    last_pid: i32,
    self_focused: bool,
}

struct ClickEnrichmentSummary {
    screenshot_path: Option<String>,
    screenshot_meta: Option<ScreenshotMeta>,
    ax_label_data: Option<(String, Option<String>)>,
    has_actionable_ax: bool,
}

enum CdpConnectOutcome {
    Ready,
    Cancelled,
    Failed(String),
}

// CachedApp is imported from clickweave_core::walkthrough::session.

mod capture;
mod cdp_click;
mod cdp_setup;
mod click_enrichment;
mod emit;
mod handle;
mod hover;
mod platform;
#[cfg(test)]
mod tests;

pub(super) use capture::process_capture_events;
pub(super) use emit::{get_recording_bar_rect, strip_recording_bar_click};
pub use handle::WalkthroughHandle;
pub(super) use platform::{populate_app_cache, spawn_mcp};

use cdp_click::cdp_retrieve_click;
use cdp_setup::{emit_cdp_progress, restore_selected_page, setup_cdp_apps};
use click_enrichment::enrich_click_background;
use emit::persist_and_emit;
use hover::{
    persist_cdp_hover_events, persist_native_hover_events, stop_recording_and_persist_frames,
};
use platform::resolve_app_name;
