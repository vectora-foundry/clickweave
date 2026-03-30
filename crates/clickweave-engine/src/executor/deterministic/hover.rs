use super::super::Mcp;
use super::super::retry_context::RetryContext;
use super::super::{ExecutorError, ExecutorResult, WorkflowExecutor};
use clickweave_core::{ClickTarget, HoverParams, NodeRun, NodeType};
use clickweave_llm::ChatBackend;
use uuid::Uuid;

impl<C: ChatBackend> WorkflowExecutor<C> {
    pub(in crate::executor) async fn resolve_hover_target(
        &self,
        node_id: Uuid,
        mcp: &(impl Mcp + ?Sized),
        params: &HoverParams,
        node_run: &mut Option<&mut NodeRun>,
        retry_ctx: &RetryContext,
    ) -> ExecutorResult<NodeType> {
        let target = params.target.as_ref().map(|t| t.text()).ok_or_else(|| {
            ExecutorError::ClickTarget("resolve_hover_target called with no target".to_string())
        })?;
        let (x, y) = self
            .resolve_target_by_text(node_id, target, mcp, node_run, retry_ctx)
            .await?;
        Ok(NodeType::Hover(HoverParams {
            target: Some(ClickTarget::Coordinates { x, y }),
            dwell_ms: params.dwell_ms,
            ..Default::default()
        }))
    }
}
