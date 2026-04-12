//! Runtime context for workflow execution.
//!
//! Holds variables produced by node outputs.

use serde_json::Value;
use std::collections::HashMap;

/// Runtime state maintained during workflow execution.
#[derive(Debug, Default)]
pub struct RuntimeContext {
    /// Variables produced by node outputs.
    /// Key format: "<auto_id>.<field>" (e.g., "find_text_1.found").
    pub variables: HashMap<String, Value>,
}

impl RuntimeContext {
    /// Create a new, empty runtime context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or update a variable.
    pub fn set_variable(&mut self, name: impl Into<String>, value: Value) {
        self.variables.insert(name.into(), value);
    }

    /// Remove all variables whose key starts with the given prefix.
    pub fn remove_variables_with_prefix(&mut self, prefix: &str) {
        let dot_prefix = format!("{}.", prefix);
        self.variables
            .retain(|k, _| !k.starts_with(&dot_prefix) && k != prefix);
    }

    /// Look up a variable by name.
    pub fn get_variable(&self, name: &str) -> Option<&Value> {
        self.variables.get(name)
    }
}
