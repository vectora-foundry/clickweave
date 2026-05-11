use super::*;

// --- CDP target enum ---

/// Distinguishes how a CDP element target was produced, so the executor can
/// choose the right resolution strategy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", content = "value")]
pub enum CdpTarget {
    /// Precise element name from `cdp_find_elements` or walkthrough recording.
    ExactLabel(String),
    /// Semantic description (e.g. "the message input field") — always resolved
    /// via snapshot + LLM at execution time.
    Intent(String),
    /// Concrete DOM UID resolved at execution time (for Run mode / decision cache).
    ResolvedUid(String),
}

impl Default for CdpTarget {
    fn default() -> Self {
        Self::Intent(String::new())
    }
}

impl CdpTarget {
    /// The inner string regardless of variant.
    pub fn as_str(&self) -> &str {
        match self {
            Self::ExactLabel(s) | Self::Intent(s) | Self::ResolvedUid(s) => s,
        }
    }
}

// --- Target-param deser macro ---

/// Generate backward-compatible deserialization for params structs whose
/// `target` field is a typed target enum (`CdpTarget` / `AxTarget`).
///
/// Accepts both the current tagged shape
/// `{"target": {"kind": "...", "value": "..."}}` and the legacy on-disk
/// shape `{"uid": "..."}`, which is routed through `$uid_fallback` to build
/// the right target variant (e.g. `CdpTarget::ExactLabel` for CDP, which
/// tries an exact-label CDP query, or `AxTarget::ResolvedUid` for AX,
/// which treats it as a raw snapshot uid). Also handles the migration
/// from split `verification_method` / `verification_assertion` fields to
/// the flattened [`VerificationConfig`] substruct.
macro_rules! impl_target_deser {
    (
        $ty:ident,
        target: $target_ty:ty,
        uid_fallback: $uid_fallback:path,
        { $($extra_field:ident : $extra_ty:ty),* $(,)? }
    ) => {
        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                #[derive(Deserialize)]
                struct Raw {
                    #[serde(default)]
                    target: Option<$target_ty>,
                    #[serde(default)]
                    uid: Option<String>,
                    $(
                        #[serde(default)]
                        $extra_field: $extra_ty,
                    )*
                    #[serde(default)]
                    verification_method: Option<VerificationMethod>,
                    #[serde(default)]
                    verification_assertion: Option<String>,
                }
                let raw = Raw::deserialize(deserializer)?;
                let verification = VerificationConfig {
                    verification_method: raw.verification_method,
                    verification_assertion: raw.verification_assertion,
                };
                Ok(Self {
                    target: match (raw.target, raw.uid) {
                        (Some(t), _) => t,
                        (None, Some(uid)) => $uid_fallback(uid),
                        (None, None) => <$target_ty>::default(),
                    },
                    $( $extra_field: raw.$extra_field, )*
                    verification,
                })
            }
        }

        impl HasVerification for $ty {
            fn verification(&self) -> Option<&VerificationConfig> {
                if self.verification.is_empty() {
                    None
                } else {
                    Some(&self.verification)
                }
            }
        }
    };
}

