# MCP Integration (Reference)

Verified at commit: `57e04e0`

Clickweave executes desktop/browser automation by spawning a single MCP server subprocess (`native-devtools-mcp`) and talking JSON-RPC over stdio via `McpClient`.

## Architecture

```
clickweave-engine
    |
    v
clickweave-mcp::McpClient  <--- JSON-RPC --->  native-devtools-mcp
```

A single `McpClient` handles both native desktop tools (`click`, `find_text`, etc.) and CDP browser tools (`cdp_connect`, `cdp_click`, `cdp_find_elements`, `cdp_take_dom_snapshot`, `cdp_type_text`, etc.). The client caches the server's `tools/list` response and can refresh that cache after operations such as `cdp_connect`.

## McpClient Lifecycle

File: `crates/clickweave-mcp/src/client.rs`

### Spawn Modes

| Method | Behavior |
|--------|----------|
| `McpClient::spawn(cmd, args)` | runs provided command and args directly |

`spawn_native()` was removed. Callers now use `McpClient::spawn(path, &[])` with a binary path resolved by `mcp_resolve::resolve_mcp_binary()` in the Tauri command layer.

### Initialization Sequence

1. Spawn subprocess with piped stdin/stdout
2. Send `initialize` request
3. Parse initialize response
4. Send `notifications/initialized` notification
5. Send `tools/list`
6. Cache tool schemas in client

### Tool Calls

`call_tool(name, arguments)` sends `tools/call` and returns `ToolCallResult`.

`has_tool(name)` checks whether a tool is in the cached tool list (used to gate CDP paths when the server lacks CDP support).

`ToolCallResult.content` items are:

- `text` (`{ type: "text", text }`)
- `image` (`{ type: "image", data, mimeType }`)
- unknown content types are preserved as raw JSON

If `is_error == true`, higher layers treat it as a tool failure.

### CDP Connection

For Electron/Chrome apps, Clickweave connects to the app's remote debugging port before CDP-backed dispatch:

- `cdp_connect({"port": N})` — connect to app's remote debugging port
- `cdp_disconnect` — disconnect before switching to a different app

Only one CDP connection at a time is supported; switching apps requires disconnect/reconnect. The deterministic executor ensures a live CDP session before CDP node dispatch. The state-spine agent runner auto-connects after successful `launch_app` / `focus_window` calls for Electron/Chrome targets and then refreshes the client-side tool cache so `has_tool()` reflects server-side availability.

The agent's LLM-visible tool list is stable for the run. `run_agent_workflow` seeds it once from `mcp.tools_as_openai()`, and `StateRunner::run` appends only the harness-local completion/replan pseudo-tools. Later `refresh_server_tool_list()` calls update `has_tool()` for observation gates, not the list passed to the LLM.

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

`tools_as_openai()` (and the free function `tools_to_openai()` in `protocol.rs`) converts MCP tool definitions to OpenAI function-calling schema:

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

Used by the deterministic executor and by the agent entry point when seeding the LLM-visible MCP tool surface at run start.

## State-Spine Agent MCP Use

Files:

- `crates/clickweave-engine/src/agent/mod.rs` — seeds `mcp.tools_as_openai()` once per run
- `crates/clickweave-engine/src/agent/runner.rs` — state-spine control loop and MCP dispatch
- `crates/clickweave-engine/src/agent/prompt.rs` — stable system prompt and per-turn state block
- `crates/clickweave-engine/src/agent/context.rs` — transcript compaction

Per step, `StateRunner::run` observes the page with `cdp_find_elements` when `has_tool("cdp_find_elements")` is true, mirrors the response into `WorldModel.elements` and `WorldModel.cdp_page`, renders `<world_model>` / `<task_state>` into the next user turn, then dispatches exactly one `AgentAction`.

Snapshot bodies are budget-managed by `context::compact`. It preserves `messages[0]` (system prompt) and `messages[1]` (goal block), folds superseded snapshot-family results across `cdp_take_dom_snapshot`, `cdp_find_elements`, and `take_ax_snapshot`, and omits copied snapshot observation bodies from prior user turns. Continuity data lives in `WorldModel.last_screenshot` and `WorldModel.last_native_ax_snapshot`; AX descriptor enrichment reads the native AX snapshot body from `WorldModel`, not by walking the transcript.

