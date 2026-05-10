use serde::{Deserialize, Serialize};

/// The type of data an output field produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum OutputFieldType {
    Bool,
    Number,
    String,
    Array,
    Object,
    Any,
}

/// A declared output field on a node type (compile-time schema metadata).
#[derive(Debug, Clone)]
pub struct OutputField {
    pub name: &'static str,
    pub field_type: OutputFieldType,
    pub description: &'static str,
}

/// Owned version of OutputField for TypeScript bindings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct OutputFieldInfo {
    pub name: String,
    pub field_type: OutputFieldType,
    pub description: String,
}

impl From<&OutputField> for OutputFieldInfo {
    fn from(f: &OutputField) -> Self {
        Self {
            name: f.name.to_string(),
            field_type: f.field_type,
            description: f.description.to_string(),
        }
    }
}

/// Method used to verify an action node's effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum VerificationMethod {
    Vlm,
    Dom,
    AccessibilityTree,
}

/// Verification configuration carried by action-node params structs.
///
/// Flattens onto its owner via `#[serde(flatten)]` as the pair of sibling
/// fields `verification_method` / `verification_assertion`, which is the
/// same on-disk layout the core types used before the substruct was
/// extracted. Both fields are optional in the stored representation — a
/// verification is only considered "configured" when both halves are
/// present. That check is centralized in [`VerificationConfig::resolved`]
/// and exposed through the [`HasVerification`] trait.
///
/// Using `Option` on the inner fields (rather than wrapping the whole
/// substruct in `Option<_>`) keeps `specta::Flatten` satisfied — specta
/// does not implement `Flatten` for `Option<T>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct VerificationConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_method: Option<VerificationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_assertion: Option<String>,
}

impl VerificationConfig {
    /// Create a fully-configured verification from a method + assertion pair.
    pub fn new(method: VerificationMethod, assertion: impl Into<String>) -> Self {
        Self {
            verification_method: Some(method),
            verification_assertion: Some(assertion.into()),
        }
    }

    /// True when neither half is set.
    pub fn is_empty(&self) -> bool {
        self.verification_method.is_none() && self.verification_assertion.is_none()
    }

    /// Resolve to a concrete `(method, assertion)` pair when both halves are
    /// present. Partial configs (one half missing) resolve to `None`.
    pub fn resolved(&self) -> Option<ResolvedVerification<'_>> {
        match (self.verification_method, &self.verification_assertion) {
            (Some(method), Some(assertion)) => Some(ResolvedVerification { method, assertion }),
            _ => None,
        }
    }
}

/// Borrowed view of a fully-configured verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedVerification<'a> {
    pub method: VerificationMethod,
    pub assertion: &'a str,
}

/// Uniform accessor for the [`VerificationConfig`] carried by every
/// action-node params struct. Returns `None` for node types that do not
/// support verification.
pub trait HasVerification {
    fn verification(&self) -> Option<&VerificationConfig>;

    /// Convenience: the resolved verification (both halves present) if any.
    fn resolved_verification(&self) -> Option<ResolvedVerification<'_>> {
        self.verification().and_then(|v| v.resolved())
    }
}

/// What kind of data a node produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum OutputRole {
    Query,
    Action,
    Ai,
    Generic,
}

/// The execution context a node operates in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum NodeContext {
    Native,
    Cdp,
    Independent,
}

// --- Output schema registry ---



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_field_type_serde_roundtrip() {
        for t in [
            OutputFieldType::Bool,
            OutputFieldType::Number,
            OutputFieldType::String,
            OutputFieldType::Array,
            OutputFieldType::Object,
            OutputFieldType::Any,
        ] {
            let json = serde_json::to_string(&t).unwrap();
            let back: OutputFieldType = serde_json::from_str(&json).unwrap();
            assert_eq!(t, back);
        }
    }
}
