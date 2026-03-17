use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use clickweave_core::AppKind;
use clickweave_core::app_detection::{bundle_path_from_pid, classify_app, classify_app_by_pid};
use clickweave_core::walkthrough::{
    ScreenshotKind, WalkthroughEvent, WalkthroughEventKind, WalkthroughSession, WalkthroughStatus,
    WalkthroughStorage,
};
use clickweave_mcp::McpRouter;
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

/// JavaScript click listener injected into CDP-enabled apps.
/// Captures the semantic target element on each click (capture phase,
/// fires before navigation/DOM mutation).
///
/// All state is stored on `document` (not `window`) because
/// chrome-devtools-mcp evaluates scripts in Puppeteer's utility world,
/// which has a separate `window` from the main world.  `document` is
/// shared across all JS execution contexts, so the listener, handler,
/// and click queue remain accessible regardless of which world runs the
/// injection or retrieval.
const CDP_CLICK_LISTENER_JS: &str = r#"() => {
  const d = document;
  d.__cw_clicks = [];
  const TAG_ROLES = {BUTTON:'button',A:'link',INPUT:'textbox',SELECT:'combobox',TEXTAREA:'textbox'};
  const INTERACTIVE = '[role="button"],[role="link"],[role="menuitem"],[role="menuitemcheckbox"],[role="menuitemradio"],[role="tab"],[role="treeitem"],[role="option"],[role="checkbox"],[role="radio"],[role="switch"],[role="textbox"],[role="combobox"],[role="searchbox"],[role="slider"],[role="spinbutton"],a,button,select,textarea,input,[tabindex]:not([tabindex="-1"])';
  function accessibleText(node) {
    const a = node.ariaLabel || node.getAttribute('aria-label');
    if (a) return a;
    const lb = node.getAttribute('aria-labelledby');
    if (lb) {
      const t = lb.split(/\s+/).map(id => document.getElementById(id)?.textContent?.trim() || '').filter(Boolean).join(' ');
      if (t) return t.substring(0, 200);
    }
    if (node.title) return node.title;
    if (node.alt) return node.alt;
    if (node.placeholder) return node.placeholder;
    if ((node.tagName === 'svg' || node.tagName === 'SVG') || (node.namespaceURI === 'http://www.w3.org/2000/svg' && node.tagName === 'svg')) {
      const st = node.querySelector('title');
      if (st?.textContent) return st.textContent.trim().substring(0, 200);
    }
    let t = '';
    for (const ch of node.childNodes) {
      if (ch.nodeType === 3) { t += ch.textContent; continue; }
      if (ch.nodeType === 1 && ch.getAttribute('aria-hidden') !== 'true') {
        const sub = accessibleText(ch);
        if (sub && t) t += ' ';
        t += sub;
      }
    }
    return t.trim().substring(0, 200);
  }
  d.__cw_handler = (e) => {
    const el = e.target.closest(INTERACTIVE) || e.target.closest('[aria-label]') || e.target;
    let text = accessibleText(el);
    if (!text) {
      let p = el.parentElement;
      while (p && p !== d.documentElement) {
        const la = p.ariaLabel || p.getAttribute('aria-label');
        if (la) { text = la; break; }
        const lb = p.getAttribute('aria-labelledby');
        if (lb) {
          const r = lb.split(/\s+/).map(id => document.getElementById(id)?.textContent?.trim() || '').filter(Boolean).join(' ');
          if (r) { text = r; break; }
        }
        if (p.title) { text = p.title; break; }
        p = p.parentElement;
      }
    }
    let parentRole = null;
    let parentName = null;
    {
      let p = el.parentElement;
      while (p && p !== d.documentElement) {
        const r = p.getAttribute('role');
        const a = p.ariaLabel || p.getAttribute('aria-label');
        if (r || a) {
          parentRole = r || null;
          parentName = a || accessibleText(p).substring(0, 200) || null;
          break;
        }
        p = p.parentElement;
      }
    }
    d.__cw_clicks.push({
      ts: Date.now(),
      tagName: el.tagName,
      role: el.getAttribute('role') || TAG_ROLES[el.tagName] || null,
      ariaLabel: el.ariaLabel || el.getAttribute('aria-label') || null,
      textContent: text || null,
      title: el.title || el.closest('[title]')?.title || null,
      value: el.value || null,
      href: el.closest('a')?.href || null,
      id: el.id || null,
      className: el.className || null,
      parentRole: parentRole,
      parentName: parentName,
    });
  };
  if (d.__cw_listener) {
    d.removeEventListener('click', d.__cw_listener, true);
  }
  d.__cw_listener = (e) => d.__cw_handler(e);
  d.addEventListener('click', d.__cw_listener, true);
}"#;

/// JavaScript to retrieve and remove the oldest click from the queue.
const CDP_RETRIEVE_CLICK_JS: &str = r#"() => {
  if (!Array.isArray(document.__cw_clicks)) return null;
  return document.__cw_clicks.shift() || null;
}"#;

/// JavaScript to check if the click listener is still alive; re-inject if lost.
/// Returns `"reinjected"` if it was re-injected, `"alive"` otherwise.
const CDP_CHECK_AND_REINJECT_JS: &str = r#"() => {
  const d = document;
  if (d.__cw_listener) return 'alive';
  d.__cw_clicks = [];
  const TAG_ROLES = {BUTTON:'button',A:'link',INPUT:'textbox',SELECT:'combobox',TEXTAREA:'textbox'};
  const INTERACTIVE = '[role="button"],[role="link"],[role="menuitem"],[role="menuitemcheckbox"],[role="menuitemradio"],[role="tab"],[role="treeitem"],[role="option"],[role="checkbox"],[role="radio"],[role="switch"],[role="textbox"],[role="combobox"],[role="searchbox"],[role="slider"],[role="spinbutton"],a,button,select,textarea,input,[tabindex]:not([tabindex="-1"])';
  function accessibleText(node) {
    const a = node.ariaLabel || node.getAttribute('aria-label');
    if (a) return a;
    const lb = node.getAttribute('aria-labelledby');
    if (lb) {
      const t = lb.split(/\s+/).map(id => document.getElementById(id)?.textContent?.trim() || '').filter(Boolean).join(' ');
      if (t) return t.substring(0, 200);
    }
    if (node.title) return node.title;
    if (node.alt) return node.alt;
    if (node.placeholder) return node.placeholder;
    if ((node.tagName === 'svg' || node.tagName === 'SVG') || (node.namespaceURI === 'http://www.w3.org/2000/svg' && node.tagName === 'svg')) {
      const st = node.querySelector('title');
      if (st?.textContent) return st.textContent.trim().substring(0, 200);
    }
    let t = '';
    for (const ch of node.childNodes) {
      if (ch.nodeType === 3) { t += ch.textContent; continue; }
      if (ch.nodeType === 1 && ch.getAttribute('aria-hidden') !== 'true') {
        const sub = accessibleText(ch);
        if (sub && t) t += ' ';
        t += sub;
      }
    }
    return t.trim().substring(0, 200);
  }
  d.__cw_handler = (e) => {
    const el = e.target.closest(INTERACTIVE) || e.target.closest('[aria-label]') || e.target;
    let text = accessibleText(el);
    if (!text) {
      let p = el.parentElement;
      while (p && p !== d.documentElement) {
        const la = p.ariaLabel || p.getAttribute('aria-label');
        if (la) { text = la; break; }
        const lb = p.getAttribute('aria-labelledby');
        if (lb) {
          const r = lb.split(/\s+/).map(id => document.getElementById(id)?.textContent?.trim() || '').filter(Boolean).join(' ');
          if (r) { text = r; break; }
        }
        if (p.title) { text = p.title; break; }
        p = p.parentElement;
      }
    }
    let parentRole = null;
    let parentName = null;
    {
      let p = el.parentElement;
      while (p && p !== d.documentElement) {
        const r = p.getAttribute('role');
        const a = p.ariaLabel || p.getAttribute('aria-label');
        if (r || a) {
          parentRole = r || null;
          parentName = a || accessibleText(p).substring(0, 200) || null;
          break;
        }
        p = p.parentElement;
      }
    }
    d.__cw_clicks.push({
      ts: Date.now(),
      tagName: el.tagName,
      role: el.getAttribute('role') || TAG_ROLES[el.tagName] || null,
      ariaLabel: el.ariaLabel || el.getAttribute('aria-label') || null,
      textContent: text || null,
      title: el.title || el.closest('[title]')?.title || null,
      value: el.value || null,
      href: el.closest('a')?.href || null,
      id: el.id || null,
      className: el.className || null,
      parentRole: parentRole,
      parentName: parentName,
    });
  };
  d.__cw_listener = (e) => d.__cw_handler(e);
  d.addEventListener('click', d.__cw_listener, true);
  return 'reinjected';
}"#;

