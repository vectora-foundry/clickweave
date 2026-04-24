//! Object-safe adapter for [`ChatBackend`].
//!
//! `ChatBackend` returns `impl Future` from its methods, which makes it
//! incompatible with `dyn` dispatch. Callers that need to store a backend
//! behind `Arc<dyn …>` (e.g. `StateRunner::vision`) wrap it in this adapter
//! instead — the blanket impl boxes the futures so the trait becomes
//! object-safe without forcing every implementor to use `Box<dyn Future>`
//! on the hot path.
//!
//! Per design-doc D-PR1: the primary chat backend stays generic on the hot
//! path; only the optional VLM (called at most once per step inside
//! `verify_completion`) is stored as `Arc<dyn DynChatBackend>`, so the
//! per-call boxing overhead is negligible.
//!
//! Matches the real `ChatBackend` signatures (anyhow::Result, `Option<&[Value]>`
//! tools, `Result<Option<ModelInfo>>` for `fetch_model_info`).

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use serde_json::Value;

use crate::client::{ChatBackend, ChatOptions};
use crate::types::{ChatResponse, Message, ModelInfo};

/// Object-safe `dyn`-compatible mirror of [`ChatBackend`]. Returns boxed
/// futures so the trait can be used through `Arc<dyn DynChatBackend>`.
pub trait DynChatBackend: Send + Sync {
    fn chat_with_options_boxed<'a>(
        &'a self,
        messages: &'a [Message],
        tools: Option<&'a [Value]>,
        options: &'a ChatOptions,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send + 'a>>;

    fn chat_boxed<'a>(
        &'a self,
        messages: &'a [Message],
        tools: Option<&'a [Value]>,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send + 'a>>;

    fn fetch_model_info_boxed<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ModelInfo>>> + Send + 'a>>;

    fn model_name(&self) -> &str;
}

impl<B: ChatBackend + Send + Sync> DynChatBackend for B {
    fn chat_with_options_boxed<'a>(
        &'a self,
        messages: &'a [Message],
        tools: Option<&'a [Value]>,
        options: &'a ChatOptions,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send + 'a>> {
        Box::pin(self.chat_with_options(messages, tools, options))
    }

    fn chat_boxed<'a>(
        &'a self,
        messages: &'a [Message],
        tools: Option<&'a [Value]>,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send + 'a>> {
        Box::pin(self.chat(messages, tools))
    }

    fn fetch_model_info_boxed<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ModelInfo>>> + Send + 'a>> {
        Box::pin(self.fetch_model_info())
    }

    fn model_name(&self) -> &str {
        ChatBackend::model_name(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatResponse, Message};
    use std::sync::Arc;

    #[derive(Default)]
    struct StubBackend;

    impl ChatBackend for StubBackend {
        fn model_name(&self) -> &str {
            "stub-model"
        }

        async fn chat_with_options(
            &self,
            _messages: &[Message],
            _tools: Option<&[Value]>,
            _options: &ChatOptions,
        ) -> Result<ChatResponse> {
            Ok(ChatResponse {
                id: "test".to_string(),
                choices: vec![crate::types::Choice {
                    index: 0,
                    message: Message::assistant("hi"),
                    finish_reason: Some("stop".to_string()),
                }],
                usage: None,
            })
        }
    }

    #[tokio::test]
    async fn arc_dyn_dispatch_works() {
        let backend: Arc<dyn DynChatBackend> = Arc::new(StubBackend);
        let opts = ChatOptions::default();
        let resp = backend
            .chat_with_options_boxed(&[], None, &opts)
            .await
            .expect("boxed call ok");
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(backend.model_name(), "stub-model");
    }
}
