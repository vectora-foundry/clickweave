use super::super::Mcp;
use serde_json::Value;

/// Invoke an MCP tool whose result we don't care about, but whose failure we
/// want visible in the trace.
///
/// Used for teardown and bookkeeping calls (e.g. `cdp_disconnect`, force
/// `quit_app`) where the executor has no meaningful recovery path but a
/// silent drop would hide diagnostics for bugs like "CDP connected to the
/// wrong profile" or "old Chrome still alive".
///
/// The `context` string is prefixed into the debug log so post-mortems can
/// pinpoint which teardown path produced the failure.
pub(crate) async fn best_effort_tool_call(
    mcp: &(impl Mcp + ?Sized),
    tool: &str,
    args: Option<Value>,
    context: &str,
) {
    if let Err(e) = mcp.call_tool(tool, args).await {
        tracing::debug!(
            tool = tool,
            context = context,
            error = %e,
            "best-effort MCP tool call failed (continuing)",
        );
    }
}
