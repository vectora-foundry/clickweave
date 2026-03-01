# MCP Integration (Reference)

Verified at commit: `1cdb730`

Clickweave executes desktop/browser automation by spawning an MCP server subprocess and talking JSON-RPC over stdio.

## Architecture

```
clickweave-engine
    |
    v
clickweave-mcp::McpClient  <--- JSON-RPC over stdio --->  native-devtools-mcp
```

## McpClient Lifecycle

File: `crates/clickweave-mcp/src/client.rs`

### Spawn Modes

| Method | Behavior |
|--------|----------|
| `McpClient::spawn_npx()` | runs `npx -y native-devtools-mcp` |
| `McpClient::spawn(cmd, args)` | runs provided command and args |

### Initialization Sequence

1. Spawn subprocess with piped stdin/stdout
2. Send `initialize` request
3. Parse initialize response
4. Send `notifications/initialized` notification
5. Send `tools/list`
6. Cache tool schemas in client

### Tool Calls

`call_tool(name, arguments)` sends `tools/call` and returns `ToolCallResult`.

`ToolCallResult.content` items are:

- `text` (`{ type: "text", text }`)
- `image` (`{ type: "image", data, mimeType }`)

If `is_error == true`, higher layers treat it as a tool failure.

### Concurrency

`io_lock` serializes request/response pairs so concurrent callers cannot interleave stdio reads/writes.

### Shutdown

`Drop` calls `kill()` to terminate subprocess when client is dropped.

## Protocol Types

File: `crates/clickweave-mcp/src/protocol.rs`

Core types:

- `JsonRpcRequest`
- `JsonRpcResponse`
- `JsonRpcError`
- `InitializeParams` / `InitializeResult`
- `ToolsListResult`
- `ToolCallParams` / `ToolCallResult`

Client sends JSON-RPC 2.0 messages and parses server responses into typed structs.

## Tool Schema Conversion

`tools_as_openai()` converts MCP tool definitions to OpenAI function-calling schema:

```json
{
  "type": "function",
  "function": {
    "name": "<tool>",
    "description": "...",
    "parameters": { "type": "object", "properties": { ... } }
  }
}
```

Used by planner and AI-step flows when passing tool schemas to LLM backends.

## NodeType <-> Tool Mapping

File: `crates/clickweave-core/src/tool_mapping.rs`

### NodeType -> Tool invocation

| NodeType | Tool | Notes |
|----------|------|-------|
| `TakeScreenshot` | `take_screenshot` | `mode`, `include_ocr`, optional `app_name` |
| `FindText` | `find_text` | `text`, optional `app_name` (from `FindTextParams.scope`) |
| `FindImage` | `find_image` | `template_image_base64`, `threshold`, `max_results` |
| `Click` | `click` | coordinates/button/count |
| `TypeText` | `type_text` | `text` |
| `PressKey` | `press_key` | `key`, optional `modifiers` |
| `Scroll` | `scroll` | `delta_y`, optional `x`,`y` |
| `ListWindows` | `list_windows` | optional `app_name` |
| `FocusWindow` | `focus_window` | one of `app_name` / `window_id` / `pid` |
| `McpToolCall` | dynamic | pass-through tool + args |

Returns `NotAToolNode` for control-flow nodes and `AiStep`.
`AppDebugKitOp` is executed in engine deterministic path directly (not through `tool_mapping`).

### Tool invocation -> NodeType

Known MCP tool names map back to typed nodes.
Unknown tool names map to `McpToolCall` only if present in known tool schema list; otherwise `UnknownTool` error.

## Configuration

UI settings store `mcpCommand`:

- `"npx"` => use `spawn_npx()`
- any other string => execute as command path with no extra args

Relevant files:

- `ui/src/store/settings.ts`
- `ui/src/components/SettingsModal.tsx`
- `src-tauri/src/commands/planner.rs`
- `crates/clickweave-engine/src/executor/run_loop.rs`

## Key Files

| File | Role |
|------|------|
| `crates/clickweave-mcp/src/client.rs` | spawn, init, tools/list, tools/call |
| `crates/clickweave-mcp/src/protocol.rs` | protocol data types |
| `crates/clickweave-mcp/src/lib.rs` | re-exports |
| `crates/clickweave-core/src/tool_mapping.rs` | shared node/tool mapping |