/// JavaScript hover listener injected into CDP-enabled apps.
/// Tracks which interactive element the cursor is over using a polling
/// approach: `mousemove` updates the last-known cursor position, and a
/// 100ms `setInterval` calls `elementFromPoint` to detect element
/// transitions with dwell timing.
///
/// Uses the same `accessibleText()`, `INTERACTIVE` selector, and parent
/// traversal logic as the click listener so hover and click results are
/// directly comparable.  Pushes to `document.__cw_hovers` only when the
/// resolved element changes, recording the dwell time on the previous
/// element.
const CDP_HOVER_LISTENER_JS: &str = r#"() => {
  const d = document;
  d.__cw_hovers = [];
  d.__cw_hover_cx = 0;
  d.__cw_hover_cy = 0;
  d.__cw_hover_enter_sx = 0;
  d.__cw_hover_enter_sy = 0;
  const TAG_ROLES = {BUTTON:'button',A:'link',INPUT:'textbox',SELECT:'combobox',TEXTAREA:'textbox'};
  const INTERACTIVE = '[role="button"],[role="link"],[role="menuitem"],[role="menuitemcheckbox"],[role="menuitemradio"],[role="tab"],[role="treeitem"],[role="option"],[role="checkbox"],[role="radio"],[role="switch"],[role="textbox"],[role="combobox"],[role="searchbox"],[role="slider"],[role="spinbutton"],a,button,select,textarea,input,[tabindex]:not([tabindex="-1"])';
  function accessibleText(node) {
    const a = node.ariaLabel || node.getAttribute('aria-label');
    if (a) return a;
    const lb = node.getAttribute('aria-labelledby');
    if (lb) {
      const t = lb.split(/\s+/).map(id => document.getElementById(id)?.textContent?.trim() || '').filter(Boolean).join(' ');
      if (t) return t.substring(0, 200);
    }
    if (node.title) return node.title;
    if (node.alt) return node.alt;
    if (node.placeholder) return node.placeholder;
    if ((node.tagName === 'svg' || node.tagName === 'SVG') || (node.namespaceURI === 'http://www.w3.org/2000/svg' && node.tagName === 'svg')) {
      const st = node.querySelector('title');
      if (st?.textContent) return st.textContent.trim().substring(0, 200);
    }
    let t = '';
    for (const ch of node.childNodes) {
      if (ch.nodeType === 3) { t += ch.textContent; continue; }
      if (ch.nodeType === 1 && ch.getAttribute('aria-hidden') !== 'true') {
        const sub = accessibleText(ch);
        if (sub && t) t += ' ';
        t += sub;
      }
    }
    return t.trim().substring(0, 200);
  }
  d.__cw_hover_lastEl = null;
  d.__cw_hover_enterTime = 0;
  const MIN_DWELL = __CW_MIN_DWELL__;
  if (d.__cw_hover_mousemove) {
    d.removeEventListener('mousemove', d.__cw_hover_mousemove, true);
  }
  d.__cw_hover_mousemove = (e) => {
    d.__cw_hover_cx = e.clientX;
    d.__cw_hover_cy = e.clientY;
  };
  d.addEventListener('mousemove', d.__cw_hover_mousemove, true);
  d.__cw_hover_flush = () => {
    const el = d.__cw_hover_lastEl;
    const enter = d.__cw_hover_enterTime;
    if (!el || !enter) return;
    const now = Date.now();
    if ((now - enter) < MIN_DWELL) return;
    let text = accessibleText(el);
    if (!text) {
      let p = el.parentElement;
      while (p && p !== d.documentElement) {
        const la = p.ariaLabel || p.getAttribute('aria-label');
        if (la) { text = la; break; }
        const lb = p.getAttribute('aria-labelledby');
        if (lb) {
          const r = lb.split(/\s+/).map(id => document.getElementById(id)?.textContent?.trim() || '').filter(Boolean).join(' ');
          if (r) { text = r; break; }
        }
        if (p.title) { text = p.title; break; }
        p = p.parentElement;
      }
    }
    let parentRole = null;
    let parentName = null;
    {
      let p = el.parentElement;
      while (p && p !== d.documentElement) {
        const r = p.getAttribute('role');
        const a = p.ariaLabel || p.getAttribute('aria-label');
        if (r || a) {
          parentRole = r || null;
          parentName = a || accessibleText(p).substring(0, 200) || null;
          break;
        }
        p = p.parentElement;
      }
    }
    d.__cw_hovers.push({
      ts: enter,
      dwellMs: now - enter,
      x: d.__cw_hover_enter_sx,
      y: d.__cw_hover_enter_sy,
      tagName: el.tagName,
      role: el.getAttribute('role') || TAG_ROLES[el.tagName] || null,
      ariaLabel: el.ariaLabel || el.getAttribute('aria-label') || null,
      textContent: text || null,
      href: el.closest('a')?.href || null,
      parentRole: parentRole,
      parentName: parentName,
    });
    d.__cw_hover_lastEl = null;
    d.__cw_hover_enterTime = 0;
  };
  if (d.__cw_hover_interval) clearInterval(d.__cw_hover_interval);
  d.__cw_hover_interval = setInterval(() => {
    const raw = d.elementFromPoint(d.__cw_hover_cx, d.__cw_hover_cy);
    if (!raw) { d.__cw_hover_lastEl = null; d.__cw_hover_enterTime = 0; return; }
    const el = raw.closest(INTERACTIVE) || raw.closest('[aria-label]') || raw;
    if (el === d.__cw_hover_lastEl) return;
    d.__cw_hover_flush();
    d.__cw_hover_lastEl = el;
    d.__cw_hover_enterTime = Date.now();
    d.__cw_hover_enter_sx = d.__cw_hover_cx + window.screenX;
    d.__cw_hover_enter_sy = d.__cw_hover_cy + window.screenY;
  }, 100);
}"#;

/// JavaScript to retrieve and clear all collected hover data from the
/// injected hover listener.  Returns the full array and resets it.
const CDP_RETRIEVE_HOVERS_JS: &str = r#"() => {
  if (!Array.isArray(document.__cw_hovers)) return [];
  const h = document.__cw_hovers;
  document.__cw_hovers = [];
  return h;
}"#;

/// JavaScript to stop the hover listener's polling interval and remove
/// the mousemove handler, flushing any pending dwell that exceeds the
/// minimum threshold.
const CDP_STOP_HOVER_JS: &str = r#"() => {
  const d = document;
  if (d.__cw_hover_interval) { clearInterval(d.__cw_hover_interval); d.__cw_hover_interval = null; }
  if (d.__cw_hover_flush) { d.__cw_hover_flush(); d.__cw_hover_flush = null; }
  if (d.__cw_hover_mousemove) { d.removeEventListener('mousemove', d.__cw_hover_mousemove, true); d.__cw_hover_mousemove = null; }
}"#;

#[cfg(target_os = "macos")]
use crate::platform::macos::{CursorRegionCapture, MacOSEventTap};

#[cfg(target_os = "macos")]
use std::sync::RwLock;

/// Shared buffer holding the most recent cursor region capture (64×64pt around
/// the cursor, polled every 100ms). Used as the click crop template — always
/// reflects the screen before hover effects from the click itself.
///
/// Inner `Arc` avoids cloning the pixel data when reading on click — only an
/// `Arc` pointer bump instead of a 64 KB memcpy.
#[cfg(target_os = "macos")]
type ScreenshotBuffer = Arc<RwLock<Option<Arc<CursorRegionCapture>>>>;

/// Cached info about a running app, populated from MCP's `list_apps` response.
pub(super) struct CachedApp {
    pub(super) name: String,
    pub(super) bundle_id: Option<String>,
}

/// Manages the walkthrough recording lifecycle.
pub struct WalkthroughHandle {
    pub session: Option<WalkthroughSession>,
    pub session_dir: Option<std::path::PathBuf>,
    pub(super) storage: Option<WalkthroughStorage>,
    pub(super) mcp_command: Option<String>,
    #[cfg(target_os = "macos")]
    pub(super) event_tap: Option<MacOSEventTap>,
    pub(super) processing_task: Option<tauri::async_runtime::JoinHandle<()>>,
    /// Cancellation signal for the processing loop.
    pub(super) cancel_tx: tokio::sync::watch::Sender<bool>,
}

impl Default for WalkthroughHandle {
    fn default() -> Self {
        let (cancel_tx, _) = tokio::sync::watch::channel(false);
        Self {
            session: None,
            session_dir: None,
            storage: None,
            mcp_command: None,
            #[cfg(target_os = "macos")]
            event_tap: None,
            processing_task: None,
            cancel_tx,
        }
    }
}

