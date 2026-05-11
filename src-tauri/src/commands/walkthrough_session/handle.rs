use super::*;

pub struct WalkthroughHandle {
    pub session: Option<WalkthroughSessionRuntime>,
    pub session_dir: Option<std::path::PathBuf>,
    pub(crate) storage: Option<WalkthroughStorage>,
    #[cfg(target_os = "macos")]
    pub(crate) event_tap: Option<MacOSEventTap>,
    #[cfg(target_os = "windows")]
    pub(crate) event_hook: Option<WindowsEventHook>,
    pub(crate) processing_task: Option<tauri::async_runtime::JoinHandle<()>>,
    /// Cancellation signal for the processing loop.
    pub(crate) cancel_tx: tokio::sync::watch::Sender<bool>,
}

impl Default for WalkthroughHandle {
    fn default() -> Self {
        let (cancel_tx, _) = tokio::sync::watch::channel(false);
        Self {
            session: None,
            session_dir: None,
            storage: None,
            #[cfg(target_os = "macos")]
            event_tap: None,
            #[cfg(target_os = "windows")]
            event_hook: None,
            processing_task: None,
            cancel_tx,
        }
    }
}

impl WalkthroughHandle {
    pub(crate) fn ensure_status(
        &self,
        expected: &[WalkthroughStatus],
    ) -> Result<&WalkthroughSessionRuntime, crate::commands::error::CommandError> {
        let session =
            self.session
                .as_ref()
                .ok_or(crate::commands::error::CommandError::validation(
                    "No walkthrough session is active",
                ))?;
        if !expected.contains(&session.meta.status) {
            return Err(crate::commands::error::CommandError::validation(format!(
                "Walkthrough is in {:?} state, expected one of {:?}",
                session.meta.status, expected
            )));
        }
        Ok(session)
    }

    /// Stop the capture backend and return the processing task handle.
    ///
    /// Signals the cancellation token so the processing loop exits promptly
    /// (any in-flight MCP call is dropped via `select!`). The caller should
    /// `await` the returned handle for a clean shutdown.
    pub(crate) fn stop_capture(&mut self) -> Option<tauri::async_runtime::JoinHandle<()>> {
        let _ = self.cancel_tx.send(true);

        #[cfg(target_os = "macos")]
        if let Some(tap) = self.event_tap.take() {
            tap.send_command(CaptureCommand::Stop);
            // Drop the tap handle — this joins the thread and closes the sender.
            drop(tap);
        }

        #[cfg(target_os = "windows")]
        if let Some(hook) = self.event_hook.take() {
            hook.send_command(CaptureCommand::Stop);
            drop(hook);
        }

        self.processing_task.take()
    }
}

// ---------------------------------------------------------------------------
// Async event processing loop
// ---------------------------------------------------------------------------
