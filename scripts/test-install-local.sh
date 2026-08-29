#!/bin/sh
set -eu

root="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
real_home="${HOME:?}"
real_cargo_home="${CARGO_HOME:-$real_home/.cargo}"
real_rustup_home="${RUSTUP_HOME:-$real_home/.rustup}"
workdir="$(mktemp -d /tmp/ucm-install.XXXXXX)"
socket="$workdir/contextd.sock"
daemon_pid=""

stop_daemon() {
  daemon_pid="$(lsof -t "$1" | head -n 1)"
  case "$daemon_pid" in
    ''|*[!0-9]*)
      echo "unable to resolve installed contextd process" >&2
      exit 1
      ;;
  esac
  kill "$daemon_pid"
  wait "$daemon_pid" >/dev/null 2>&1 || true
  daemon_pid=""
}

cleanup() {
  if [ -n "$daemon_pid" ]; then
    kill "$daemon_pid" >/dev/null 2>&1 || true
    wait "$daemon_pid" >/dev/null 2>&1 || true
  fi
  rm -rf "$workdir"
}
trap cleanup EXIT HUP INT TERM

for command in jq lsof; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "required command not found: $command" >&2
    exit 1
  fi
done

cd "$root"
CONTEXT_MANAGER_HOME="$workdir/data" \
CONTEXT_SOCKET_PATH="$socket" \
CARGO_TARGET_DIR="$workdir/cargo-target" \
  ./scripts/install-local.sh \
    --debug \
    --no-adapters \
    --bin-dir "$workdir/bin" \
    >"$workdir/install.out"

for binary in contextd contextctl context-mcp; do
  test -x "$workdir/bin/$binary"
done

grep -q "Installation complete." "$workdir/install.out"

CONTEXT_MANAGER_HOME="$workdir/data" \
CONTEXT_SOCKET_PATH="$socket" \
CONTEXT_MANAGER_BIN_DIR="$workdir/bin" \
  "$workdir/bin/contextctl" --json doctor \
  | jq -e '
      .overall != "failed"
      and .versions.state == "compatible"
      and .versions.api.state == "compatible"
      and any(.checks[]; .id == "mcp_handshake" and .status == "pass")
    ' >/dev/null

stop_daemon "$socket"

fake_bin="$workdir/fake-bin"
fake_home="$workdir/fake-home"
adapter_socket="$workdir/adapter-contextd.sock"
adapter_log="$workdir/adapter.log"
mkdir -p "$fake_bin" "$fake_home"

cat >"$fake_bin/codex" <<'STUB'
#!/bin/sh
set -eu
printf 'codex %s\n' "$*" >>"${UCM_TEST_ADAPTER_LOG:?}"
plugin_root="${HOME:?}/.codex/plugins/context-manager"
marketplace_state="${HOME}/.codex/ucm-marketplace"
case "$*" in
  "plugin list --json")
    if [ -f "$plugin_root/.codex-plugin/plugin.json" ]; then
      printf '%s\n' '{"installed":[{"pluginId":"context-manager@universal-context-manager-local","enabled":true}]}'
    else
      printf '%s\n' '{"installed":[]}'
    fi
    ;;
  "plugin marketplace list --json")
    if [ -f "$marketplace_state" ]; then
      printf '%s\n' '{"marketplaces":[{"name":"universal-context-manager-local"}]}'
    else
      printf '%s\n' '{"marketplaces":[]}'
    fi
    ;;
  "plugin remove context-manager@universal-context-manager-local")
    rm -rf "$plugin_root"
    ;;
  "plugin marketplace remove universal-context-manager-local")
    rm -f "$marketplace_state"
    ;;
  plugin\ marketplace\ add\ *)
    mkdir -p "$(dirname "$marketplace_state")"
    : >"$marketplace_state"
    ;;
  "plugin add context-manager@universal-context-manager-local")
    mkdir -p "$plugin_root"
    cp -R "${UCM_TEST_ROOT:?}/plugins/context-manager/." "$plugin_root/"
    ;;
  *)
    echo "unexpected codex command: $*" >&2
    exit 2
    ;;
esac
STUB
chmod 0755 "$fake_bin/codex"

