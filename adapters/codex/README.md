# Codex adapter

The Codex adapter is packaged as the local plugin fixture at `plugins/context-manager` and published through the repo marketplace file at `.agents/plugins/marketplace.json`.

## What is bundled

- shared `context-manager` skill
- `hooks/hooks.json` with fail-open shell shims
- plugin-scoped `.mcp.json` for `context-mcp`
- repo-local marketplace fixture named `universal-context-manager-local`

## Install locally

From the repository root:

```bash
codex plugin marketplace add .
codex plugin add context-manager@universal-context-manager-local
```

Optional user config is documented in [`config.example.toml`](config.example.toml).

## Runtime behavior

- `SessionStart` hooks inject composed global/project/task context into Codex.
- `SessionEnd` hooks stay fail-open and flush any explicitly supplied commit envelope or older spooled writes.
- the plugin-scoped MCP launcher starts `context-mcp` over stdio for `compose_context`, `search_context`, and `commit_work`.
- the shared skill tells the harness to call `commit_work` once after successful durable work rather than on every prompt or tool call.
