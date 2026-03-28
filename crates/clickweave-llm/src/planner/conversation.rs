use serde::{Deserialize, Serialize};

/// A single entry in the assistant conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ChatEntry {
    pub role: ChatRole,
    pub content: String,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_summary: Option<PatchSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_context: Option<RunContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum ChatRole {
    User,
    Assistant,
    ToolCall,
    ToolResult,
}

/// Compact summary of what a patch did (for conversation context, not the full patch).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PatchSummary {
    pub added: u32,
    pub removed: u32,
    pub updated: u32,
    #[serde(default)]
    pub added_names: Vec<String>,
    #[serde(default)]
    pub removed_names: Vec<String>,
    #[serde(default)]
    pub updated_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Execution results available at the time of a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RunContext {
    pub execution_dir: String,
    pub node_results: Vec<NodeResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct NodeResult {
    pub node_name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Persistent conversation session for a workflow.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ConversationSession {
    pub messages: Vec<ChatEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub summary_cutoff: usize,
}

const DEFAULT_WINDOW_SIZE: usize = 5;

impl ConversationSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a user message.
    pub fn push_user(&mut self, content: String, run_context: Option<RunContext>) {
        self.messages.push(ChatEntry {
            role: ChatRole::User,
            content,
            timestamp: now_epoch_ms(),
            patch_summary: None,
            run_context,
            tool_call_id: None,
            tool_name: None,
        });
    }

    /// Push an assistant message.
    pub fn push_assistant(&mut self, content: String, patch_summary: Option<PatchSummary>) {
        self.messages.push(ChatEntry {
            role: ChatRole::Assistant,
            content,
            timestamp: now_epoch_ms(),
            patch_summary,
            run_context: None,
            tool_call_id: None,
            tool_name: None,
        });
    }

    /// Index where the recent window starts, counting only User/Assistant entries.
    fn window_start_index(&self, window_size: Option<usize>) -> usize {
        let target = window_size.unwrap_or(DEFAULT_WINDOW_SIZE) * 2;
        let mut count = 0;
        for (i, entry) in self.messages.iter().enumerate().rev() {
            if matches!(entry.role, ChatRole::User | ChatRole::Assistant) {
                count += 1;
                if count >= target {
                    return i;
                }
            }
        }
        0 // fewer entries than the window — start from the beginning
    }

    /// Messages in the recent window (last N user+assistant exchanges).
    ///
    /// Counts only User and Assistant entries when determining the window
    /// boundary, so interleaved ToolCall/ToolResult entries don't shrink
    /// the effective window.
    pub fn recent_window(&self, window_size: Option<usize>) -> &[ChatEntry] {
        let start = self.window_start_index(window_size);
        &self.messages[start..]
    }

    /// Messages that have aged out of the window but haven't been summarized yet.
    pub fn unsummarized_overflow(&self, window_size: Option<usize>) -> &[ChatEntry] {
        let window_start = self.window_start_index(window_size);
        if window_start > self.summary_cutoff {
            &self.messages[self.summary_cutoff..window_start]
        } else {
            &[]
        }
    }

    /// Whether summarization is needed (overflow exists).
    pub fn needs_summarization(&self, window_size: Option<usize>) -> bool {
        !self.unsummarized_overflow(window_size).is_empty()
    }

    /// Compute the summary_cutoff value for the current message count.
    pub fn current_cutoff(&self, window_size: Option<usize>) -> usize {
        self.window_start_index(window_size)
    }

    /// Update the summary after summarization.
    pub fn set_summary(&mut self, summary: String, window_size: Option<usize>) {
        self.summary = Some(summary);
        self.summary_cutoff = self.current_cutoff(window_size);
    }
}

impl ChatEntry {
    pub fn tool_call(tool_name: &str, tool_call_id: &str, content: &str) -> Self {
        Self {
            role: ChatRole::ToolCall,
            content: content.to_string(),
            timestamp: now_epoch_ms(),
            patch_summary: None,
            run_context: None,
            tool_call_id: Some(tool_call_id.to_string()),
            tool_name: Some(tool_name.to_string()),
        }
    }

    pub fn tool_result(tool_call_id: &str, tool_name: &str, content: &str) -> Self {
        Self {
            role: ChatRole::ToolResult,
            content: content.to_string(),
            timestamp: now_epoch_ms(),
            patch_summary: None,
            run_context: None,
            tool_call_id: Some(tool_call_id.to_string()),
            tool_name: Some(tool_name.to_string()),
        }
    }
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
