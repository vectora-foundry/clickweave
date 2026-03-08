# MCP Integration (Conceptual)

MCP is the runtime boundary between Clickweave and external automation capabilities.

## Role of MCP in the System

- Clickweave does not directly automate OS/browser surfaces.
- It delegates concrete operations to MCP server subprocesses, communicating via JSON-RPC 2.0 over stdio (stdin/stdout pipes).
- Multiple servers are managed by `McpRouter`, which merges tool lists and routes `call_tool` requests to the owning server.
- The primary server (`native-devtools-mcp`) is always spawned. CDP servers (`chrome-devtools-mcp`) are spawned lazily per-app when an Electron or Chrome-family app is targeted.
- The executor stays focused on orchestration, retries, and state.

## Lifecycle Model

There are two distinct spawn lifecycles:

- **Planning**: The primary MCP server is spawned briefly to fetch tool schemas (`tools_as_openai()` converts MCP tool definitions to OpenAI function-calling format for use in LLM prompts), then torn down immediately.
- **Execution**: The primary MCP server is spawned at the start of a workflow run. Additional CDP servers may be spawned lazily during the run when the executor encounters Electron or Chrome-family apps. All servers stay alive for tool calls during the graph walk and are terminated when the run completes (via Rust `Drop`, which ensures cleanup even on errors).

Within each lifecycle: initialize the connection, query available tools and schemas, call tools as needed, tear down.

## Design Benefits

- Backend stays provider-agnostic at the tool layer.
- Tool schemas are automatically converted to LLM-consumable format for planning and agentic steps.
- Request/response pairs are serialized (`io_lock`), so tool calls are safe from concurrent callers.
- Failures in external automation are isolated at a clear process boundary.

For protocol and exact command behavior, see `docs/reference/mcp/integration.md`.
