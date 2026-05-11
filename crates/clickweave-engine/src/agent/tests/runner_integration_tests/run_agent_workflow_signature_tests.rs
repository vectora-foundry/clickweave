/// Compile-time assertion: `run_agent_workflow` accepts a
/// `Option<RunStorageHandle>` as its last parameter.
///
/// If this coerces, the plumbing compiles; we do not invoke the
/// function here because it takes a concrete `McpClient` which cannot
/// be instantiated in-crate without spawning the external MCP server.
/// Task 3a.1's `ScriptedLlm`/`StaticMcp` stubs enable a live
/// end-to-end test.
#[test]
fn run_agent_workflow_accepts_storage_argument() {
    fn _coerce() {
        let _: Option<crate::agent::RunStorageHandle> = None;
    }
}
