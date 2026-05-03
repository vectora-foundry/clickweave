mod app_debug;
pub(crate) mod ax;
pub(crate) mod best_effort;
pub(crate) mod cdp;
mod cdp_actions;
mod chrome;
mod click;
mod dispatch;
mod generic;
mod helpers;
mod hover;
mod hover_dispatch;
mod resolve;
pub(crate) mod tool_result;
mod window;

pub(crate) use best_effort::best_effort_tool_call;
pub(crate) use tool_result::ToolResult;

use helpers::{
    ClickResolution, GenericCallHints, cdp_pages_show_navigation_progress, is_return_key,
    kill_chrome_profile_instance, launch_chrome_with_profile,
    launch_chrome_with_profile_and_debug_port, looks_like_browser_url_input,
    parse_cdp_page_payloads, select_best_window, truncate_for_error,
};

use super::retry_context::RetryContext;
use super::{ExecutorError, ExecutorResult, Mcp, WorkflowExecutor};
use clickweave_core::AppKind;
use clickweave_core::output_schema::NodeContext;
use clickweave_core::{
    FocusTarget, FocusWindowParams, NodeRun, NodeType, ScreenshotMode, TakeScreenshotParams,
    tool_mapping,
};
use clickweave_llm::ChatBackend;
use clickweave_mcp::ToolCallResult;
use serde_json::Value;
use uuid::Uuid;

impl<C: ChatBackend> WorkflowExecutor<C> {
    pub(crate) fn check_tool_error(result: &ToolCallResult, tool_name: &str) -> ExecutorResult<()> {
        if result.is_error == Some(true) {
            let error_text = crate::cdp_lifecycle::extract_text(result);
            return Err(ExecutorError::ToolCall {
                tool: tool_name.to_string(),
                message: error_text,
            });
        }
        Ok(())
    }

    /// Store the tool result for supervision (preserving the raw text that
    /// the supervisor prompt quotes back to the LLM), then return the
    /// legacy [`Value`] shape that downstream variable extraction expects.
    ///
    /// Call sites assemble a [`ToolResult`] via [`ToolResult::from_text`]
    /// so the text-to-JSON parse happens exactly once per tool invocation;
    /// this helper is the lone seam where the executor hands that pair
    /// back to [`RetryContext`] for supervision to re-use.
    fn set_tool_result_and_parse(
        retry_ctx: &mut RetryContext,
        result: ToolResult,
    ) -> ExecutorResult<Value> {
        retry_ctx.last_tool_result = Some(result.raw_text().to_string());
        Ok(result.into_value())
    }
}
