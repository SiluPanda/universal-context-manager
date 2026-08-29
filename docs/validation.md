# Validation

The adapter validation path is intentionally shell-first and repository-local.

## Main command

```bash
./scripts/validate-adapters.sh
```

It runs:

1. shell syntax and help checks for the source installer and canonical launchers
2. `./scripts/sync-shared-assets.sh --check`
3. `./scripts/test-hooks.sh`, including explicit, managed-bin, and `~/.local/bin` discovery
4. `./scripts/e2e-smoke.sh`, including a real daemon, doctor checks, setup/import preview,
   source-import apply, review-policy changes, review approval, built MCP launcher, MCP post-work
   commit, and host-compatible session hook output
5. `./scripts/test-install-local.sh`, which verifies the source installer in an isolated local
   home without changing real harness configuration
6. optional external validators when they are installed locally:
   - the Codex plugin validator from the `plugin-creator` skill
   - `claude plugin validate` for the Claude plugin fixture
   - `claude plugin validate` for the repo marketplace fixture
7. `STRICT_EXTERNAL_VALIDATORS=1 ./scripts/validate-adapters.sh` turns missing external validators into hard failures

CI checks the core/CLI crates with Rust 1.85 and the Tauri desktop crate with Rust 1.88.

## What `test-hooks.sh` covers

- JSON parsing for plugin manifests, MCP manifests, hook configs, and marketplace fixtures
- drift checks for copied shared assets
- hook dispatch argument coverage for both Codex and Claude Code adapters
- fail-open behavior when `contextctl` is missing or unsupported
- MCP launcher behavior for supported and unsupported `context-mcp` binaries
