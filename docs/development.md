# Development

## Edit order

1. update canonical shared assets in `adapters/shared/`
2. run `./scripts/sync-shared-assets.sh`
3. update adapter-specific manifests, hook configs, and marketplace fixtures
4. run `./scripts/validate-adapters.sh`
5. run `./scripts/e2e-smoke.sh` when adapter contracts, hooks, or MCP launch paths change

## Tooling expectations

- Rust 1.85+ for `context-core`, `context-client`, `contextd`, `contextctl`, and `context-mcp`
- Rust 1.88+ for the current Tauri desktop dependency graph
- `python3` (plus `pyyaml` if you want the optional Codex plugin validator)
- `claude` CLI with plugin validation support
- `jq`
- `lsof` for the isolated installer lifecycle test
- POSIX `sh`

## Binary paths during development

- source development uses local builds such as `cargo build`, `cargo build --bins`, or `make build-rust`
- `./scripts/install-local.sh --debug` copies a coherent local binary set into `~/.local/bin` and
  verifies any explicitly selected adapters
- the desktop bundle path is reserved for local-architecture sidecars under `/Applications/Universal Context Manager.app/Contents/MacOS/`
- adapter wrappers honor `CONTEXT_MANAGER_BIN_DIR`, explicit `CONTEXTCTL_BIN` and
  `CONTEXT_MCP_BIN` overrides, `PATH`, and `~/.local/bin`

## Why the repo keeps copies instead of symlinks

Both Codex and Claude Code package plugin roots independently. Keeping copied, validated files inside each plugin root avoids depending on symlink resolution or external paths during installation.

## Stable contracts targeted here

- hooks call `contextctl hook`
- plugin-scoped MCP launchers call `context-mcp`

Those contracts are implemented in this repository. Update the wrapper scripts, hook fixtures, and docs together whenever the contract changes.
