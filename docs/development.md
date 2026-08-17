# Development

## Edit order

1. update canonical shared assets in `adapters/shared/`
2. run `./scripts/sync-shared-assets.sh`
3. update adapter-specific manifests, hook configs, and marketplace fixtures
4. run `./scripts/validate-adapters.sh`
5. run `./scripts/e2e-smoke.sh` when adapter contracts, hooks, or MCP launch paths change

## Tooling expectations

- `python3` (plus `pyyaml` if you want the optional Codex plugin validator)
- `claude` CLI with plugin validation support
- `jq`
- POSIX `sh`

## Binary paths during development

- source development uses local builds such as `cargo build`, `cargo build --bins`, or `make build-rust`
- the desktop bundle path is reserved for local-architecture sidecars under `/Applications/Universal Context Manager.app/Contents/MacOS/`
- adapter wrappers also honor explicit `CONTEXTCTL_BIN` and `CONTEXT_MCP_BIN` overrides

## Why the repo keeps copies instead of symlinks

Both Codex and Claude Code package plugin roots independently. Keeping copied, validated files inside each plugin root avoids depending on symlink resolution or external paths during installation.

## Stable contracts targeted here

- hooks call `contextctl hook`
- plugin-scoped MCP launchers call `context-mcp`

Those contracts are implemented in this repository. Update the wrapper scripts, hook fixtures, and docs together whenever the contract changes.
