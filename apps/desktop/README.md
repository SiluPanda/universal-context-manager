# Universal Context Manager desktop

The macOS control plane for the local Universal Context Manager daemon. The production Tauri
runtime talks to `contextd` through `context-client`; the browser-only development and unit-test
surface uses synthetic data because a regular browser cannot call Tauri IPC.

Prerequisites: Rust 1.88+, Node 22+, pnpm 9+, and macOS 13+.

## User workflows

- **Onboarding:** select a project, preview existing instructions, create or import the first
  durable entry, choose review policy, and verify effective context
- **Inbox:** inspect provenance and before/after content, then approve, edit, reject, or process
  compatible items in bulk
- **Library:** edit every individual Markdown or JSON entry without collapsing sibling entries in
  the same pack
- **Effective Context:** inspect the backend-rendered Global → Project → Task result, exclusions,
  provenance, revisions, and adapter destination
- **Search:** navigate directly to entries, reviews, history, activity, and connections
- **Connections:** diagnose daemon, binary, MCP, version, permission, spool, and plugin health
- **Privacy & Data:** inspect local paths and storage boundaries, preview backups/imports, and
  archive scoped context with confirmation

## Run the frontend

```bash
pnpm install
pnpm dev
```

This is useful for visual work. It intentionally displays the mock dashboard.

## Run the live Tauri app

From this directory:

```bash
pnpm tauri dev
```

The Tauri pre-dev command builds the Rust daemon, CLI, and MCP server, prepares local-architecture
sidecars, then starts Vite on port 1420. Context remains in the local data directory selected by
`CONTEXT_MANAGER_HOME` or the platform default.

## Build the macOS bundle

```bash
pnpm tauri build
```

The build bundles `contextd`, `contextctl`, and `context-mcp` as sidecars. The app is not signed or
notarized in this source MVP.

## Verify

```bash
pnpm lint
pnpm test
pnpm build
cargo test -p app
```

The desktop exposes live packs, layered previews, FTS search, review decisions, runs, revision
restore, staged instruction import, validated bundle import, adapter diagnostics, review policy,
native file dialogs, and local privacy/data controls.