## NodeType <-> Tool Mapping

File: `crates/clickweave-core/src/tool_mapping.rs`

### NodeType -> Tool invocation

| NodeType | Tool | Notes |
|----------|------|-------|
| `TakeScreenshot` | `take_screenshot` | `mode`, `include_ocr`, optional `app_name` |
| `FindText` | `find_text` | `text`, optional `app_name` (from `FindTextParams.scope`) |
| `FindImage` | `find_image` | `template_image_base64`, `threshold`, `max_results` |
| `Click` | `click` | coordinates/button/count |
| `Hover` | `move_mouse` | optional `x`, `y` |
| `TypeText` | `type_text` | `text` |
| `PressKey` | `press_key` | `key`, optional `modifiers` |
| `Scroll` | `scroll` | `delta_y`, optional `x`,`y` |
| `FocusWindow` | `focus_window` | one of `app_name` / `window_id` / `pid`; optional `app_kind` |
| `McpToolCall` | dynamic | pass-through tool + args |

`AppDebugKitOp` is executed in the engine's deterministic path directly (not through `tool_mapping`).

### Tool invocation -> NodeType

Known MCP tool names map back to typed nodes.
Unknown tool names map to `McpToolCall` only if present in known tool schema list; otherwise `UnknownTool` error.

## Configuration

The MCP binary path is resolved automatically by `mcp_resolve::resolve_mcp_binary()` in the Tauri command layer — no user-facing `mcpCommand` setting is required. The resolved path is passed directly to `McpClient::spawn(path, &[])`.

Relevant files:

- `src-tauri/src/mcp_resolve.rs`
- `src-tauri/src/commands/agent.rs` — spawns the MCP client for the agent loop
- `src-tauri/src/commands/executor.rs` — spawns the MCP client for deterministic workflow execution
- `crates/clickweave-engine/src/agent/runner.rs` — state-spine runner that dispatches MCP tools step by step from agent decisions
- `crates/clickweave-engine/src/executor/run_loop.rs` — dispatches MCP tools node by node for saved workflows

## App Detection

File: `crates/clickweave-core/src/app_detection.rs`

During walkthrough recording, apps are classified as `Native`, `ChromeBrowser`, or `ElectronApp` when they receive focus. This classification is emitted on `AppFocused` events via the `app_kind` field.

### Detection Strategy

| App Type | Detection Method | Maintenance |
|----------|-----------------|-------------|
| Chrome-family | Bundle ID matching (6 entries) | Rarely changes |
| Electron apps | Framework directory check | Zero — automatic |
| Native apps | Default (neither matches) | N/A |

**Chrome-family**: matched by bundle ID (`com.google.Chrome`, `com.google.Chrome.canary`, `com.brave.Browser`, `com.microsoft.edgemac`, `company.thebrowser.Browser`, `org.chromium.Chromium`).

**Electron**: detected by checking for `Contents/Frameworks/Electron Framework.framework` (macOS) or `resources\electron.asar` (Windows) in the app bundle. Uses `proc_pidpath` (macOS) to resolve PID → bundle path.

### Reactive Fallback

If proactive detection classifies an app as `Native` but accessibility enrichment returns no actionable element, the framework check is re-run. This catches Electron apps with unusual bundle structures.

### Key Files

| File | Role |
|------|------|
| `crates/clickweave-core/src/app_detection.rs` | `classify_app`, `bundle_path_from_pid`, Chrome/Electron checks |
| `crates/clickweave-core/src/walkthrough/types.rs` | `AppKind` enum, `AppFocused` event |
| `src-tauri/src/commands/walkthrough_session.rs` | Event loop integration, reactive fallback, CDP setup |

## Key Files

| File | Role |
|------|------|
| `crates/clickweave-mcp/src/client.rs` | McpClient: spawn, init, tools/list, tools/call |
| `crates/clickweave-mcp/src/protocol.rs` | protocol data types |
| `crates/clickweave-mcp/src/lib.rs` | re-exports |
| `crates/clickweave-core/src/tool_mapping.rs` | shared node/tool mapping |
| `crates/clickweave-core/src/app_detection.rs` | Electron/Chrome app classification |
