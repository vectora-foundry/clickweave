use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A single snapshot line that matched the resolver target, retained so the
/// agent loop (or a human reading the error) can disambiguate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdpCandidate {
    /// UID parsed from the snapshot line (e.g. `a5`, `1_0`).
    pub uid: String,
    /// The full snapshot line, trimmed, for context.
    pub snippet: String,
}

/// Viewport rectangle of a candidate element, expressed in CSS pixels relative
/// to the top-left of the page viewport.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Extended candidate record carrying its viewport rect so the UI can draw
/// overlays. Used by the `AmbiguityResolved` executor event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateView {
    pub uid: String,
    pub snippet: String,
    pub rect: Option<Rect>,
}

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("LLM error: {0}")]
    Llm(String),

    #[error("Tool call failed: {tool}: {message}")]
    ToolCall { tool: String, message: String },

    #[error("App resolution failed: {0}")]
    AppResolution(String),

    #[error("Element resolution failed: {0}")]
    ElementResolution(String),

    #[error("Click target not found: {0}")]
    ClickTarget(String),

    /// Generic CDP error — retained for paths where a more specific variant
    /// isn't yet wired up.
    #[error("CDP error: {0}")]
    Cdp(String),

    /// Taking a CDP snapshot (`cdp_take_snapshot`) itself failed or returned an
    /// MCP-level error. Treat this as transient: the resolver retries the
    /// snapshot a handful of times before surfacing the failure.
    #[error("CDP snapshot failed: {0}")]
    CdpSnapshotFailed(String),

    /// The CDP snapshot was collected successfully but no accessibility-tree
    /// line matched the resolver target after the retry budget was exhausted.
    /// Permanent failure for this attempt — the page does not contain the
    /// target, or the label does not match; no amount of additional retrying
    /// will change the snapshot.
    #[error("No matching CDP element for target '{target}'")]
    CdpNotFound { target: String },

    /// An ambiguous CDP target was routed through the agent-driven
    /// disambiguation round-trip and the round-trip itself failed (bad VLM
    /// reply, unknown chosen uid, missing screenshot, etc.). Distinct from
    /// [`Self::CdpAmbiguousTarget`], which is the initial signal that
    /// disambiguation is needed; this variant is the terminal failure of
    /// that resolution attempt.
    #[error("CDP disambiguation failed: {0}")]
    CdpDisambiguationFailed(String),

    /// Quitting/killing/relaunching the CDP-capable app failed.
    #[error("CDP relaunch failed for '{app_name}': {message}")]
    CdpRelaunchFailed { app_name: String, message: String },

    /// `cdp_connect` never succeeded within the configured retry budget.
    #[error("Failed to connect CDP for '{app_name}' after {attempts} attempts: {last_error}")]
    CdpConnectTimeout {
        app_name: String,
        attempts: u32,
        last_error: String,
    },

    #[error(
        "Ambiguous CDP target '{target}': {} candidates matched (uids: {})",
        candidates.len(),
        candidates.iter().map(|c| c.uid.as_str()).collect::<Vec<_>>().join(", ")
    )]
    CdpAmbiguousTarget {
        target: String,
        candidates: Vec<CdpCandidate>,
    },

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Cancelled")]
    Cancelled,

    #[error("IO error: {0}")]
    Io(String),

    #[error("MCP spawn failed: {0}")]
    McpSpawn(String),

    #[error("Variable not found: {reference}")]
    VariableNotFound { reference: String },

    #[error("Invalid coordinates: {0}")]
    InvalidCoordinates(String),

    #[error(
        "No CDP connection — ensure a FocusWindow or LaunchApp targeting a CDP-capable app runs before {node_type}"
    )]
    NoCdpConnection { node_type: String },

    /// `take_ax_snapshot` itself failed. Transient — the executor retries once
    /// internally before surfacing this.
    #[error("AX snapshot failed: {0}")]
    AxSnapshotFailed(String),

    /// The current AX snapshot contains no element whose descriptor matches
    /// the node's target, so dispatch cannot proceed.
    #[error("No matching AX element for target '{target}'")]
    AxNotFound { target: String },

    /// AX dispatch returned a typed error code from the MCP server (e.g.
    /// `snapshot_expired`, `not_dispatchable`, `no_row_ancestor`,
    /// `no_outline_container`, `ax_error`). Carries the optional `fallback`
    /// screen coordinate the server suggests for a coord-based retry.
    #[error("AX dispatch failed ({tool}): {code}: {message}")]
    AxDispatch {
        tool: String,
        code: String,
        message: String,
        fallback: Option<(f64, f64)>,
    },
}

/// Alias used throughout the executor.
pub type ExecutorResult<T> = Result<T, ExecutorError>;
