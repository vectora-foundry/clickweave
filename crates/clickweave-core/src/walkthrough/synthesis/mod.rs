use uuid::Uuid;

use crate::{
    AxClickParams, AxSelectParams, AxTarget, CdpClickParams, CdpHoverParams, CdpTarget,
    ClickParams, ClickTarget, Edge, FocusTarget, FocusWindowParams, HoverParams, MouseButton, Node,
    NodeType, Position, PressKeyParams, ScrollParams, TypeTextParams, Workflow,
};

use super::event_coalescing::{
    KEY_COALESCE_GAP_MS, SCROLL_COALESCE_GAP_MS, TEXT_IDLE_GAP_MS, flush_text,
};
use super::event_interpretation::{WindowControl, shortcut_display_name};
use super::target_resolution::{
    AxElementData, CdpElementData, ClickEnrichment, build_target_candidates, score_confidence,
};
use super::types::{
    ActionConfidence, ScreenshotKind, ScreenshotMeta, TargetCandidate, WalkthroughAction,
    WalkthroughActionKind, WalkthroughEvent, WalkthroughEventKind,
};

// Re-export OCR_PROXIMITY_PX so external callers (if any) still find it here.
pub use super::target_resolution::OCR_PROXIMITY_PX;

mod draft;
mod normalize;

pub use draft::synthesize_draft;
pub use normalize::normalize_events;

#[cfg(test)]
mod tests;
