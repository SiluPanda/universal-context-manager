# Threat model

## Assets

- durable project, task, and handoff context
- local review metadata and provenance
- plugin hook payloads
- imported/exported context bundles

## Trust boundaries

- harness to hook shim
- hook shim to `contextctl`
- plugin-scoped MCP launcher to `context-mcp`
- local user review actions to durable storage
- export bundles crossing repository or machine boundaries

## Primary risks

### Prompt-injected context poisoning
A model or tool output may try to persist unreviewed instructions as durable context.

**Current mitigation:** shared skill text treats stored context as potentially stale user-controlled
memory, forbids transcript/chain-of-thought persistence, and limits writes to one concise post-work
commit. Global, conflicting, and locked writes require review. Project/task writes retain actor,
run, request, revision, and source provenance so operators can inspect or revert them.

**Residual risk:** safe-looking project/task proposals auto-apply. Operators should review unexpected
changes and lock packs that require an approval gate.

### Silent adapter breakage
A missing local backend could block a coding session.

**Current mitigation:** hook scripts fail open. They preserve the harness flow even when `contextctl` is absent, unsupported, or returns an error.

### False-positive MCP availability
A broken binary could emit invalid output and look successful.

**Current mitigation:** the launcher checks `context-mcp --help` for stdio-serving capability and refuses to run when that capability is absent.

### Local secret exfiltration
Context imports or exports may include sensitive material.

**Current mitigation:** core writes and review approval run secret-pattern checks, local directories
and the Unix socket are user-restricted, and the product has no automatic network export transport.
Exports are explicit operator actions.

**Residual risk:** SQLite and export bundles are not application-level encrypted in v1. Host disk
encryption and careful handling of exported files remain the operator's responsibility.

### Multiple local writers
Two daemons against one database could produce confusing concurrent state.

**Current mitigation:** `contextd` holds an exclusive per-data-directory lock, refuses to replace a
live socket, and only removes a socket after determining that it is stale.

### Fixture drift between Codex and Claude Code
Shared skill or launcher logic could diverge.

**Current mitigation:** shared assets are canonical under `adapters/shared/` and enforced with `scripts/sync-shared-assets.sh --check`.
