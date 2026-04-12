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
  - `engine/execution.md` — executor flow, control-flow semantics, retries, variable extraction, caches
  - `llm/planning-retries.md` — prompt structure, retry layers, planner/patcher/assistant pipelines
  - `frontend/architecture.md` — React stack, directory layout, Zustand slices, graph editor behavior
  - `mcp/integration.md` — MCP client lifecycle, tool mapping, protocol types

## Planner Eval
- **Location:** `crates/clickweave-llm/eval/` — config, cases, results
- **Binary:** `cargo run -p clickweave-llm --features eval --bin planner_eval`
- **Purpose:** Measure planner prompt quality by running real user prompts through the LLM pipeline and scoring the generated workflows against structural expectations
- **Workflow:** Set eval case expectations to what you **want** the planner to produce → run evals → find failures → iterate on the system prompt (`crates/clickweave-llm/prompts/planner.md`) to fix failures → re-run. Do NOT relax expectations to make failing cases pass — use failures to improve the prompt.
- **Cases:** TOML files in `eval/cases/`, each with a user prompt and `[expect]` block (min/max nodes, required tools, required patterns like `loop`/`conditional`/`verification`)
- **Config:** `eval/eval.toml` — LLM endpoint, model, prompt template path, runs per case
- **Flags:** `--case <name>` (substring filter), `--runs N`, `--model`, `--prompt`, `--concurrency N`
- **Results:** JSON files in `eval/results/` with full generated workflows for manual analysis

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
