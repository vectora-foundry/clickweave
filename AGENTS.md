## Run Trace Logs
- **Saved projects:** `<project>/.clickweave/runs/<workflow_name>/<execution_dir>/<node_name>/`
- **Unsaved projects (macOS):** `~/Library/Application Support/com.clickweave.app/runs/<workflow_name>_<short_uuid>/<execution_dir>/<node_name>/`
- **Unsaved projects (Windows):** `%APPDATA%\Clickweave\runs\<workflow_name>_<short_uuid>\<execution_dir>\<node_name>\`
- **Unsaved projects (Linux):** `$XDG_DATA_HOME/clickweave/runs/<workflow_name>_<short_uuid>/<execution_dir>/<node_name>/`
- `<workflow_name>` — sanitized workflow name (lowercase, dashes)
- `<execution_dir>` — `YYYY-MM-DD_HH-MM-SS_<short_uuid>` per workflow execution
- `<node_name>` — sanitized node name (e.g., `launch-calculator/`)
- `events.jsonl` — newline-delimited trace events (node_started, tool_call, tool_result)
- `run.json` — run metadata (status, timestamps, trace_level)
- `artifacts/` — output artifacts from the run
- **When debugging runtime issues**, always check the most recent run logs first — read `events.jsonl` for each node to understand the actual execution flow before proposing fixes

## Walkthrough Session Logs
- **Saved projects:** `<project>/.clickweave/walkthroughs/<session_dir>/`
- **Unsaved projects (macOS):** `~/Library/Application Support/com.clickweave.app/walkthroughs/<session_dir>/`
- **Unsaved projects (Windows):** `%APPDATA%\Clickweave\walkthroughs\<session_dir>\`
- **Unsaved projects (Linux):** `$XDG_DATA_HOME/clickweave/walkthroughs/<session_dir>/`
- `session.json` — session metadata
- `events.jsonl` — raw walkthrough events
- `actions.json` — extracted actions
- `draft.json` — generated workflow draft
- `artifacts/` — screenshots and other captured artifacts

## Application Logs
- **macOS:** `~/Library/Logs/Clickweave/clickweave.YYYY-MM-DD.txt`
- **Windows:** `%LOCALAPPDATA%\Clickweave\logs\clickweave.YYYY-MM-DD.txt`
- **Linux:** `$XDG_DATA_HOME/clickweave/logs/clickweave.YYYY-MM-DD.txt` (fallback: `~/.local/share/clickweave/logs/`)
- JSON-formatted, daily rotation
- Configured in `src-tauri/src/main.rs` (`log_dir()` + tracing subscriber setup)

## Reference Docs
- `docs/reference/` — **read these first** when exploring a subsystem, before doing broad searches
  - `architecture/overview.md` — crate structure, dependency graph, module tables, IPC commands, event contract
  - `engine/execution.md` — deterministic executor flow, agent loop structure, retries, caches, supervision
  - `frontend/architecture.md` — React stack, directory layout, Zustand slices (including agent slice), graph editor behavior
  - `mcp/integration.md` — MCP client lifecycle, tool mapping, protocol types

## Design & Implementation Plans
- Location: `internal_docs/plans/` (gitignored, local-only)
- Naming: `YYYY-MM-DD_HH-MM-SS-<topic>.md` (e.g., `2026-02-12_10-07-02-app-name-resolution.md`)

## Rust Development

### Code Style
- Follow the Rust style guide as outlined in [rustfmt](https://github.com/rust-lang/rustfmt)
- Use 4 spaces for indentation
- Sort imports alphabetically

### Tooling
- Use `cargo clippy` for linting
- Use `cargo fmt` for formatting

### Error Handling
- Use Result types that are used in the file you're editing for functions that can fail
- Avoid using `unwrap()` or `expect()` in production code

### Build & Run
```bash
cargo build
cargo run
```

### Preferred Patterns
- Use traits for polymorphism
- Leverage Rust's ownership system for memory safety
- Use iterators and closures for data transformation
- Pin dependency versions for reproducible builds
