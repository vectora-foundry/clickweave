---
name: check-docs-freshness
description: Verify reference docs match the current codebase by checking only what changed since each doc's last verified commit. Checks file paths, commands, events, types, and defaults.
disable-model-invocation: true
---

# Check Docs Freshness

Verify that `docs/reference/` is still accurate by checking only what changed since each doc was last verified.

## Process

1. **For each reference doc** in `docs/reference/`, read the `Verified at commit:` sha from the top of the file.

2. **Get the diff since that commit:**
   ```bash
   git diff <verified_sha>..HEAD --name-only
   ```

3. **Filter to relevant changes** using this mapping:

   | Reference doc | Trigger paths |
   |---|---|
   | `docs/reference/architecture/overview.md` | `src-tauri/src/commands/`, `crates/clickweave-core/src/workflow.rs`, `node_types.rs`, `control_flow.rs`, `checks.rs`, `validation.rs`, `storage.rs` |
   | `docs/reference/engine/execution.md` | `crates/clickweave-engine/src/executor/`, `crates/clickweave-core/src/context.rs` |
   | `docs/reference/llm/planning-retries.md` | `crates/clickweave-llm/src/planner/`, `crates/clickweave-llm/src/client.rs`, `crates/clickweave-llm/src/types.rs` |
   | `docs/reference/mcp/integration.md` | `crates/clickweave-mcp/src/`, `crates/clickweave-core/src/tool_mapping.rs` |
   | `docs/reference/frontend/architecture.md` | `ui/src/store/`, `ui/src/components/`, `ui/src/App.tsx`, `ui/src/bindings.ts` |

   If no trigger paths changed for a doc, skip it — it's still fresh.

4. **For each doc with relevant changes**, read the actual diff content:
   ```bash
   git diff <verified_sha>..HEAD -- <trigger_paths>
   ```
   Then read the reference doc and check whether any claims are now wrong:
   - **File paths** that no longer exist
   - **Function/struct/enum names** that were renamed or removed
   - **Tauri commands** added, removed, or renamed
   - **Event names** changed
   - **Default values** changed
   - **Tables** (module tables, command tables, settings tables) missing new entries

5. **Report findings** as a table:

   | Doc | Finding | Type |
   |-----|---------|------|
   | `docs/reference/engine/execution.md` | `run_loop.rs` renamed to `runner.rs` | Broken path |
   | `docs/reference/architecture/overview.md` | New command `export_workflow` not listed | Missing |
   | `docs/reference/frontend/architecture.md` | `vlmEnabled` default changed to `true` | Stale |

   If all docs are fresh, say so.

6. **If the user asks to fix**, update the affected reference docs and bump `Verified at commit:` to `git rev-parse --short HEAD`. Stage with `git add`.

## Important

- Only check `docs/reference/`, not `docs/concepts/`.
- Report findings first, don't auto-fix.
- Focus on **documented public surfaces**. Internal refactors that don't affect any claim in the doc are not findings.
- If the `Verified at commit:` sha doesn't exist in the repo (e.g., rebased away), fall back to checking the full doc against current source.