cat >"$fake_bin/claude" <<'STUB'
#!/bin/sh
set -eu
printf 'claude %s\n' "$*" >>"${UCM_TEST_ADAPTER_LOG:?}"
plugin_root="${HOME:?}/.claude/plugins/cache/context-manager/0.1.0"
registry="${HOME}/.claude/plugins/installed_plugins.json"
marketplace_state="${HOME}/.claude/ucm-marketplace"
case "$*" in
  "plugin list --json")
    if [ -f "$plugin_root/.claude-plugin/plugin.json" ]; then
      printf '%s\n' '[{"id":"context-manager@universal-context-manager-local","enabled":true}]'
    else
      printf '%s\n' '[]'
    fi
    ;;
  "plugin marketplace list --json")
    if [ -f "$marketplace_state" ]; then
      printf '%s\n' '[{"name":"universal-context-manager-local"}]'
    else
      printf '%s\n' '[]'
    fi
    ;;
  "plugin uninstall --scope user context-manager@universal-context-manager-local")
    rm -rf "$plugin_root"
    rm -f "$registry"
    ;;
  "plugin marketplace remove universal-context-manager-local")
    rm -f "$marketplace_state"
    ;;
  plugin\ marketplace\ add\ --scope\ user\ *)
    mkdir -p "$(dirname "$marketplace_state")"
    : >"$marketplace_state"
    ;;
  "plugin install --scope user context-manager@universal-context-manager-local")
    mkdir -p "$plugin_root"
    cp -R "${UCM_TEST_ROOT:?}/adapters/claude-code/." "$plugin_root/"
    mkdir -p "$(dirname "$registry")"
    printf '{"plugins":{"context-manager@universal-context-manager-local":[{"installPath":"%s"}]}}\n' \
      "$plugin_root" >"$registry"
    ;;
  *)
    echo "unexpected claude command: $*" >&2
    exit 2
    ;;
esac
STUB
chmod 0755 "$fake_bin/claude"

# Seed stale installs so the installer must refresh both adapters from this checkout.
HOME="$fake_home" UCM_TEST_ROOT="$root" UCM_TEST_ADAPTER_LOG="$adapter_log" \
  "$fake_bin/codex" plugin marketplace add /stale/checkout
HOME="$fake_home" UCM_TEST_ROOT="$root" UCM_TEST_ADAPTER_LOG="$adapter_log" \
  "$fake_bin/codex" plugin add context-manager@universal-context-manager-local
HOME="$fake_home" UCM_TEST_ROOT="$root" UCM_TEST_ADAPTER_LOG="$adapter_log" \
  "$fake_bin/claude" plugin marketplace add --scope user /stale/checkout
HOME="$fake_home" UCM_TEST_ROOT="$root" UCM_TEST_ADAPTER_LOG="$adapter_log" \
  "$fake_bin/claude" plugin install --scope user context-manager@universal-context-manager-local

HOME="$fake_home" \
CARGO_HOME="$real_cargo_home" \
RUSTUP_HOME="$real_rustup_home" \
PATH="$fake_bin:$PATH" \
UCM_TEST_ROOT="$root" \
UCM_TEST_ADAPTER_LOG="$adapter_log" \
CONTEXT_MANAGER_HOME="$workdir/adapter-data" \
CONTEXT_SOCKET_PATH="$adapter_socket" \
CARGO_TARGET_DIR="$workdir/cargo-target" \
  ./scripts/install-local.sh \
    --debug \
    --no-build \
    --all-adapters \
    --bin-dir "$workdir/adapter-bin" \
    >"$workdir/adapter-install.out"

grep -q "codex plugin remove context-manager@universal-context-manager-local" "$adapter_log"
grep -q "codex plugin marketplace remove universal-context-manager-local" "$adapter_log"
grep -q "claude plugin uninstall --scope user context-manager@universal-context-manager-local" "$adapter_log"
grep -q "claude plugin marketplace remove universal-context-manager-local" "$adapter_log"
grep -q "Verified Codex adapter installation" "$workdir/adapter-install.out"
grep -q "Verified Claude Code adapter installation" "$workdir/adapter-install.out"

stop_daemon "$adapter_socket"

echo "local installer test passed"
