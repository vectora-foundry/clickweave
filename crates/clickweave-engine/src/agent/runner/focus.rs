use super::*;

/// Why a `focus_window` MCP call was suppressed by the runner. Ported
/// verbatim from the legacy `FocusSkipReason`.
///
/// The LLM sees a synthetic `StepOutcome::Success` whose text comes from
/// [`FocusSkipReason::llm_message`]; that text must stay byte-identical to
/// the legacy strings so replay / transcript tests still pin the same
/// contract.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum FocusSkipReason {
    /// macOS Native target, full AX dispatch toolset available —
    /// AX dispatch is focus-preserving so the real call is redundant.
    AxAvailable,
    /// Electron / Chrome target with a live CDP session and the minimum
    /// CDP dispatch toolset — CDP operates on backgrounded windows.
    CdpLive,
    /// Electron / Chrome target where CDP isn't live yet but the MCP
    /// server advertises `cdp_connect`. The post-tool hook's
    /// `auto_connect_cdp` will fire on its own; the real `focus_window`
    /// is unnecessary and would only steal foreground in the meantime.
    CdpAttachable,
    /// Operator flipped [`AgentConfig::allow_focus_window`] to `false`;
    /// every focus_window is dropped regardless of kind or toolset.
    PolicyDisabled,
}

#[derive(Debug, Clone)]
pub(super) struct RunningAppInfo {
    pub(super) name: String,
    pub(super) pid: Option<i32>,
    pub(super) kind: Option<String>,
}

impl FocusSkipReason {
    pub(super) const ALL: [Self; 4] = [
        Self::AxAvailable,
        Self::CdpLive,
        Self::CdpAttachable,
        Self::PolicyDisabled,
    ];

    /// Result text returned to the LLM in the synthetic
    /// `StepOutcome::Success`. Must not drift from the strings the tests
    /// pin — they encode the agent→LLM skip-contract.
    pub(crate) const fn llm_message(self) -> &'static str {
        match self {
            Self::AxAvailable => {
                "skipped focus_window: AX tools available; window focus not required"
            }
            Self::CdpLive => "skipped focus_window: CDP already live; focus not required",
            Self::CdpAttachable => {
                "focus_window skipped: CDP-attachable target; auto-connect will fire. \
                 Use cdp_* tools after the connection lands."
            }
            Self::PolicyDisabled => {
                "focus_window skipped: agent policy disallows focus changes. Use AX dispatch \
                 (ax_click/ax_set_value/ax_select) or CDP (cdp_click/cdp_fill) instead — \
                 these operate on background windows."
            }
        }
    }

    /// Terse summary for the `SubAction` event surface.
    pub(crate) const fn sub_action_summary(self) -> &'static str {
        match self {
            Self::AxAvailable => "skipped: AX dispatch available",
            Self::CdpLive => "skipped: CDP already live; focus not required",
            Self::CdpAttachable => "skipped: CDP-attachable target; auto-connect will fire",
            Self::PolicyDisabled => "skipped: focus_window disabled by agent policy",
        }
    }

    /// Recover the variant from an LLM-visible result text. Used by the
    /// post-step bookkeeping predicate to keep synthetic skips invisible
    /// to CDP auto-connect and workflow-node creation.
    pub(crate) fn from_llm_message(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.llm_message() == text)
    }
}

/// True when a model-authored `launch_app` call has no launch-only
/// arguments. Native-devtools brings an already-running app to the
/// foreground in this shape, so the no-focus policy must verify the
/// process state before dispatching it.
pub(super) fn launch_app_has_launch_only_args(arguments: &Value) -> bool {
    match arguments.get("args") {
        Some(Value::Array(args)) => !args.is_empty(),
        Some(Value::String(args)) => !args.trim().is_empty(),
        Some(Value::Null) | None => false,
        Some(_) => true,
    }
}

pub(super) fn force_background_launch_app(action: &mut AgentAction, allow_focus_window: bool) {
    if allow_focus_window {
        return;
    }
    let AgentAction::ToolCall {
        tool_name,
        arguments,
        ..
    } = action
    else {
        return;
    };
    if tool_name != "launch_app"
        || arguments
            .get("background")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return;
    }
    if let Value::Object(map) = arguments {
        map.insert("background".to_string(), Value::Bool(true));
    }
}

/// macOS AX-dispatch toolset — every tool required for the
/// focus-preserving automation path. When the MCP server advertises
/// **all** of these plus `take_ax_snapshot`, the agent can drive native
/// apps without moving the cursor or raising windows, which makes a
/// preceding `focus_window` call redundant (and focus-stealing).
///
/// Mirrors the legacy `AX_DISPATCH_TOOLSET` byte-for-byte.
pub(super) const AX_DISPATCH_TOOLSET: &[&str] =
    &["take_ax_snapshot", "ax_click", "ax_set_value", "ax_select"];

/// Minimum CDP dispatch toolset required before the runner may suppress
/// a `focus_window` against an Electron / Chrome-browser target. Kept
/// conservative: `cdp_find_elements` + `cdp_click` is enough to prove
/// the agent's next move will operate against the CDP target (all CDP
/// operations are focus-preserving). Servers missing these tools fall
/// through to the real `focus_window`.
///
/// Mirrors the legacy `CDP_DISPATCH_TOOLSET` byte-for-byte.
pub(super) const CDP_DISPATCH_TOOLSET: &[&str] = &["cdp_find_elements", "cdp_click"];

/// True when every member of `toolset` is advertised by `mcp`.
pub(super) fn mcp_has_toolset<M: Mcp + ?Sized>(mcp: &M, toolset: &[&str]) -> bool {
    toolset.iter().all(|name| mcp.has_tool(name))
}

/// Coordinate-based primitives that move the cursor and steal focus.
/// `coordinate_primitive_blocked` rejects these when a structured
/// surface (CDP page or Native AX dispatch) is wired for the focused
/// app. Mirrors the `Coordinate` arm of
/// [`crate::agent::prompt::classify_tool_family`] but kept narrower:
/// `find_text` / `find_image` / `element_at_point` are coordinate
/// *observations*, not actions, so they pass through.
pub(super) fn is_coordinate_primitive(name: &str) -> bool {
    matches!(
        name,
        "click" | "type_text" | "press_key" | "move_mouse" | "scroll" | "drag"
    )
}