impl WalkthroughHandle {
    pub(super) fn ensure_status(
        &self,
        expected: &[WalkthroughStatus],
    ) -> Result<&WalkthroughSession, super::error::CommandError> {
        let session = self
            .session
            .as_ref()
            .ok_or(super::error::CommandError::validation(
                "No walkthrough session is active",
            ))?;
        if !expected.contains(&session.status) {
            return Err(super::error::CommandError::validation(format!(
                "Walkthrough is in {:?} state, expected one of {:?}",
                session.status, expected
            )));
        }
        Ok(session)
    }

    /// Stop the capture backend and return the processing task handle.
    ///
    /// Signals the cancellation token so the processing loop exits promptly
    /// (any in-flight MCP call is dropped via `select!`). The caller should
    /// `await` the returned handle for a clean shutdown.
    pub(super) fn stop_capture(&mut self) -> Option<tauri::async_runtime::JoinHandle<()>> {
        let _ = self.cancel_tx.send(true);

        #[cfg(target_os = "macos")]
        if let Some(tap) = self.event_tap.take() {
            tap.send_command(CaptureCommand::Stop);
            // Drop the tap handle — this joins the thread and closes the sender.
            drop(tap);
        }
        self.processing_task.take()
    }
}

// ---------------------------------------------------------------------------
// Async event processing loop
// ---------------------------------------------------------------------------

