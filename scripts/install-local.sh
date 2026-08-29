#!/bin/sh
set -eu

root="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
profile="release"
build=true
adapter_mode="auto"
install_codex=false
install_claude=false
bin_dir="${CONTEXT_MANAGER_BIN_DIR:-${HOME:?}/.local/bin}"

usage() {
  cat <<'EOF'
Install Universal Context Manager from this source checkout.

Usage: ./scripts/install-local.sh [options]

Options:
  --debug                 Install debug binaries instead of release binaries.
  --release               Install release binaries (default).
  --bin-dir PATH          Install binaries into PATH (default: ~/.local/bin).
  --no-build              Reuse binaries already present under target/.
  --codex                 Install the Codex adapter.
  --claude-code           Install the Claude Code adapter.
  --all-adapters          Install both adapters.
  --no-adapters           Install binaries only.
  -h, --help              Show this help.

With no adapter option, adapters are installed for the Codex or Claude Code
CLIs currently available on this machine.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --debug)
      profile="debug"
      shift
      ;;
    --release)
      profile="release"
      shift
      ;;
    --bin-dir)
      if [ "$#" -lt 2 ]; then
        echo "--bin-dir requires a path" >&2
        exit 2
      fi
      bin_dir="$2"
      shift 2
      ;;
    --no-build)
      build=false
      shift
      ;;
    --codex)
      adapter_mode="explicit"
      install_codex=true
      shift
      ;;
    --claude-code)
      adapter_mode="explicit"
      install_claude=true
      shift
      ;;
    --all-adapters)
      adapter_mode="explicit"
      install_codex=true
      install_claude=true
      shift
      ;;
    --no-adapters)
      adapter_mode="explicit"
      install_codex=false
      install_claude=false
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [ "$adapter_mode" = "auto" ]; then
  if command -v codex >/dev/null 2>&1; then
    install_codex=true
  fi
  if command -v claude >/dev/null 2>&1; then
    install_claude=true
  fi
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required for verified installation" >&2
  exit 1
fi

if [ "$build" = true ]; then
  echo "Building Universal Context Manager ($profile)..."
  if [ "$profile" = "release" ]; then
    (cd "$root" && cargo build --release -p contextd -p contextctl -p context-mcp)
  else
    (cd "$root" && cargo build -p contextd -p contextctl -p context-mcp)
  fi
fi

target_dir="$(cd "$root" && cargo metadata --format-version 1 --no-deps | jq -er '.target_directory')"
source_dir="$target_dir/$profile"
for binary in contextd contextctl context-mcp; do
  if [ ! -x "$source_dir/$binary" ]; then
    echo "missing $source_dir/$binary; rerun without --no-build" >&2
    exit 1
  fi
done

set +e
preflight_json="$(
  CONTEXTD_BIN="$source_dir/contextd" \
  CONTEXT_MCP_BIN="$source_dir/context-mcp" \
    "$source_dir/contextctl" init >/dev/null 2>&1 \
    && CONTEXTD_BIN="$source_dir/contextd" \
      CONTEXT_MCP_BIN="$source_dir/context-mcp" \
      "$source_dir/contextctl" --json doctor 2>/dev/null
)"
preflight_status=$?
set -e
if [ "$preflight_status" -ne 0 ] \
  || ! printf '%s\n' "$preflight_json" \
    | jq -e '
        .versions.state == "compatible"
        and .versions.api.state == "compatible"
      ' >/dev/null; then
  echo "The running daemon is not compatible with the binaries being installed." >&2
  echo "Stop the older Universal Context Manager daemon or desktop app, then rerun installation." >&2
  CONTEXTD_BIN="$source_dir/contextd" \
  CONTEXT_MCP_BIN="$source_dir/context-mcp" \
    "$source_dir/contextctl" doctor || true
  exit 1
fi

mkdir -p "$bin_dir"
for binary in contextd contextctl context-mcp; do
  install -m 0755 "$source_dir/$binary" "$bin_dir/$binary"
done

export CONTEXT_MANAGER_BIN_DIR="$bin_dir"
export CONTEXT_MANAGER_SOURCE_ROOT="$root"
export CONTEXTD_BIN="$bin_dir/contextd"
export CONTEXTCTL_BIN="$bin_dir/contextctl"
export CONTEXT_MCP_BIN="$bin_dir/context-mcp"

