# Adapters

| Harness | Plugin root | Marketplace fixture | Hooks | MCP | Status |
| --- | --- | --- | --- | --- | --- |
| Codex | `plugins/context-manager` | `.agents/plugins/marketplace.json` | `hooks/hooks.json` | `.mcp.json` | local plugin fixture |
| Claude Code | `adapters/claude-code` | `.claude-plugin/marketplace.json` | `hooks/hooks.json` | `.mcp.json` | local plugin fixture |

## Shared behavior

Both adapters:

- ship the same `context-manager` skill
- call the same stable wrapper contracts
- inject context at `SessionStart`
- keep `SessionEnd` cleanup fail-open
- rely on `context-mcp` for `compose_context`, `search_context`, and `commit_work`
- resolve source-installed binaries through `CONTEXT_MANAGER_BIN_DIR`, `PATH`, or
  `~/.local/bin`, with explicit per-binary overrides still taking precedence
- report persistence outcome counts instead of claiming a write succeeded without confirmation

See the adapter-specific READMEs under `adapters/codex/` and `adapters/claude-code/` for install commands.

Use `contextctl doctor` for end-to-end health. The existence of a harness configuration directory
alone is not sufficient to mark an adapter healthy.

## Any other coding or non-coding harness

The storage protocol is not tied to either plugin. Any MCP-capable harness can launch:

```bash
context-mcp serve --adapter my-harness --stdio
```

It receives the same three tools and governance behavior. A harness that supports lifecycle hooks
can additionally call `contextctl hook --adapter my-harness --mode session-start ...` using the
JSON hook shape documented by its host. Harnesses without hooks should call `compose_context` at
the beginning of work and `commit_work` once after successful durable work.

Direct CLI integrations can use `contextctl compose`, `contextctl search`, and
`contextctl commit-work --file request.json` over the same daemon and SQLite store.