/// Process captured events: enrich with MCP data, persist, and emit to frontend.
///
/// Click enrichment (screenshot + accessibility + VLM) runs in background tasks
/// so the event loop never blocks on MCP calls and captures every click.
#[allow(clippy::too_many_arguments)]
pub(super) async fn process_capture_events(
    app: tauri::AppHandle,
    mut event_rx: tokio::sync::mpsc::UnboundedReceiver<CaptureEvent>,
    mcp_command: String,
    planner: Option<super::types::EndpointConfig>,
    storage: WalkthroughStorage,
    session_dir: std::path::PathBuf,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    cdp_apps: Vec<CdpAppConfig>,
    hover_dwell_ms: u64,
) {
    // Spawn the MCP server for enrichment (screenshots + OCR).
    let mut mcp_raw = spawn_mcp(&mcp_command).await;

    // Set up CDP servers for selected apps before wrapping in Arc
    // (spawn_server requires &mut).
    let cdp_state: HashMap<String, String> = if !cdp_apps.is_empty() {
        if let Some(ref mut mcp) = mcp_raw {
            setup_cdp_apps(&cdp_apps, mcp, &app, &mut cancel, hover_dwell_ms).await
        } else {
            tracing::warn!("No MCP server available for CDP setup");
            for cdp_app in &cdp_apps {
                emit_cdp_progress(
                    &app,
                    &cdp_app.name,
                    CdpSetupStatus::Failed {
                        reason: "MCP server unavailable".to_string(),
                    },
                );
            }
            HashMap::new()
        }
    } else {
        HashMap::new()
    };

    // Signal frontend that CDP setup is complete so the modal can close.
    if !cdp_apps.is_empty() {
        emit_cdp_progress(&app, "", CdpSetupStatus::Done);
    }

    // Drain any events captured during CDP setup (app restarts generate
    // focus/input events that are not user-initiated). Drain even if all
    // setups failed — the quit/relaunch attempt still produces events.
    if !cdp_apps.is_empty() {
        while event_rx.try_recv().is_ok() {}
    }

    // Wrap in Arc so background enrichment tasks can share it.
    let mcp: Option<std::sync::Arc<McpRouter>> = mcp_raw.map(std::sync::Arc::new);

    // Initialize VLM backend if planner config is available.
    let vlm_backend: Option<std::sync::Arc<clickweave_llm::LlmClient>> =
        planner.filter(|p| !p.is_empty()).map(|p| {
            let config = p
                .into_llm_config(Some(0.0))
                .with_max_tokens(2048)
                .with_thinking(false);
            std::sync::Arc::new(clickweave_llm::LlmClient::new(config))
        });

    // Start hover tracking for the recording session (non-fatal if unavailable).
    if let Some(ref mcp) = mcp {
        let hover_args = serde_json::json!({
            "min_dwell_ms": 100,
            "poll_interval_ms": 100,
            "max_duration_ms": 600_000,
        });
        if let Err(e) = mcp
            .call_tool("start_hover_tracking", Some(hover_args))
            .await
        {
            tracing::warn!("Failed to start hover tracking: {e}");
        }
    }

    // Start continuous screen recording for hover screenshots (non-fatal).
    if let Some(ref mcp) = mcp {
        let artifacts_dir = session_dir.join("artifacts");
        let recording_args = serde_json::json!({
            "output_dir": artifacts_dir.to_string_lossy(),
            "max_duration_ms": 600_000,
        });
        match mcp.call_tool("start_recording", Some(recording_args)).await {
            Ok(r) if r.is_error != Some(true) => {
                tracing::info!("Continuous recording started")
            }
            Ok(r) => {
                let msg: String = r.content.iter().filter_map(|c| c.as_text()).collect();
                tracing::warn!("start_recording returned error (non-fatal): {msg}");
            }
            Err(e) => tracing::warn!("Failed to start recording (non-fatal): {e}"),
        }
    }

    // Background tasks for click enrichment and VLM resolution.
    // Each task persists and emits its own events; the event loop
    // only needs to drain completions to detect errors.
    let mut bg_tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

    // Sequential CDP click retrieval channel.  System clicks arrive in order
    // and the JS listener pushes in order, so we must consume shift() entries
    // sequentially — otherwise independent tasks race and steal each other's
    // entries.  A single consumer task drains this channel in FIFO order.
    struct CdpClickRequest {
        server_name: String,
        click_event_id: Uuid,
        click_timestamp: u64,
    }
    let (cdp_tx, cdp_rx) = tokio::sync::mpsc::unbounded_channel::<CdpClickRequest>();

    // Screenshot buffer: a small (64pt / 128px on Retina) region around the
    // cursor, captured every 100ms. Used as the crop source for clicks —
    // always reflects what the user sees before hover effects from the click.
    #[cfg(target_os = "macos")]
    let screenshot_buffer: ScreenshotBuffer = Arc::new(RwLock::new(None));

    // Spawn a background task that continuously captures the region under the
    // cursor. Aborted when the event loop exits.
    #[cfg(target_os = "macos")]
    let cursor_poll_handle = {
        let buf = screenshot_buffer.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                let buf2 = buf.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let (cx, cy) = crate::platform::macos::get_cursor_position();
                    if let Some(shot) = crate::platform::macos::capture_cursor_region(cx, cy)
                        && let Ok(mut guard) = buf2.write()
                    {
                        *guard = Some(Arc::new(shot));
                    }
                })
                .await;
            }
        })
    };

    // Cache PID → app info to avoid repeated lookups.
    let mut app_cache: HashMap<i32, CachedApp> = HashMap::new();
    let app_kind_cache: Arc<Mutex<HashMap<i32, AppKind>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut last_pid: i32 = 0;
    let mut self_focused = false;

    if let Some(ref mcp) = mcp {
        populate_app_cache(mcp, &mut app_cache).await;
    }

    // Spawn the sequential CDP click consumer.  It processes requests in FIFO
    // order so each shift() retrieves the entry that matches the system click.
    let cdp_consumer_handle = {
        let mcp_for_cdp = mcp.clone();
        let app_for_cdp = app.clone();
        let storage_for_cdp = storage.clone();
        let dir_for_cdp = session_dir.clone();
        tokio::spawn(async move {
            let mut rx = cdp_rx;
            while let Some(req) = rx.recv().await {
                if let Some(ref mcp) = mcp_for_cdp {
                    cdp_retrieve_click(
                        mcp,
                        &req.server_name,
                        &app_for_cdp,
                        &storage_for_cdp,
                        &dir_for_cdp,
                        req.click_event_id,
                        req.click_timestamp,
                    )
                    .await;
                }
            }
        })
    };

    'event_loop: loop {
        // Drain completed background tasks and wait for the next capture event.
        let capture = loop {
            tokio::select! {
                biased;
                _ = cancel.changed() => break 'event_loop,
                Some(result) = bg_tasks.join_next() => {
                    if let Err(e) = result {
                        tracing::warn!("Background enrichment task panicked: {e}");
                    }
                    continue;
                }
                msg = event_rx.recv() => match msg {
                    Some(c) => break c,
                    None => break 'event_loop,
                },
            }
        };
        // Detect app focus changes.
        if capture.target_pid != 0 && capture.target_pid != last_pid {
            let app_name = resolve_app_name(capture.target_pid, &mcp, &mut app_cache).await;

            // Skip events targeting our own app (recording bar clicks, etc.).
            // We track focus but don't emit the AppFocused event for ourselves.
            if app_name == SELF_APP_NAME {
                last_pid = capture.target_pid;
                self_focused = true;
                continue;
            }

            // Classify the app's UI framework (Chrome, Electron, or Native).
            let app_kind = {
                let mut cache = app_kind_cache.lock().unwrap();
                if let Some(&cached_kind) = cache.get(&capture.target_pid) {
                    cached_kind
                } else {
                    let bundle_id = app_cache
                        .get(&capture.target_pid)
                        .and_then(|c| c.bundle_id.as_deref());
                    let bundle_path = bundle_path_from_pid(capture.target_pid);
                    let kind = classify_app(bundle_id, bundle_path.as_deref());
                    if kind != AppKind::Native {
                        tracing::info!(
                            "App '{}' (PID {}) classified as {:?}",
                            app_name,
                            capture.target_pid,
                            kind,
                        );
                    }
                    cache.insert(capture.target_pid, kind);
                    kind
                }
            };

            let focus_event = WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: capture.timestamp,
                kind: WalkthroughEventKind::AppFocused {
                    app_name: app_name.clone(),
                    pid: capture.target_pid,
                    window_title: None,
                    app_kind,
                },
            };
            persist_and_emit(&app, &storage, &session_dir, &focus_event);
            last_pid = capture.target_pid;
            self_focused = false;
        }

        // Skip events while our own app is focused.
        if self_focused {
            continue;
        }

        // Translate the capture event into a walkthrough event.
        let wt_event = match capture.kind {
            CaptureEventKind::MouseClick {
                x,
                y,
                button,
                click_count,
                modifiers,
            } => {
                let click_event = WalkthroughEvent {
                    id: Uuid::new_v4(),
                    timestamp: capture.timestamp,
                    kind: WalkthroughEventKind::MouseClicked {
                        x,
                        y,
                        button,
                        click_count,
                        modifiers,
                    },
                };

                // Persist the click event immediately so it's never lost.
                persist_and_emit(&app, &storage, &session_dir, &click_event);

                // Spawn enrichment (screenshot + accessibility + VLM) as a
                // background task so the event loop stays responsive.
                // Only spawn enrichment if MCP is available.
                if let Some(ref mcp_arc) = mcp {
                    let task_app_name = app_cache.get(&capture.target_pid).map(|c| c.name.clone());

                    // Queue CDP click retrieval (processed sequentially to
                    // preserve FIFO ordering with the JS click listener).
                    if let Some(server_name) = task_app_name
                        .as_deref()
                        .and_then(|name| cdp_state.get(name))
                    {
                        let _ = cdp_tx.send(CdpClickRequest {
                            server_name: server_name.clone(),
                            click_event_id: click_event.id,
                            click_timestamp: capture.timestamp,
                        });
                    }

                    let task_mcp = mcp_arc.clone();
                    let task_vlm = vlm_backend.clone();
                    let task_app = app.clone();
                    let task_storage = storage.clone();
                    let task_dir = session_dir.clone();
                    let ts = capture.timestamp;
                    let task_kind_cache = app_kind_cache.clone();
                    let task_pid = capture.target_pid;
                    #[cfg(target_os = "macos")]
                    let task_prehover = screenshot_buffer.read().ok().and_then(|g| g.clone());

                    bg_tasks.spawn(async move {
                        enrich_click_background(
                            task_mcp,
                            task_vlm,
                            task_app,
                            task_storage,
                            task_dir,
                            task_app_name,
                            x,
                            y,
                            ts,
                            VLM_CALL_TIMEOUT,
                            task_kind_cache,
                            task_pid,
                            #[cfg(target_os = "macos")]
                            task_prehover,
                        )
                        .await;
                    });
                }

                continue;
            }

            CaptureEventKind::KeyDown {
                key_name,
                characters,
                modifiers,
            } => {
                // If the key produces printable text and has no command/control
                // modifiers, emit TextCommitted instead of KeyPressed.
                let has_command_modifiers =
                    modifiers.iter().any(|m| m == "command" || m == "control");
                let is_printable = !has_command_modifiers
                    && characters
                        .as_ref()
                        .is_some_and(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()));

                let kind = if is_printable {
                    WalkthroughEventKind::TextCommitted {
                        text: characters.unwrap(),
                    }
                } else {
                    WalkthroughEventKind::KeyPressed {
                        key: key_name,
                        modifiers,
                    }
                };

                WalkthroughEvent {
                    id: Uuid::new_v4(),
                    timestamp: capture.timestamp,
                    kind,
                }
            }

            CaptureEventKind::ScrollWheel { delta_y, x, y } => WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp: capture.timestamp,
                kind: WalkthroughEventKind::Scrolled {
                    delta_y,
                    x: Some(x),
                    y: Some(y),
                },
            },
        };

        persist_and_emit(&app, &storage, &session_dir, &wt_event);
    }

    // Drop the CDP sender so the sequential consumer finishes after
    // processing any remaining queued requests.
    drop(cdp_tx);

    // Await in-flight enrichment tasks and the CDP consumer so their events
    // are on disk before stop_walkthrough reads them.  Bounded by a total
    // drain timeout so a wedged MCP server can't block shutdown indefinitely.
    let drain_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);

    // Wait for the CDP consumer first (it has its own sequential pipeline).
    if tokio::time::timeout_at(drain_deadline, cdp_consumer_handle)
        .await
        .is_err()
    {
        tracing::warn!("CDP consumer drain timeout reached");
    }

    loop {
        match tokio::time::timeout_at(drain_deadline, bg_tasks.join_next()).await {
            Ok(Some(Ok(()))) => {} // task completed successfully
            Ok(Some(Err(e))) => tracing::warn!("Enrichment task panicked: {e}"),
            Ok(None) => break, // all tasks finished
            Err(_) => {
                let remaining = bg_tasks.len();
                tracing::warn!("Drain timeout reached, aborting {remaining} enrichment task(s)");
                bg_tasks.abort_all();
                break;
            }
        }
    }

    // Stop the cursor region polling task.
    #[cfg(target_os = "macos")]
    cursor_poll_handle.abort();

    // Stop continuous recording and persist the frame list so
    // stop_walkthrough can attach recording frames to hover actions.
    if let Some(ref mcp) = mcp {
        let recording_timeout = tokio::time::Duration::from_secs(10);
        match tokio::time::timeout(recording_timeout, mcp.call_tool("stop_recording", None)).await {
            Ok(Ok(result)) if result.is_error != Some(true) => {
                let frames = super::walkthrough_enrichment::parse_recording_frames(&result.content);
                tracing::info!("Recording stopped, got {} frames", frames.len());
                let frames_path = session_dir.join("recording_frames.json");
                match serde_json::to_string_pretty(&frames) {
                    Ok(json) => {
                        if let Err(e) = std::fs::write(&frames_path, json) {
                            tracing::warn!("Failed to write recording frames: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to serialize recording frames: {e}");
                    }
                }
            }
            Ok(Ok(_)) => {
                tracing::debug!("stop_recording returned error (may not have been active)");
            }
            Ok(Err(e)) => {
                tracing::debug!("stop_recording call failed: {e}");
            }
            Err(_) => {
                tracing::warn!("stop_recording timed out after {recording_timeout:?}");
            }
        }
    }

    // Retrieve hover events from MCP and persist them as HoverDetected
    // walkthrough events so that stop_walkthrough's retrieve_hover_candidates
    // can find them when scanning the event log.
    if let Some(ref mcp) = mcp {
        let hover_timeout = tokio::time::Duration::from_secs(5);
        match tokio::time::timeout(hover_timeout, mcp.call_tool("stop_hover_tracking", None)).await
        {
            Ok(Ok(result)) if result.is_error != Some(true) => {
                let raw_text: String = result.content.iter().filter_map(|c| c.as_text()).collect();
                match serde_json::from_str::<Vec<serde_json::Value>>(&raw_text) {
                    Ok(events) => {
                        let mut count = 0u32;

                        for ev in events {
                            // Skip timeout sentinel events.
                            if ev.get("timeout").and_then(|v| v.as_bool()) == Some(true) {
                                continue;
                            }
                            let x = ev
                                .pointer("/cursor/x")
                                .and_then(|v| v.as_f64())
                                .unwrap_or(0.0);
                            let y = ev
                                .pointer("/cursor/y")
                                .and_then(|v| v.as_f64())
                                .unwrap_or(0.0);
                            let element_name = ev
                                .pointer("/element/name")
                                .and_then(|v| v.as_str())
                                .or_else(|| ev.pointer("/element/label").and_then(|v| v.as_str()))
                                .unwrap_or("")
                                .to_string();
                            let element_role = ev
                                .pointer("/element/role")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            let dwell_ms = ev.get("dwell_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                            let timestamp_ms =
                                ev.get("timestamp_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                            let app_name = ev
                                .pointer("/element/app_name")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());

                            let hover_event = WalkthroughEvent {
                                id: Uuid::new_v4(),
                                timestamp: timestamp_ms,
                                kind: WalkthroughEventKind::HoverDetected {
                                    x,
                                    y,
                                    element_name,
                                    element_role,
                                    dwell_ms,
                                    app_name,
                                },
                            };
                            persist_and_emit(&app, &storage, &session_dir, &hover_event);
                            count += 1;
                        }
                        if count > 0 {
                            tracing::info!("Persisted {count} hover events from native tracking");
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse hover tracking response: {e}");
                    }
                }
            }
            Ok(Ok(_)) => {
                tracing::debug!("stop_hover_tracking returned error (may not have been active)");
            }
            Ok(Err(e)) => {
                tracing::debug!("stop_hover_tracking call failed: {e}");
            }
            Err(_) => {
                tracing::warn!("stop_hover_tracking timed out after {hover_timeout:?}");
            }
        }
    }

    // Retrieve CDP hover data from each CDP-enabled app. The JS hover
    // listener has been tracking element transitions in real-time; we now
    // stop it, flush any pending dwell, and retrieve the collected entries.
    if let Some(ref mcp) = mcp {
        for (app_name, server_name) in &cdp_state {
            // Stop the hover interval + flush pending dwell.
            let stop_args = serde_json::json!({ "function": CDP_STOP_HOVER_JS });
            let _ = mcp
                .call_tool_on(server_name, "evaluate_script", Some(stop_args))
                .await;

            // Retrieve all collected hover entries.
            let retrieve_args = serde_json::json!({ "function": CDP_RETRIEVE_HOVERS_JS });
            let result = match tokio::time::timeout(
                CDP_SNAPSHOT_TIMEOUT,
                mcp.call_tool_on(server_name, "evaluate_script", Some(retrieve_args)),
            )
            .await
            {
                Ok(Ok(r)) if r.is_error != Some(true) => r,
                Ok(Ok(_)) | Ok(Err(_)) | Err(_) => {
                    tracing::debug!("CDP hover retrieve failed for '{app_name}'");
                    continue;
                }
            };

            let raw: String = result.content.iter().filter_map(|c| c.as_text()).collect();
            let text = extract_eval_result(&raw);
            let entries: Vec<serde_json::Value> = match serde_json::from_str(text) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let mut count = 0u32;
            for entry in entries {
                let label = entry["textContent"]
                    .as_str()
                    .or_else(|| entry["ariaLabel"].as_str())
                    .filter(|s| !s.is_empty());
                let Some(label) = label else { continue };

                let ts = entry["ts"].as_u64().unwrap_or(0);
                let dwell_ms = entry["dwellMs"].as_u64().unwrap_or(0);

                // Emit a HoverDetected event (so retrieve_hover_candidates finds it).
                let hover_id = Uuid::new_v4();
                let hover_event = WalkthroughEvent {
                    id: hover_id,
                    timestamp: ts,
                    kind: WalkthroughEventKind::HoverDetected {
                        x: entry["x"].as_f64().unwrap_or(0.0),
                        y: entry["y"].as_f64().unwrap_or(0.0),
                        element_name: label.to_string(),
                        element_role: entry["role"].as_str().map(|s| s.to_string()),
                        dwell_ms,
                        app_name: Some(app_name.clone()),
                    },
                };
                persist_and_emit(&app, &storage, &session_dir, &hover_event);

                // Emit a paired CdpHoverResolved event with the full DOM info.
                let cdp_event = WalkthroughEvent {
                    id: Uuid::new_v4(),
                    timestamp: ts,
                    kind: WalkthroughEventKind::CdpHoverResolved {
                        hover_event_id: hover_id,
                        name: label.to_string(),
                        role: entry["role"].as_str().map(|s| s.to_string()),
                        href: entry["href"].as_str().map(|s| s.to_string()),
                        parent_role: entry["parentRole"].as_str().map(|s| s.to_string()),
                        parent_name: entry["parentName"].as_str().map(|s| s.to_string()),
                    },
                };
                persist_and_emit(&app, &storage, &session_dir, &cdp_event);
                count += 1;
            }
            if count > 0 {
                tracing::info!("Persisted {count} CDP hover events from '{app_name}'");
            }
        }
    }

    tracing::info!("Walkthrough capture event loop ended");
}

/// Get the recording bar window's bounds in logical screen coordinates.
///
/// Returns `(x, y, width, height)` if the window exists, or `None` if it has
/// already been closed.
pub(super) fn get_recording_bar_rect(app: &tauri::AppHandle) -> Option<(f64, f64, f64, f64)> {
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
/// When the user clicks Stop, the event tap captures that click before shutting
/// down. This function removes that click and any events sharing its timestamp
/// (enrichment data for the stop-button click), preserving all other events
/// (e.g. VLM results for earlier clicks that were appended later).
pub(super) fn strip_recording_bar_click(
    events: &mut Vec<WalkthroughEvent>,
    bar_rect: (f64, f64, f64, f64),
) {
    let (bar_x, bar_y, bar_w, bar_h) = bar_rect;

    let last_click_pos = events
        .iter()
        .rposition(|e| matches!(&e.kind, WalkthroughEventKind::MouseClicked { .. }));

    if let Some(idx) = last_click_pos
        && let WalkthroughEventKind::MouseClicked { x, y, .. } = &events[idx].kind
        && *x >= bar_x
        && *x <= bar_x + bar_w
        && *y >= bar_y
        && *y <= bar_y + bar_h
    {
        let click_ts = events[idx].timestamp;
        events.retain(|e| e.timestamp != click_ts);
    }
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
        super::types::WalkthroughEventPayload {
            event: event.clone(),
        },
    );
}

// ---------------------------------------------------------------------------
// CDP helpers
// ---------------------------------------------------------------------------

/// Check if an app is already running with `--remote-debugging-port=<N>`.
/// Returns the port if found, so we can skip the quit/relaunch cycle.
async fn existing_debug_port(app_name: &str) -> Option<u16> {
    let output = tokio::process::Command::new("pgrep")
        .args(["-x", app_name])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let pids = String::from_utf8_lossy(&output.stdout);
    for pid_str in pids.split_whitespace() {
        let pid: u32 = pid_str.parse().ok()?;
        let args_output = tokio::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "args="])
            .output()
            .await
            .ok()?;
        let args = String::from_utf8_lossy(&args_output.stdout);
        if let Some(flag) = args
            .split_whitespace()
            .find(|a| a.starts_with("--remote-debugging-port="))
            && let Some(port_str) = flag.strip_prefix("--remote-debugging-port=")
            && let Ok(port) = port_str.parse::<u16>()
        {
            return Some(port);
        }
    }
    None
}

