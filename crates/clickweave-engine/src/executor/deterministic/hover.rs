use super::super::{ExecutorError, ExecutorResult, WorkflowExecutor};
use clickweave_core::{HoverParams, NodeRun, NodeType};
use clickweave_llm::ChatBackend;
use clickweave_mcp::ToolProvider;
use uuid::Uuid;

impl<C: ChatBackend> WorkflowExecutor<C> {
    pub(in crate::executor) async fn resolve_hover_target(
        &self,
        node_id: Uuid,
        mcp: &(impl ToolProvider + ?Sized),
        params: &HoverParams,
        node_run: &mut Option<&mut NodeRun>,
    ) -> ExecutorResult<NodeType> {
        let target = params.target.as_ref().map(|t| t.text()).ok_or_else(|| {
            ExecutorError::ClickTarget("resolve_hover_target called with no target".to_string())
        })?;
        let (x, y) = self
            .resolve_target_by_text(node_id, target, mcp, node_run)
            .await?;
        Ok(NodeType::Hover(HoverParams {
            target: params.target.clone(),
            x: Some(x),
            y: Some(y),
            dwell_ms: params.dwell_ms,
            ..Default::default()
        }))
    }

    pub(in crate::executor) async fn resolve_hover_target_by_image(
        &self,
        _node_id: Uuid,
        mcp: &(impl ToolProvider + ?Sized),
        params: &HoverParams,
        node_run: &mut Option<&mut NodeRun>,
    ) -> ExecutorResult<NodeType> {
        let b64 = params.template_image.as_deref().ok_or_else(|| {
            ExecutorError::ClickTarget(
                "resolve_hover_target_by_image called without template_image".to_string(),
            )
        })?;
        let (x, y) = self.resolve_target_by_image(b64, mcp, node_run).await?;
        Ok(NodeType::Hover(HoverParams {
            target: params.target.clone(),
            template_image: params.template_image.clone(),
            x: Some(x),
            y: Some(y),
            dwell_ms: params.dwell_ms,
        }))
    }
}
