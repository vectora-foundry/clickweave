//! Shared prompt-building primitives for executor LLM calls.
//!
//! Keeps the "JSON only, no fences" contract in one place so a future
//! prompt-format change touches a single line instead of every resolver,
//! verdict, and supervision site.

/// Sentence inserted into every executor LLM prompt that requests a JSON
/// body. Accompanies a follow-up sentence or schema the caller emits on the
/// same or next line.
pub(crate) const JSON_ONLY_INSTRUCTION: &str =
    "Respond with ONLY a JSON object (no markdown fences).";
