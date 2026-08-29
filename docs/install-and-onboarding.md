# Install and onboarding

## Recommended source installation

From the repository root:

```bash
./scripts/install-local.sh
export PATH="$HOME/.local/bin:$PATH"
cd /path/to/project
contextctl setup
```

The installer:

- builds `contextd`, `contextctl`, and `context-mcp`
- installs the three binaries into `~/.local/bin` by default
- initializes the private local data store and daemon
- installs adapters for supported harness CLIs found on the machine
- refreshes the UCM local marketplace/plugin from the current checkout and verifies the result
- runs setup and doctor before reporting success

Choose adapters explicitly when needed:

```bash
./scripts/install-local.sh --codex
./scripts/install-local.sh --claude-code
./scripts/install-local.sh --all-adapters
./scripts/install-local.sh --no-adapters
```

Use `--bin-dir PATH`, `--debug`, or `--no-build` for development-specific installations.

## Guided setup

`contextctl setup` is safe to run repeatedly. By default it:

1. resolves the current Git repository as project scope
2. starts or discovers the daemon
3. reports the local database, socket, and spool paths
4. detects existing instruction files from supported harnesses
5. previews proposed imports and conflicts
6. checks requested adapters
7. composes a real effective-context preview

It does not import detected files unless explicitly requested:

```bash
contextctl setup --project "$PWD" --apply --yes
```

Review modes can be selected during setup or changed later:

```bash
contextctl policy set strict
contextctl policy set balanced
contextctl policy set fast
```

Project-scoped CLI commands use the current Git root when `--scope-id` or `--project` is omitted.
Generate shell completion with:

```bash
mkdir -p ~/.zfunc ~/.local/share/bash-completion/completions ~/.config/fish/completions
contextctl completion zsh > ~/.zfunc/_contextctl
contextctl completion bash > ~/.local/share/bash-completion/completions/contextctl
contextctl completion fish > ~/.config/fish/completions/contextctl.fish
```

## Manual adapter installation

### Codex

```bash
codex plugin marketplace add .
codex plugin add context-manager@universal-context-manager-local
```

Optional user-side MCP policy examples live in `adapters/codex/config.example.toml`.

### Claude Code

```bash
claude plugin marketplace add .
claude plugin install context-manager@universal-context-manager-local
```

Reload plugins or start a new session after updates.

## First-session expectations

- `contextctl doctor` reports the adapter as healthy only after its plugin markers, launchers,
  binaries, and MCP handshake are verified
- the skill is available after the harness reloads its plugin configuration
- `SessionStart` hooks inject the composed context and remind the harness to call `commit_work` after successful durable work
- hooks do not block the session if the local backend is unavailable
- agents report concise applied, pending, skipped, rejected, or spooled counts after persistence
- in a Tauri bundle, the app prepares local-architecture sidecars for `contextd`, `contextctl`, and `context-mcp`
- harness wrappers accept `CONTEXTCTL_BIN`, `CONTEXT_MCP_BIN`, or
  `CONTEXT_MANAGER_BIN_DIR`; otherwise they check `PATH`, `~/.local/bin`, and the app bundle
- no signed releases or packaged binaries are claimed by this repository today

## Troubleshooting

Run:

```bash
contextctl doctor
contextctl doctor --repair
```

Repairs are limited to safe local actions such as creating private directories, starting the
daemon, and retrying spooled writes. The command does not silently rewrite Codex or Claude Code
configuration.
