# Architecture

Universal Context Manager is a local-first, human-governed control plane for durable coding context.

## Runtime pieces

- `contextctl`: operator CLI for ingest, review, import, export, and adapter hook dispatch.
- `context-mcp`: MCP server that exposes reviewed context to harnesses.
- `contextd`: local daemon for indexing, queues, and background maintenance.
- desktop app: macOS review surface for approvals, exports, revisions, and pack editing.
- adapters: Codex and Claude Code packaging, hook shims, and marketplace fixtures.

## Local data plane

- SQLite is the source of truth for packs, entries, reviews, revisions, runs, and imports/exports.
- `contextd` serves the same store over a Unix domain socket for harness-facing clients.
- the desktop app reads and writes the same local store, while keeping separate local-only UI preferences.
- the persisted review policy is shared by CLI, desktop, MCP, imports, and adapter writes

## User-facing model

- **Library:** approved entries grouped into internal packs.
- **Inbox:** pending proposals and import conflicts.
- **Effective context:** the ordered Global → Project → Task composition for a selected adapter.
- **History:** immutable revision and provenance records.
- **Connections:** verified daemon, MCP, and adapter health.

Review policy modes are:

- `strict`: review every non-duplicate proposal
- `balanced`: review global, conflicting, and locked writes
- `fast`: review global and locked writes while allowing project/task conflicts to apply

Secret rejection remains mandatory in every mode.

## Adapter flow

1. the harness fires a plugin hook event;
2. the adapter shell shim stores the JSON payload in a temp file;
3. the shim attempts `contextctl hook --adapter <name> --mode <event> --payload-file <file>`;
4. `SessionStart` composes the layered context, creates or reuses a run id, and injects a concise reminder to use `commit_work` once after successful durable work;
5. `SessionEnd` stays cleanup-only and fail-open;
6. the plugin-scoped MCP launcher starts `context-mcp` in stdio mode.

## Shared assets

Canonical shared assets live under `adapters/shared/`:

- `skills/context-manager/SKILL.md`
- `scripts/run-context-hook.sh`
- `scripts/run-context-mcp.sh`

Repo fixtures copy those files into both plugin roots. `scripts/sync-shared-assets.sh` is the single source-of-truth sync step and `--check` mode protects against drift in CI.