/// Pick a random port in the ephemeral range (49152–65535).
pub(super) fn rand_ephemeral_port() -> u16 {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let raw = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    let range = 65535 - 49152;
    49152 + (raw % range) as u16
}

/// Build the McpServerConfig for a chrome-devtools-mcp connected to a specific port.
pub(super) fn cdp_server_config(server_name: &str, port: u16) -> clickweave_mcp::McpServerConfig {
    clickweave_mcp::McpServerConfig {
        name: server_name.to_string(),
        command: "npx".into(),
        args: vec![
            "-y".into(),
            "chrome-devtools-mcp".into(),
            format!("--browserUrl=http://127.0.0.1:{}", port),
        ],
    }
}

/// Set up CDP servers for user-selected apps.
///
/// For each app: quit the running instance, relaunch with
/// `--remote-debugging-port`, spawn a chrome-devtools-mcp server, and
/// poll until ready. Returns a map of app_name → CDP server name.
async fn setup_cdp_apps(
    cdp_apps: &[CdpAppConfig],
    mcp: &mut McpRouter,
    app: &tauri::AppHandle,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    hover_dwell_ms: u64,
) -> HashMap<String, String> {
    use clickweave_core::cdp::cdp_server_name;

    let mut state: HashMap<String, String> = HashMap::new();

    for cdp_app in cdp_apps {
        // Check for cancellation between apps.
        if *cancel.borrow() {
            break;
        }

        let server_name = cdp_server_name(&cdp_app.name);

        // Check if the app is already running with a debug port — if so, skip
        // the quit/relaunch cycle and reuse the existing port.
        let port = match existing_debug_port(&cdp_app.name).await {
            Some(p) => {
                tracing::info!(
                    "'{}' already running with --remote-debugging-port={}, reusing",
                    cdp_app.name,
                    p
                );
                emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Connecting);
                p
            }
            None => {
                let port = rand_ephemeral_port();

                if cdp_app.binary_path.is_some() {
                    emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Launching);
                } else {
                    emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Restarting);
                }

                // Quit existing instance and wait for it to exit.
                let quit_args = serde_json::json!({ "app_name": &cdp_app.name });
                match mcp.call_tool("quit_app", Some(quit_args)).await {
                    Ok(r) if r.is_error == Some(true) => {
                        tracing::debug!(
                            "quit_app for '{}' returned error (may not be running)",
                            cdp_app.name
                        );
                    }
                    Err(e) => {
                        tracing::debug!("quit_app for '{}' failed: {e}", cdp_app.name);
                    }
                    _ => {}
                }

                // Poll until the app is no longer reported as running (up to 10s).
                let poll_args =
                    serde_json::json!({ "app_name": &cdp_app.name, "user_apps_only": true });
                let mut quit_confirmed = false;
                for _ in 0..20 {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    if let Ok(r) = mcp.call_tool("list_apps", Some(poll_args.clone())).await {
                        let text = r
                            .content
                            .iter()
                            .filter_map(|c| c.as_text())
                            .collect::<String>();
                        if text.trim() == "[]" {
                            quit_confirmed = true;
                            break;
                        }
                    }
                }

                if !quit_confirmed {
                    tracing::warn!("'{}' did not quit within 10s, force-killing", cdp_app.name);
                    let force_args =
                        serde_json::json!({ "app_name": &cdp_app.name, "force": true });
                    let _ = mcp.call_tool("quit_app", Some(force_args)).await;
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }

                // Relaunch with debug port.
                let launch_args = if let Some(ref binary_path) = cdp_app.binary_path {
                    serde_json::json!({
                        "app_name": binary_path,
                        "args": [format!("--remote-debugging-port={}", port)],
                    })
                } else {
                    serde_json::json!({
                        "app_name": &cdp_app.name,
                        "args": [format!("--remote-debugging-port={}", port)],
                    })
                };

                let launch_result = mcp.call_tool("launch_app", Some(launch_args)).await;

                match &launch_result {
                    Err(e) => {
                        tracing::warn!("Failed to launch '{}' with CDP: {}", cdp_app.name, e);
                        emit_cdp_progress(
                            app,
                            &cdp_app.name,
                            CdpSetupStatus::Failed {
                                reason: e.to_string(),
                            },
                        );
                        continue;
                    }
                    Ok(r) if r.is_error == Some(true) => {
                        let reason = r
                            .content
                            .iter()
                            .filter_map(|c| c.as_text())
                            .collect::<Vec<_>>()
                            .join("; ");
                        tracing::warn!(
                            "launch_app for '{}' returned error: {reason}",
                            cdp_app.name
                        );
                        emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Failed { reason });
                        continue;
                    }
                    _ => {}
                }

                // Wait for the app to start.
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;

                port
            }
        };

        emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Connecting);

        // Spawn the CDP server.
        let config = cdp_server_config(&server_name, port);
        if let Err(e) = mcp.spawn_server(&config).await {
            tracing::warn!("Failed to spawn CDP server for '{}': {}", cdp_app.name, e);
            emit_cdp_progress(
                app,
                &cdp_app.name,
                CdpSetupStatus::Failed {
                    reason: e.to_string(),
                },
            );
            continue;
        }

        // Poll until ready (10s timeout), with cancellation.
        let ready = tokio::select! {
            biased;
            _ = cancel.changed() => {
                tracing::info!("CDP setup cancelled during poll for '{}'", cdp_app.name);
                break;
            }
            result = poll_cdp_ready(mcp, &server_name, 10) => result,
        };

        match ready {
            Ok(()) => {
                tracing::info!(
                    "CDP connected to '{}' (port {}, server '{}')",
                    cdp_app.name,
                    port,
                    server_name,
                );

                // Inject click listener for record-time element capture.
                let inject_args = serde_json::json!({ "function": CDP_CLICK_LISTENER_JS });
                let inject_ok = match mcp
                    .call_tool_on(&server_name, "evaluate_script", Some(inject_args))
                    .await
                {
                    Ok(r) if r.is_error != Some(true) => {
                        tracing::info!("Injected click listener into '{}'", cdp_app.name);
                        true
                    }
                    Ok(r) => {
                        let err: String = r.content.iter().filter_map(|c| c.as_text()).collect();
                        tracing::warn!(
                            "CDP click listener injection rejected for '{}': {err}",
                            cdp_app.name
                        );
                        false
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to inject click listener into '{}': {e}",
                            cdp_app.name
                        );
                        false
                    }
                };

                // Inject hover listener alongside click listener.
                if inject_ok {
                    let hover_js = CDP_HOVER_LISTENER_JS
                        .replace("__CW_MIN_DWELL__", &hover_dwell_ms.to_string());
                    let hover_args = serde_json::json!({ "function": hover_js });
                    match mcp
                        .call_tool_on(&server_name, "evaluate_script", Some(hover_args))
                        .await
                    {
                        Ok(r) if r.is_error != Some(true) => {
                            tracing::info!("Injected hover listener into '{}'", cdp_app.name);
                        }
                        Ok(r) => {
                            let err: String =
                                r.content.iter().filter_map(|c| c.as_text()).collect();
                            tracing::warn!(
                                "CDP hover listener injection rejected for '{}': {err}",
                                cdp_app.name
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to inject hover listener into '{}': {e}",
                                cdp_app.name
                            );
                        }
                    }
                }

                if inject_ok {
                    emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Ready);
                    state.insert(cdp_app.name.clone(), server_name);
                } else {
                    emit_cdp_progress(
                        app,
                        &cdp_app.name,
                        CdpSetupStatus::Failed {
                            reason: "Click listener injection failed".to_string(),
                        },
                    );
                }
            }
            Err(e) => {
                tracing::warn!("CDP poll failed for '{}': {}", cdp_app.name, e);
                emit_cdp_progress(app, &cdp_app.name, CdpSetupStatus::Failed { reason: e });
            }
        }
    }

    state
}

