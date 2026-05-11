//! Compatibility shim — the canonical implementations live in
//! [`super::parser`] (markdown body + marker grammar) and
//! [`super::emitter`] (minimal frontmatter + fenced action_sketch).
//!
//! Existing callers that still import `frontmatter::{parse_skill_md,
//! emit_skill_md}` keep working. New code should import from `parser`
//! and `emitter` directly.

#![allow(dead_code)]

pub use super::emitter::emit_skill_md;
pub use super::parser::parse_skill_md;
