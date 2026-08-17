# Install and onboarding

## Codex

From the repository root:

```bash
codex plugin marketplace add .
codex plugin add context-manager@universal-context-manager-local
```

Optional user-side MCP policy examples live in `adapters/codex/config.example.toml`.

## Claude Code

From the repository root:

```bash
claude plugin marketplace add .
claude plugin install context-manager@universal-context-manager-local
```

Reload plugins or start a new session after updates.

## First-session expectations

- the skill is available immediately after install
- `SessionStart` hooks inject the composed context and remind the harness to call `commit_work` after successful durable work
- hooks do not block the session if the local backend is unavailable
- in source development, build the Rust binaries locally (`cargo build` or `make build-rust`)
- in a Tauri bundle, the app prepares local-architecture sidecars for `contextd`, `contextctl`, and `context-mcp`
- harness wrappers accept explicit `CONTEXTCTL_BIN` / `CONTEXT_MCP_BIN` overrides and otherwise fall back to `PATH` or `/Applications/Universal Context Manager.app/Contents/MacOS/...`
- no signed releases or packaged binaries are claimed by this repository today