/// Poll `list_pages` on a CDP server until it returns at least one page.
async fn poll_cdp_ready(
    mcp: &McpRouter,
    server_name: &str,
    timeout_secs: u64,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    loop {
        match mcp
            .call_tool_on(server_name, "list_pages", Some(serde_json::json!({})))
            .await
        {
            Ok(result) if result.is_error != Some(true) => {
                let text: String = result
                    .content
                    .iter()
                    .filter_map(|c| c.as_text())
                    .collect::<Vec<_>>()
                    .join("\n");
                // Page index may be 0-based or 1-based depending on MCP
                // server version — check for any "N: <url>" page entry.
                if text.lines().any(|l| {
                    l.as_bytes().first().is_some_and(|b| b.is_ascii_digit()) && l.contains(": ")
                }) {
                    return Ok(());
                }
                tracing::debug!(
                    "CDP list_pages for '{}' returned but no pages yet: {:?}",
                    server_name,
                    &text[..text.len().min(500)]
                );
            }
            Ok(result) => {
                let text: String = result
                    .content
                    .iter()
                    .filter_map(|c| c.as_text())
                    .collect::<Vec<_>>()
                    .join("\n");
                tracing::debug!("CDP list_pages error for '{}': {}", server_name, text);
            }
            Err(e) => {
                tracing::debug!("CDP list_pages call failed for '{}': {}", server_name, e);
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "Timed out waiting for CDP server '{}' to be ready ({}s)",
                server_name, timeout_secs
            ));
        }

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

pub(super) fn emit_cdp_progress(app: &tauri::AppHandle, app_name: &str, status: CdpSetupStatus) {
    let _ = app.emit(
        "walkthrough://cdp-setup",
        CdpSetupProgress {
            app_name: app_name.to_string(),
            status,
        },
    );
}

/// Extract the JSON payload from a chrome-devtools-mcp `evaluate_script` response.
///
/// The tool wraps results in markdown:
/// ```text
/// Script ran on page and returned:
/// ```json
/// <value>
/// ```
/// ```
fn extract_eval_result(text: &str) -> &str {
    // Look for content between ```json and ``` fences.
    if let Some(start) = text.find("```json\n") {
        let json_start = start + "```json\n".len();
        if let Some(end) = text[json_start..].find("\n```") {
            return text[json_start..json_start + end].trim();
        }
    }
    text.trim()
}

/// Retrieve the last click's DOM element data from the injected listener.
///
/// Returns a `CdpClickResolved` event if data is available, or None if the
/// click landed outside the CDP app / listener was lost.
async fn cdp_retrieve_click(
    mcp: &McpRouter,
    server_name: &str,
    app: &tauri::AppHandle,
    storage: &WalkthroughStorage,
    session_dir: &std::path::Path,
    click_event_id: Uuid,
    click_timestamp: u64,
) {
    // Poll the click queue with retries.  The macOS event tap fires before the
    // click is delivered to the app, so the JS click event may not have pushed
    // to the queue yet on the first attempt.
    const POLL_DELAYS_MS: &[u64] = &[100, 200, 300, 400];
    let mut text = String::new();

    for (attempt, &delay_ms) in POLL_DELAYS_MS.iter().enumerate() {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;

        let retrieve_args = serde_json::json!({ "function": CDP_RETRIEVE_CLICK_JS });
        let call_fut = mcp.call_tool_on(server_name, "evaluate_script", Some(retrieve_args));
        let result = match tokio::time::timeout(CDP_SNAPSHOT_TIMEOUT, call_fut).await {
            Ok(Ok(r)) if r.is_error != Some(true) => r,
            Ok(Ok(r)) => {
                let err: String = r.content.iter().filter_map(|c| c.as_text()).collect();
                tracing::debug!("CDP click retrieve error for {click_event_id}: {err}");
                return;
            }
            Ok(Err(e)) => {
                tracing::debug!("CDP click retrieve failed for {click_event_id}: {e}");
                return;
            }
            Err(_) => {
                tracing::debug!("CDP click retrieve timed out for {click_event_id}");
                return;
            }
        };

        let raw_text: String = result.content.iter().filter_map(|c| c.as_text()).collect();
        text = extract_eval_result(&raw_text).to_string();
        if text != "null" && text != "undefined" && !text.is_empty() {
            break;
        }

        if attempt < POLL_DELAYS_MS.len() - 1 {
            tracing::debug!(
                "CDP click queue empty for {click_event_id} (attempt {}), retrying",
                attempt + 1
            );
        }
    }

    if text == "null" || text == "undefined" || text.is_empty() {
        tracing::debug!("CDP click queue empty after all retries for {click_event_id}");

        // Check listener health and re-inject if lost (single MCP call).
        let check_args = serde_json::json!({ "function": CDP_CHECK_AND_REINJECT_JS });
        match mcp
            .call_tool_on(server_name, "evaluate_script", Some(check_args))
            .await
        {
            Ok(r) => {
                let raw: String = r.content.iter().filter_map(|c| c.as_text()).collect();
                let status = extract_eval_result(&raw);
                if status.contains("reinjected") {
                    tracing::info!("CDP click listener lost after navigation, re-injected");
                }
            }
            Err(e) => tracing::warn!("CDP click listener health check failed: {e}"),
        }
        return;
    }

    // Parse the JSON result from evaluate_script.
    let parsed: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("CDP click data parse failed for {click_event_id}: {e}");
            return;
        }
    };

    // Build name from ariaLabel, textContent, value, or title.
    let text_name = parsed["ariaLabel"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| parsed["textContent"].as_str().filter(|s| !s.is_empty()))
        .or_else(|| parsed["value"].as_str().filter(|s| !s.is_empty()))
        .or_else(|| parsed["title"].as_str().filter(|s| !s.is_empty()));

    // Synthesize a structural fallback when no text-based name is available.
    // These won't help at execution time but make the click visible in the
    // review panel so the user can pick a different target candidate.
    let fallback;
    let name = match text_name {
        Some(n) => n,
        None => {
            if let Some(id) = parsed["id"].as_str().filter(|s| !s.is_empty()) {
                fallback = format!("#{id}");
            } else {
                let tag = parsed["tagName"]
                    .as_str()
                    .unwrap_or("element")
                    .to_lowercase();
                fallback = match parsed["role"].as_str().filter(|s| !s.is_empty()) {
                    Some(role) => format!("{tag}[{role}]"),
                    None => tag,
                };
            }
            tracing::debug!(
                "CDP click has no text name for {click_event_id}, using fallback: {fallback}"
            );
            &fallback
        }
    };

    let role = parsed["role"].as_str().map(|s| s.to_string());
    let href = parsed["href"].as_str().map(|s| s.to_string());
    let parent_role = parsed["parentRole"].as_str().map(|s| s.to_string());
    let parent_name = parsed["parentName"].as_str().map(|s| s.to_string());

    tracing::info!(
        "CDP resolved click {click_event_id} → name={:?} role={:?}",
        name,
        role
    );

    let event = WalkthroughEvent {
        id: Uuid::new_v4(),
        timestamp: click_timestamp,
        kind: WalkthroughEventKind::CdpClickResolved {
            name: name.to_string(),
            role,
            href,
            parent_role,
            parent_name,
            click_event_id,
        },
    };
    persist_and_emit(app, storage, session_dir, &event);
}