echo "Installed binaries in $bin_dir"
"$bin_dir/contextctl" init

if [ "$install_codex" = true ]; then
  if ! command -v codex >/dev/null 2>&1; then
    echo "Codex was requested but the codex CLI is unavailable" >&2
    exit 1
  fi
  if codex plugin list --json \
    | jq -e '.installed[]? | select(.pluginId == "context-manager@universal-context-manager-local")' \
      >/dev/null; then
    codex plugin remove context-manager@universal-context-manager-local
  fi
  if codex plugin marketplace list --json \
    | jq -e '.marketplaces[]? | select(.name == "universal-context-manager-local")' \
      >/dev/null; then
    codex plugin marketplace remove universal-context-manager-local
  fi
  codex plugin marketplace add "$root"
  codex plugin add context-manager@universal-context-manager-local
  if ! codex plugin list --json \
    | jq -e '.installed[]? | select(.pluginId == "context-manager@universal-context-manager-local" and .enabled == true)' \
      >/dev/null; then
    echo "Codex adapter installation could not be verified" >&2
    exit 1
  fi
  echo "Verified Codex adapter installation"
fi

if [ "$install_claude" = true ]; then
  if ! command -v claude >/dev/null 2>&1; then
    echo "Claude Code was requested but the claude CLI is unavailable" >&2
    exit 1
  fi
  if claude plugin list --json \
    | jq -e '.[]? | select(.id == "context-manager@universal-context-manager-local" and ((.scope // "user") == "user"))' \
      >/dev/null; then
    claude plugin uninstall --scope user context-manager@universal-context-manager-local
  fi
  if claude plugin marketplace list --json \
    | jq -e '.[]? | select(.name == "universal-context-manager-local" and ((.scope // "user") == "user"))' \
      >/dev/null; then
    claude plugin marketplace remove universal-context-manager-local
  fi
  claude plugin marketplace add --scope user "$root"
  claude plugin install --scope user context-manager@universal-context-manager-local
  if ! claude plugin list --json \
    | jq -e '.[]? | select(.id == "context-manager@universal-context-manager-local" and .enabled == true and ((.scope // "user") == "user"))' \
      >/dev/null; then
    echo "Claude Code adapter installation could not be verified" >&2
    exit 1
  fi
  echo "Verified Claude Code adapter installation"
fi

echo
echo "Running setup preflight..."
setup_preflight() {
  "$bin_dir/contextctl" setup --project "$root" "$@"
}

verify_setup_adapters() {
  "$bin_dir/contextctl" --json setup --project "$root" "$@" \
    | jq -e 'all(.adapters[]; .configured == true)' >/dev/null
}

if [ "$install_codex" = true ] && [ "$install_claude" = true ]; then
  setup_preflight --adapter codex --adapter claude-code
  if ! verify_setup_adapters --adapter codex --adapter claude-code; then
    echo "Installed adapter files did not pass end-to-end setup verification." >&2
    exit 1
  fi
elif [ "$install_codex" = true ]; then
  setup_preflight --adapter codex
  if ! verify_setup_adapters --adapter codex; then
    echo "Installed Codex adapter files did not pass setup verification." >&2
    exit 1
  fi
elif [ "$install_claude" = true ]; then
  setup_preflight --adapter claude-code
  if ! verify_setup_adapters --adapter claude-code; then
    echo "Installed Claude Code adapter files did not pass setup verification." >&2
    exit 1
  fi
else
  setup_preflight
fi

echo
echo "Running diagnostics..."
"$bin_dir/contextctl" doctor
if ! "$bin_dir/contextctl" --json doctor | jq -e '.overall != "failed"' >/dev/null; then
  echo "Installation stopped because required runtime diagnostics failed." >&2
  echo "Review the doctor output above, repair the reported issue, and rerun this installer." >&2
  exit 1
fi

echo
echo "Installation complete."
echo "Add this directory to PATH for direct CLI use:"
echo "  export PATH=\"$bin_dir:\$PATH\""
echo
echo "Run setup again from any repository to import or create its context:"
echo "  cd /path/to/project && $bin_dir/contextctl setup"
