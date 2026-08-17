# Validation

The adapter validation path is intentionally shell-first and repository-local.

## Main command

```bash
./scripts/validate-adapters.sh
```

It runs:

1. `./scripts/sync-shared-assets.sh --check`
2. `./scripts/test-hooks.sh`
3. `./scripts/e2e-smoke.sh`, including a real daemon, review approval, built MCP launcher, MCP
   post-work commit, and host-compatible session hook output
4. optional external validators when they are installed locally:
   - the Codex plugin validator from the `plugin-creator` skill
   - `claude plugin validate` for the Claude plugin fixture
   - `claude plugin validate` for the repo marketplace fixture
5. `STRICT_EXTERNAL_VALIDATORS=1 ./scripts/validate-adapters.sh` turns missing external validators into hard failures

## What `test-hooks.sh` covers

- JSON parsing for plugin manifests, MCP manifests, hook configs, and marketplace fixtures
- drift checks for copied shared assets
- hook dispatch argument coverage for both Codex and Claude Code adapters
- fail-open behavior when `contextctl` is missing or unsupported
- MCP launcher behavior for supported and unsupported `context-mcp` binaries