// ---------------------------------------------------------------------------
// MCP helpers
// ---------------------------------------------------------------------------

pub(super) async fn spawn_mcp(mcp_command: &str) -> Option<McpRouter> {
    let configs = clickweave_mcp::default_server_configs(mcp_command);
    match McpRouter::spawn(&configs).await {
        Ok(router) => {
            tracing::info!(
                "MCP router spawned for walkthrough enrichment: {} servers, {} tools",
                router.server_count(),
                router.tools().len()
            );
            Some(router)
        }
        Err(e) => {
            tracing::warn!(
                "Failed to spawn MCP servers for walkthrough: {e}. Continuing without enrichment."
            );
            None
        }
    }
}

pub(super) async fn populate_app_cache(mcp: &McpRouter, cache: &mut HashMap<i32, CachedApp>) {
    let result = mcp
        .call_tool(
            "list_apps",
            Some(serde_json::json!({"user_apps_only": true})),
        )
        .await;

    if let Ok(result) = result {
        for content in &result.content {
            if let Some(text) = content.as_text() {
                // list_apps returns JSON with apps array.
                if let Ok(apps) = serde_json::from_str::<serde_json::Value>(text)
                    && let Some(arr) = apps.as_array()
                {
                    for app in arr {
                        if let (Some(name), Some(pid)) = (app["name"].as_str(), app["pid"].as_i64())
                        {
                            cache.insert(
                                pid as i32,
                                CachedApp {
                                    name: name.to_string(),
                                    bundle_id: app["bundle_id"].as_str().map(|s| s.to_string()),
                                },
                            );
                        }
                    }
                }
            }
        }
        tracing::debug!("App cache populated with {} entries", cache.len());
    }
}

async fn resolve_app_name(
    pid: i32,
    mcp: &Option<std::sync::Arc<McpRouter>>,
    cache: &mut HashMap<i32, CachedApp>,
) -> String {
    if let Some(cached) = cache.get(&pid) {
        return cached.name.clone();
    }

    // Re-fetch the app list from MCP to find the new PID.
    if let Some(mcp) = mcp {
        populate_app_cache(mcp.as_ref(), cache).await;
        if let Some(cached) = cache.get(&pid) {
            return cached.name.clone();
        }
    }

    // Insert negative-cache entry to avoid repeated MCP calls for unknown PIDs.
    let fallback = format!("PID:{pid}");
    cache.insert(
        pid,
        CachedApp {
            name: fallback.clone(),
            bundle_id: None,
        },
    );
    fallback
}

