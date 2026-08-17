#!/bin/sh
set -eu

root="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
cd "$root"

./scripts/sync-shared-assets.sh --check

jq empty plugins/context-manager/.codex-plugin/plugin.json >/dev/null
jq empty plugins/context-manager/.mcp.json >/dev/null
jq empty plugins/context-manager/hooks/hooks.json >/dev/null
jq empty adapters/claude-code/.claude-plugin/plugin.json >/dev/null
jq empty adapters/claude-code/.mcp.json >/dev/null
jq empty adapters/claude-code/hooks/hooks.json >/dev/null
jq empty .agents/plugins/marketplace.json >/dev/null
jq empty .claude-plugin/marketplace.json >/dev/null

jq -e '.hooks.SessionStart[0].hooks[0].command | contains("run-context-hook.sh\" codex session-start")' plugins/context-manager/hooks/hooks.json >/dev/null
jq -e '(.hooks | keys | sort) == ["SessionEnd", "SessionStart"]' plugins/context-manager/hooks/hooks.json >/dev/null
jq -e '.hooks.SessionStart[0].hooks[0].args[1] == "claude-code"' adapters/claude-code/hooks/hooks.json >/dev/null
jq -e '.hooks.SessionEnd[0].hooks[0].args[2] == "session-end"' adapters/claude-code/hooks/hooks.json >/dev/null
jq -e '(.hooks | keys | sort) == ["SessionEnd", "SessionStart"]' adapters/claude-code/hooks/hooks.json >/dev/null

workdir="$(mktemp -d)"
cleanup() {
  rm -rf "$workdir"
}
trap cleanup EXIT HUP INT TERM

sample_payload='{"session":"demo","message":"hello"}'

cat > "$workdir/contextctl" <<'STUB'
#!/bin/sh
set -eu
log="${CONTEXT_TEST_LOG:?}"
cmd="${1:-}"
if [ "$cmd" = "--help" ]; then
  echo "usage: contextctl hook"
  exit 0
fi
if [ "$cmd" != "hook" ]; then
  echo "unexpected command: $cmd" >&2
  exit 2
fi
shift
adapter=""
mode=""
payload_file=""
project_dir=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --adapter)
      adapter="$2"
      shift 2
      ;;
    --mode)
      mode="$2"
      shift 2
      ;;
    --payload-file)
      payload_file="$2"
      shift 2
      ;;
    --project-dir)
      project_dir="$2"
      shift 2
      ;;
    --plugin-root|--plugin-data)
      shift 2
      ;;
    *)
      echo "unexpected arg: $1" >&2
      exit 2
      ;;
  esac
done
printf '%s|%s|%s\n' "$adapter" "$mode" "$project_dir" >> "$log"
printf '%s\n' "$(tr -d '\n' < "$payload_file")" >> "$log"
exit 0
STUB
chmod +x "$workdir/contextctl"

log_file="$workdir/hook.log"
CONTEXT_TEST_LOG="$log_file" CONTEXTCTL_BIN="$workdir/contextctl" sh plugins/context-manager/scripts/run-context-hook.sh codex session-start <<EOF_PAYLOAD
$sample_payload
EOF_PAYLOAD
CONTEXT_TEST_LOG="$log_file" CONTEXTCTL_BIN="$workdir/contextctl" sh adapters/claude-code/scripts/run-context-hook.sh claude-code session-end <<EOF_PAYLOAD
$sample_payload
EOF_PAYLOAD

grep -q 'codex|session-start|' "$log_file"
grep -q 'claude-code|session-end|' "$log_file"
grep -q "$sample_payload" "$log_file"

cat > "$workdir/contextctl-unsupported" <<'STUB'
#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  echo "hello world"
  exit 0
fi
exit 1
STUB
chmod +x "$workdir/contextctl-unsupported"
CONTEXTCTL_BIN="$workdir/contextctl-unsupported" sh plugins/context-manager/scripts/run-context-hook.sh codex session-end <<EOF_PAYLOAD
$sample_payload
EOF_PAYLOAD

cat > "$workdir/context-mcp" <<'STUB'
#!/bin/sh
set -eu
log="${CONTEXT_TEST_LOG:?}"
if [ "${1:-}" = "--help" ]; then
  echo "usage: context-mcp serve --stdio"
  exit 0
fi
if [ "${1:-}" = "serve" ] && [ "${2:-}" = "--help" ]; then
  echo "serve"
  exit 0
fi
printf '%s\n' "$*" >> "$log"
exit 0
STUB
chmod +x "$workdir/context-mcp"
CONTEXT_TEST_LOG="$log_file" CONTEXT_MCP_BIN="$workdir/context-mcp" CONTEXT_MANAGER_HARNESS="codex" sh plugins/context-manager/scripts/run-context-mcp.sh

grep -q 'serve --adapter codex --stdio' "$log_file"

cat > "$workdir/context-mcp-unsupported" <<'STUB'
#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  echo "hello world"
  exit 0
fi
exit 1
STUB
chmod +x "$workdir/context-mcp-unsupported"
if CONTEXT_MCP_BIN="$workdir/context-mcp-unsupported" sh plugins/context-manager/scripts/run-context-mcp.sh 2>/dev/null; then
  echo "expected unsupported context-mcp launcher to fail" >&2
  exit 1
fi

echo "adapter hook tests passed"
