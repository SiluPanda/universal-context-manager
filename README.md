# Universal Context Manager

A local-first, human-governed context control plane for AI coding harnesses.

Today this repository contains a working local stack:

- `context-core`: SQLite/WAL store with layered scopes, reviews, revisions, runs, import/export, and FTS search
- `contextd`: local daemon over a Unix domain socket
- `contextctl`: CLI for compose/search/commit/review/import/export/hook dispatch
- `context-mcp`: stdio MCP server exposing `compose_context`, `search_context`, and `commit_work`
- macOS desktop app under `apps/desktop`
- Codex and Claude Code adapters plus shared skills and launcher scripts

Current boundaries:

- local-first only; no hosted sync or cloud service
- native desktop focus is macOS/Tauri
- adapter hooks intentionally stay fail-open so harness sessions do not break when the local backend is unavailable
- no signed releases or packaged binaries are published yet

## Build and run locally

Prerequisites: Rust 1.85+, Node 22+, pnpm 9+, `jq`, and macOS 13+ for the desktop app.

```bash
# Build the daemon, CLI, and MCP server.
cargo build --bins

# Start or discover the local daemon and print its paths.
CONTEXTD_BIN="$PWD/target/debug/contextd" target/debug/contextctl init

# Verify CLI, review governance, real hook output, and MCP post-work writes.
./scripts/e2e-smoke.sh

# Run the live Mac app (the Tauri command prepares its sidecars automatically).
cd apps/desktop
pnpm install
pnpm tauri dev
```

The regular `pnpm dev` command is only a browser-based visual preview and uses synthetic data;
`pnpm tauri dev` is the live local control plane.

## Governance model

- global, project, and task packs compose in that order
- safe project/task proposals auto-apply
- global, conflicting, and locked proposals enter the human review queue
- every accepted mutation creates provenance and a restorable revision
- secret-like values are rejected before persistence
- adapter hooks load context at session start; agents write concise durable updates once after work through MCP

## Quick validation

```bash
./scripts/validate-adapters.sh
```

Use `STRICT_EXTERNAL_VALIDATORS=1` to require locally installed Codex and Claude plugin
validators in addition to the portable repository checks.

## End-to-end smoke test

```bash
./scripts/e2e-smoke.sh
```

## Documentation map

- [Architecture](docs/architecture.md)
- [Threat model](docs/threat-model.md)
- [Development](docs/development.md)
- [Validation](docs/validation.md)
- [Install and onboarding](docs/install-and-onboarding.md)
- [Import and export](docs/import-export.md)
- [Adapters](docs/adapters.md)
- [Codex adapter](adapters/codex/README.md)
- [Claude Code adapter](adapters/claude-code/README.md)