/// Background task that enriches a click with MCP data, generates a click crop,
/// and optionally resolves the target via VLM. Persists and emits all resulting
/// events.
///
/// Runs entirely off the main event loop so click capture is never blocked.
/// The crop and VLM resolution run concurrently — neither depends on the other.
#[allow(clippy::too_many_arguments)]
async fn enrich_click_background(
    mcp: std::sync::Arc<McpRouter>,
    vlm_backend: Option<std::sync::Arc<clickweave_llm::LlmClient>>,
    app: tauri::AppHandle,
    storage: WalkthroughStorage,
    session_dir: std::path::PathBuf,
    app_name: Option<String>,
    x: f64,
    y: f64,
    timestamp: u64,
    vlm_timeout: tokio::time::Duration,
    app_kind_cache: Arc<Mutex<HashMap<i32, AppKind>>>,
    target_pid: i32,
    #[cfg(target_os = "macos")] prehover_screenshot: Option<Arc<CursorRegionCapture>>,
) {
    use base64::Engine;
    use clickweave_core::walkthrough::ScreenshotMeta;

    // Run enrichment without checking the cancel token — we want MCP calls
    // to complete even after Stop is pressed so every click gets a screenshot.
    // The drain timeout in the event loop bounds total shutdown time.
    let enrichment_events =
        enrich_click(&mcp, &session_dir, x, y, app_name.as_deref(), timestamp).await;

    for ev in &enrichment_events {
        persist_and_emit(&app, &storage, &session_dir, ev);
    }

    // Extract screenshot info and AX label from enrichment events.
    let mut screenshot_path: Option<String> = None;
    let mut screenshot_meta: Option<ScreenshotMeta> = None;
    let mut ax_label_data: Option<(String, Option<String>)> = None;
    let mut has_actionable_ax = false;

    for ev in &enrichment_events {
        match &ev.kind {
            WalkthroughEventKind::ScreenshotCaptured { path, meta, .. } => {
                screenshot_path = Some(path.clone());
                screenshot_meta = *meta;
            }
            WalkthroughEventKind::AccessibilityElementCaptured { label, role, .. } => {
                // Only treat as actionable if we also have a non-empty label.
                // Elements with an actionable role but no label (e.g. unlabeled
                // buttons with a subrole) should still get VLM fallback.
                has_actionable_ax = !label.is_empty()
                    && clickweave_core::walkthrough::is_actionable_ax_role(role.as_deref());
                ax_label_data = Some((label.clone(), role.clone()));
            }
            _ => {}
        }
    }

    // Reactive Electron detection: if native AX returned nothing useful
    // and the app is still classified as Native, recheck for Electron
    // framework. This catches apps with unusual bundle structures that
    // slipped past proactive detection.
    if !has_actionable_ax {
        let current_kind = app_kind_cache.lock().unwrap().get(&target_pid).copied();
        if current_kind == Some(AppKind::Native) {
            let rechecked = classify_app_by_pid(target_pid);
            if rechecked != AppKind::Native {
                tracing::info!(
                    "Reactive detection: PID {} reclassified as {:?} (empty AX triggered recheck)",
                    target_pid,
                    rechecked,
                );
                app_kind_cache.lock().unwrap().insert(target_pid, rechecked);

                // Re-emit focus event with corrected app_kind so downstream
                // normalization picks up the reclassification.
                let updated_focus = WalkthroughEvent {
                    id: Uuid::new_v4(),
                    timestamp,
                    kind: WalkthroughEventKind::AppFocused {
                        app_name: app_name.clone().unwrap_or_default(),
                        pid: target_pid,
                        window_title: None,
                        app_kind: rechecked,
                    },
                };
                persist_and_emit(&app, &storage, &session_dir, &updated_focus);
            }
        }
    }

    // Both crop and VLM need a screenshot. Bail early if we don't have one.
    let (Some(screenshot_path), Some(screenshot_meta)) = (screenshot_path, screenshot_meta) else {
        return;
    };

    // Crop and VLM are independent — run them concurrently.
    //
    // For the crop, the cursor region capture (polled every 100ms) IS the
    // template — it's already the right size and shows the screen before
    // hover effects. Just JPEG-encode and emit it. Fall back to the MCP
    // screenshot + crop_click_region if the buffer was empty.
    //
    // VLM sends the screenshot to the vision model to identify the element.
    // Skipped when the click already has an actionable accessibility label.

    let crop_app = app.clone();
    let crop_storage = storage.clone();
    let crop_dir = session_dir.clone();
    let crop_path = screenshot_path.clone();
    let crop_fut = async move {
        use super::walkthrough_enrichment::crop_click_region;

        let artifacts_dir = crop_dir.join("artifacts");

        let emit_crop = |b64: String, path: std::path::PathBuf| {
            let ev = WalkthroughEvent {
                id: Uuid::new_v4(),
                timestamp,
                kind: WalkthroughEventKind::ScreenshotCaptured {
                    path: path.to_string_lossy().to_string(),
                    kind: ScreenshotKind::ClickCrop,
                    meta: None,
                    image_b64: Some(b64),
                },
            };
            persist_and_emit(&crop_app, &crop_storage, &crop_dir, &ev);
        };

        // Try the cursor region capture first (pre-hover, already cropped).
        #[cfg(target_os = "macos")]
        if let Some(shot) = prehover_screenshot {
            tracing::debug!("Using cursor region capture for click crop");
            let artifacts_for_capture = artifacts_dir.clone();
            let crop_result = tokio::task::spawn_blocking(move || {
                let img =
                    image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba_bytes.clone())?;
                let dynamic = image::DynamicImage::ImageRgba8(img);
                let mut jpeg_buf = std::io::Cursor::new(Vec::new());
                dynamic
                    .write_to(&mut jpeg_buf, image::ImageFormat::Jpeg)
                    .ok()?;
                let jpeg_bytes = jpeg_buf.into_inner();
                let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg_bytes);
                let filename = format!("crop_{timestamp}.jpg");
                let path = artifacts_for_capture.join(&filename);
                let _ = std::fs::write(&path, &jpeg_bytes);
                Some((b64, path))
            })
            .await;
            if let Ok(Some((crop_b64, crop_path))) = crop_result {
                emit_crop(crop_b64, crop_path);
                return;
            }
        }

        // Fallback: crop from the MCP screenshot.
        tracing::debug!("Falling back to MCP screenshot for crop");
        let bytes = match tokio::fs::read(&crop_path).await {
            Ok(b) => b,
            Err(_) => return,
        };
        let (px, py) = screenshot_meta.screen_to_pixel(x, y);
        let scale = screenshot_meta.scale;
        let crop_result = tokio::task::spawn_blocking(move || {
            let img = image::load_from_memory(&bytes).ok()?;
            crop_click_region(&img, px, py, scale).map(|(jpeg, b64)| {
                let filename = format!("crop_{timestamp}.jpg");
                let path = artifacts_dir.join(&filename);
                let _ = std::fs::write(&path, &jpeg);
                (b64, path)
            })
        })
        .await;
        if let Ok(Some((crop_b64, crop_path))) = crop_result {
            emit_crop(crop_b64, crop_path);
        }
    };

    let vlm_fut = async {
        if has_actionable_ax {
            return;
        }
        let backend = match vlm_backend {
            Some(ref b) => b,
            None => return,
        };
        let ax_ref = ax_label_data
            .as_ref()
            .map(|(l, r)| (l.as_str(), r.as_deref()));
        let req = match prepare_vlm_click_request(
            &screenshot_path,
            x,
            y,
            screenshot_meta,
            ax_ref,
            None,
            app_name.as_deref(),
        ) {
            Some(r) => r,
            None => return,
        };

        let vlm_result = tokio::time::timeout(
            vlm_timeout,
            execute_vlm_click_request(backend.as_ref(), &req),
        )
        .await;

        match vlm_result {
            Ok(Some(label)) => {
                tracing::info!("VLM resolved click at ts={timestamp} → \"{label}\"");
                let vlm_event = WalkthroughEvent {
                    id: Uuid::new_v4(),
                    timestamp,
                    kind: WalkthroughEventKind::VlmLabelResolved { label },
                };
                persist_and_emit(&app, &storage, &session_dir, &vlm_event);
            }
            Ok(None) => {}
            Err(_) => {
                tracing::warn!("VLM timed out for click at ts={timestamp}");
            }
        }
    };

    tokio::join!(crop_fut, vlm_fut);
}

#[cfg(test)]
mod tests {
    use clickweave_core::MouseButton;

    use super::*;

    fn click_event(timestamp: u64, x: f64, y: f64) -> WalkthroughEvent {
        WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp,
            kind: WalkthroughEventKind::MouseClicked {
                x,
                y,
                button: MouseButton::Left,
                click_count: 1,
                modifiers: vec![],
            },
        }
    }

    fn stopped_event(timestamp: u64) -> WalkthroughEvent {
        WalkthroughEvent {
            id: Uuid::new_v4(),
            timestamp,
            kind: WalkthroughEventKind::Stopped,
        }
    }

    // --- strip_recording_bar_click ---

    #[test]
    fn strip_removes_click_inside_bar() {
        let bar = (100.0, 200.0, 300.0, 50.0);
        let mut events = vec![
            click_event(1, 50.0, 50.0),   // outside bar — keep
            click_event(2, 150.0, 220.0), // inside bar (last click)
        ];
        strip_recording_bar_click(&mut events, bar);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].timestamp, 1);
    }

    #[test]
    fn strip_keeps_click_outside_bar() {
        let bar = (100.0, 200.0, 300.0, 50.0);
        let mut events = vec![
            click_event(1, 50.0, 50.0),
            click_event(2, 50.0, 100.0), // outside bar
        ];
        strip_recording_bar_click(&mut events, bar);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn strip_noop_when_no_clicks() {
        let bar = (100.0, 200.0, 300.0, 50.0);
        let mut events = vec![stopped_event(1)];
        strip_recording_bar_click(&mut events, bar);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn strip_removes_all_events_with_same_timestamp() {
        let bar = (100.0, 200.0, 300.0, 50.0);
        let mut events = vec![
            click_event(1, 50.0, 50.0),   // different ts — keep
            click_event(2, 150.0, 220.0), // inside bar, ts=2
            stopped_event(2),             // same ts as bar click — also removed
        ];
        strip_recording_bar_click(&mut events, bar);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].timestamp, 1);
    }

    // --- rand_ephemeral_port ---

    #[test]
    fn ephemeral_port_in_range() {
        for _ in 0..100 {
            let port = rand_ephemeral_port();
            assert!(
                (49152..=65535).contains(&port),
                "port {port} outside ephemeral range"
            );
        }
    }

    // --- cdp_server_config ---

    #[test]
    fn cdp_server_config_builds_correctly() {
        let config = cdp_server_config("cdp:Discord", 9222);
        assert_eq!(config.name, "cdp:Discord");
        assert_eq!(config.command, "npx");
        assert_eq!(
            config.args,
            vec![
                "-y",
                "chrome-devtools-mcp",
                "--browserUrl=http://127.0.0.1:9222"
            ]
        );
    }
}
