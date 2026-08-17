# Universal Context Manager desktop

The macOS control plane for the local Universal Context Manager daemon. The production Tauri
runtime talks to `contextd` through `context-client`; the browser-only development and unit-test
surface uses synthetic data because a regular browser cannot call Tauri IPC.

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
restore, JSON export, JSON/Markdown import, adapter health, and local settings.
