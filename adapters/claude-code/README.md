# Claude Code adapter

The Claude Code adapter lives in `adapters/claude-code` and is published through the repo marketplace file at `.claude-plugin/marketplace.json`.

## What is bundled

- `.claude-plugin/plugin.json`
- shared `context-manager` skill
- `hooks/hooks.json` with fail-open shell shims
- plugin-scoped `.mcp.json` for `context-mcp`

## Install locally

From the repository root:

```bash
claude plugin marketplace add .
claude plugin install context-manager@universal-context-manager-local
```

After updates, reload plugins or start a new Claude Code session.

## Runtime behavior

- `SessionStart` hooks inject composed global/project/task context into Claude Code.
- `SessionEnd` hooks stay fail-open and perform cleanup without blocking the session.
- the plugin-scoped MCP launcher starts `context-mcp` over stdio for `compose_context`, `search_context`, and `commit_work`.
- the shared skill tells the harness to call `commit_work` once after successful durable work rather than on every prompt or tool call.
