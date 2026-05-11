use crate::AppKind;
use crate::output_schema::{HasVerification, VerificationConfig, VerificationMethod};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// --- Verification helpers ---

/// Generate a default-tolerant `Deserialize` impl + [`HasVerification`] impl
/// for an action params struct that carries a flattened [`VerificationConfig`]
/// as its `verification` field.
///
/// The derived `Deserialize` on the struct itself would also work, but going
/// through a `Raw` helper lets the struct use `#[serde(default)]`-style
/// semantics across *all* fields without listing `#[serde(default)]` on every
/// one, and guarantees that missing-everywhere JSON produces the struct's
/// `Default`. It also lets the [`HasVerification`] impl live inside the
/// macro so there's no chance of drift between the data layout and the
/// accessor.
macro_rules! impl_verification_deser {
    (
        $ty:ident {
            $( $field:ident : $field_ty:ty = $default:expr ),* $(,)?
        }
    ) => {
        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> Result<Self, D::Error> {
                #[derive(Deserialize)]
                struct Raw {
                    $(
                        #[serde(default)]
                        $field: Option<$field_ty>,
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
                Ok($ty {
                    $( $field: raw.$field.unwrap_or_else(|| $default), )*
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

mod desktop;
mod targets;
mod trace;

pub use desktop::*;
pub use targets::*;
pub use trace::*;

#[cfg(test)]
mod tests;