// --- CDP node params ---

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpClickParams {
    pub target: CdpTarget,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_target_deser!(CdpClickParams, target: CdpTarget, uid_fallback: CdpTarget::ExactLabel, {});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpHoverParams {
    pub target: CdpTarget,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_target_deser!(CdpHoverParams, target: CdpTarget, uid_fallback: CdpTarget::ExactLabel, {});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpFillParams {
    pub target: CdpTarget,
    pub value: String,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_target_deser!(
    CdpFillParams,
    target: CdpTarget,
    uid_fallback: CdpTarget::ExactLabel,
    { value: String }
);

// --- AX target enum ---

/// Distinguishes how a macOS accessibility-tree element target was produced,
/// so the executor can choose the right resolution strategy at dispatch time.
///
/// AX snapshots are session-stateful: every call to `take_ax_snapshot` bumps a
/// generation and emits uids like `a42g3`. Uids from prior snapshots are
/// rejected by `ax_click` / `ax_set_value` / `ax_select` with
/// `snapshot_expired`. To replay safely, the executor re-snapshots immediately
/// before each dispatch and resolves the node's descriptor (role + name) to a
/// fresh uid — see [`clickweave_engine::executor::deterministic::ax`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", content = "value")]
pub enum AxTarget {
    /// Replay-stable descriptor. Executor re-resolves via `take_ax_snapshot`
    /// and matches the first entry with this `role` whose `name` matches —
    /// optional `parent_name` breaks ties for sidebars/outlines where many
    /// rows share a role.
    Descriptor {
        role: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_name: Option<String>,
    },
    /// Raw uid captured at agent run time. Valid only for the current AX
    /// snapshot generation — will fail with `snapshot_expired` on replay.
    /// Agent-loop post-hooks upgrade `ResolvedUid` to `Descriptor` when the
    /// original snapshot is still on hand.
    ResolvedUid(String),
}

impl Default for AxTarget {
    fn default() -> Self {
        Self::ResolvedUid(String::new())
    }
}

impl AxTarget {
    /// A human-readable handle for the target regardless of variant — the
    /// descriptor's `name`, or the uid string.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Descriptor { name, .. } => name.as_str(),
            Self::ResolvedUid(s) => s.as_str(),
        }
    }
}

// --- AX node params ---

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AxClickParams {
    pub target: AxTarget,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_target_deser!(AxClickParams, target: AxTarget, uid_fallback: AxTarget::ResolvedUid, {});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AxSetValueParams {
    pub target: AxTarget,
    pub value: String,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_target_deser!(
    AxSetValueParams,
    target: AxTarget,
    uid_fallback: AxTarget::ResolvedUid,
    { value: String }
);

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AxSelectParams {
    pub target: AxTarget,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_target_deser!(AxSelectParams, target: AxTarget, uid_fallback: AxTarget::ResolvedUid, {});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpTypeParams {
    pub text: String,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(CdpTypeParams {
    text: String = String::new(),
});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpPressKeyParams {
    pub key: String,
    #[serde(default)]
    pub modifiers: Vec<String>,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(
    CdpPressKeyParams {
        key: String = String::new(),
        modifiers: Vec<String> = Vec::new(),
    }
);

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpNavigateParams {
    pub url: String,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(CdpNavigateParams {
    url: String = String::new(),
});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpNewPageParams {
    #[serde(default)]
    pub url: String,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(CdpNewPageParams {
    url: String = String::new(),
});

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpClosePageParams {
    #[serde(default)]
    pub page_index: Option<u32>,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(
    CdpClosePageParams {
        page_index: Option<u32> = None,
    }
);

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpSelectPageParams {
    pub page_index: u32,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl_verification_deser!(CdpSelectPageParams {
    page_index: u32 = 0,
});

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpWaitParams {
    pub text: String,
    #[serde(default = "default_cdp_wait_timeout")]
    pub timeout_ms: u64,
}

fn default_cdp_wait_timeout() -> u64 {
    10_000
}

impl Default for CdpWaitParams {
    fn default() -> Self {
        Self {
            text: String::new(),
            timeout_ms: default_cdp_wait_timeout(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CdpHandleDialogParams {
    pub accept: bool,
    #[serde(default)]
    pub prompt_text: Option<String>,
    #[serde(flatten, default)]
    pub verification: VerificationConfig,
}

impl Default for CdpHandleDialogParams {
    fn default() -> Self {
        Self {
            accept: true,
            prompt_text: None,
            verification: VerificationConfig::default(),
        }
    }
}

impl_verification_deser!(
    CdpHandleDialogParams {
        accept: bool = true,
        prompt_text: Option<String> = None,
    }
);
