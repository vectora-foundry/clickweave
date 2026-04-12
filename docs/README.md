# Documentation Map

Clickweave docs are split into two layers:

## 1) Reference Docs (`docs/reference/*`)

Use these when you need exact, code-coupled truth.

- Target audience: agents, implementers, reviewers
- Characteristics: file paths, exact event payloads, command contracts, defaults
- Freshness: each file includes `Verified at commit: <sha>`

## 2) Conceptual Docs (`docs/concepts/*`)

Use these when you need system understanding and design intent.

- Target audience: humans
- Characteristics: architecture mental models, rationale, tradeoffs, visual diagrams
- Stability: less coupled to file-level details

Each conceptual doc includes a `.drawio.png` diagram (editable in draw.io via the embedded XML). The `.drawio` source files live alongside the PNGs in each subdirectory.

## Area Index

| Area | Reference | Conceptual |
|------|-----------|------------|
| Architecture | `docs/reference/architecture/overview.md` | `docs/concepts/architecture/overview.md` |
| Engine Execution | `docs/reference/engine/execution.md` | `docs/concepts/engine/execution.md` |
| MCP Integration | `docs/reference/mcp/integration.md` | `docs/concepts/mcp/integration.md` |
| Frontend Architecture | `docs/reference/frontend/architecture.md` | `docs/concepts/frontend/architecture.md` |
| Verification | `docs/verification/node-checks.md` | n/a |

## Editing Guidance

- If behavior, APIs, paths, or payloads change: update reference docs in the same PR.
- If intent/mental model evolves: update conceptual docs.
- If both changed, update both layers.
