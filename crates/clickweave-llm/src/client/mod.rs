use crate::types::*;
use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tracing::{debug, error, info, trace, warn};

/// Default request timeout applied to `LlmClient`'s shared HTTP client.
/// Prevents hung chat completions from stalling the executor indefinitely
/// while still leaving room for slow local models on CPU.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

mod backend;
mod config;
mod endpoint;
mod http;
mod model_info;
mod prompts;
mod vision;

pub use backend::{ChatBackend, ChatOptions};
pub use config::LlmConfig;
pub use endpoint::{check_endpoint, list_models};
pub use prompts::{build_step_prompt, build_vlm_prompt, vlm_system_prompt, workflow_system_prompt};
pub use vision::analyze_images;

pub struct LlmClient {
    config: LlmConfig,
    http: reqwest::Client,
    /// Cached context length from provider, 0 means unknown.
    context_length: AtomicU64,
}

#[cfg(test)]
mod tests;
