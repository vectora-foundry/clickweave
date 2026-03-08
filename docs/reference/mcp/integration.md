# MCP Integration (Reference)

Verified at commit: `d0fd809`

Clickweave executes desktop/browser automation by spawning MCP server subprocesses and talking JSON-RPC over stdio. Multiple servers are managed by `McpRouter`, which merges tool lists and routes `call_tool` requests to the owning server.

## Architecture

```
clickweave-engine
    |
    v
clickweave-mcp::McpRouter
    |
    +--- McpClient  <--- JSON-RPC --->  native-devtools-mcp  (primary, always)
    |
    +--- McpClient  <--- JSON-RPC --->  chrome-devtools-mcp   (spawned lazily per CDP app)
```

## McpRouter

File: `crates/clickweave-mcp/src/router.rs`

### McpServerConfig

```rust
pub struct McpServerConfig {
    pub name: String,      // display name (e.g. "native-devtools")
    pub command: String,    // binary or "npx"
    pub args: Vec<String>,  // e.g. ["-y", "native-devtools-mcp"]
}
```

### Spawn & Tool Routing

`McpRouter::spawn(configs)` spawns all configured servers concurrently via `JoinSet`:

- **Primary server** (index 0): failure is fatal — returns `Err`.
- **Non-primary servers**: failure logs a warning and continues without that server.

After spawning, the router builds a merged tool list. On tool-name conflicts, **first server wins** — the duplicate is logged and skipped.

### default_server_configs

`default_server_configs(mcp_command)` builds a single-server config (native-devtools only). CDP servers are spawned lazily per-app by the executor via `spawn_server()`.

| Server | Command | Args |
|--------|---------|------|
| `native-devtools` | `mcp_command` (or `npx` with `-y native-devtools-mcp`) | varies |

### Key Methods

| Method | Behavior |
|--------|----------|
| `spawn(configs)` | Spawn all servers, build routing table |
| `call_tool(name, args)` | Route to owning server |
| `tools()` | Merged tool list |
| `tools_as_openai()` | OpenAI function-calling format |
| `server_count()` | Number of active servers |
| `kill_all()` | Kill all server processes |
| `call_tool_on(server, name, args)` | Route to a specific server by name |
| `has_server(name)` | Check whether a named server is connected |
| `spawn_server(config)` | Spawn a single MCP server at runtime and add to routing table |

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
| `FocusWindow` | `focus_window` | one of `app_name` / `window_id` / `pid`; optional `app_kind` |
| `McpToolCall` | dynamic | pass-through tool + args |

Returns `NotAToolNode` for control-flow nodes and `AiStep`.
`AppDebugKitOp` is executed in engine deterministic path directly (not through `tool_mapping`).

### Tool invocation -> NodeType

Known MCP tool names map back to typed nodes.
Unknown tool names map to `McpToolCall` only if present in known tool schema list; otherwise `UnknownTool` error.

## Configuration

UI settings store `mcpCommand`:

- `"npx"` => `default_server_configs("npx")` spawns native-devtools via npx
- any other string => used as direct command for native-devtools

The `mcpCommand` string is converted to `Vec<McpServerConfig>` via `default_server_configs()` in both the planner and executor Tauri commands.

Relevant files:

- `ui/src/store/settings.ts`
- `ui/src/components/SettingsModal.tsx`
- `src-tauri/src/commands/planner.rs`
- `src-tauri/src/commands/executor.rs`
- `crates/clickweave-engine/src/executor/run_loop.rs`

## App Detection

File: `crates/clickweave-core/src/app_detection.rs`

During walkthrough recording, apps are classified as `Native`, `ChromeBrowser`, or `ElectronApp` when they receive focus. This classification is emitted on `AppFocused` events via the `app_kind` field.

### Detection Strategy

| App Type | Detection Method | Maintenance |
|----------|-----------------|-------------|
| Chrome-family | Bundle ID matching (6 entries) | Rarely changes |
| Electron apps | Framework directory check | Zero — automatic |
| Native apps | Default (neither matches) | N/A |

**Chrome-family**: matched by bundle ID (`com.google.Chrome`, `com.brave.Browser`, `com.microsoft.edgemac`, `company.thebrowser.Browser`, `org.chromium.Chromium`).

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
| `crates/clickweave-mcp/src/router.rs` | McpRouter, McpServerConfig, default_server_configs |
| `crates/clickweave-mcp/src/client.rs` | spawn, init, tools/list, tools/call |
| `crates/clickweave-mcp/src/protocol.rs` | protocol data types |
| `crates/clickweave-mcp/src/lib.rs` | re-exports |
| `crates/clickweave-core/src/tool_mapping.rs` | shared node/tool mapping |
| `crates/clickweave-core/src/app_detection.rs` | Electron/Chrome app classification |
