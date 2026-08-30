#!/bin/sh
set -eu

root="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
cd "$root"

./scripts/sync-shared-assets.sh --check
test -x plugins/context-manager/scripts/run-context-mcp.sh
test -x adapters/claude-code/scripts/run-context-mcp.sh

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

workdir="$(mktemp -d "${TMPDIR:-/tmp}/ucm-hooks.XXXXXX")"
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

managed_bin="$workdir/managed-bin"
mkdir -p "$managed_bin"
cp "$workdir/contextctl" "$managed_bin/contextctl"
managed_hook_log="$workdir/managed-hook.log"
CONTEXT_TEST_LOG="$managed_hook_log" CONTEXT_MANAGER_BIN_DIR="$managed_bin" CONTEXTCTL_BIN= \
  sh plugins/context-manager/scripts/run-context-hook.sh codex session-start <<EOF_PAYLOAD
$sample_payload
EOF_PAYLOAD
grep -q 'codex|session-start|' "$managed_hook_log"

user_bin="$workdir/home/.local/bin"
mkdir -p "$user_bin"
cp "$workdir/contextctl" "$user_bin/contextctl"
user_hook_log="$workdir/user-hook.log"
CONTEXT_TEST_LOG="$user_hook_log" HOME="$workdir/home" CONTEXTCTL_BIN= CONTEXT_MANAGER_BIN_DIR= \
  PATH="/usr/bin:/bin" sh plugins/context-manager/scripts/run-context-hook.sh codex session-start <<EOF_PAYLOAD
$sample_payload
EOF_PAYLOAD
grep -q 'codex|session-start|' "$user_hook_log"

app_bin="$workdir/app-home/Applications/Universal Context Manager.app/Contents/MacOS"
mkdir -p "$app_bin"
cp "$workdir/contextctl" "$app_bin/contextctl"
app_hook_log="$workdir/app-hook.log"
CONTEXT_TEST_LOG="$app_hook_log" HOME="$workdir/app-home" CONTEXTCTL_BIN= CONTEXT_MANAGER_BIN_DIR= \
  PATH="/usr/bin:/bin" sh plugins/context-manager/scripts/run-context-hook.sh codex session-start <<EOF_PAYLOAD
$sample_payload
EOF_PAYLOAD
grep -q 'codex|session-start|' "$app_hook_log"

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

cat > "$workdir/contextctl-failing" <<'STUB'
#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  echo "usage: contextctl hook"
  exit 0
fi
echo "must not reach hook stdout"
echo "daemon unavailable" >&2
exit 1
STUB
chmod +x "$workdir/contextctl-failing"
CONTEXTCTL_BIN="$workdir/contextctl-failing" \
  sh plugins/context-manager/scripts/run-context-hook.sh codex session-start \
  >"$workdir/failing-hook.out" 2>"$workdir/failing-hook.err" <<EOF_PAYLOAD
$sample_payload
EOF_PAYLOAD
test ! -s "$workdir/failing-hook.out"
grep -q 'continuing without persistence' "$workdir/failing-hook.err"

cat > "$workdir/contextctl-hanging" <<'STUB'
#!/bin/sh
set -eu
if [ "${1:-}" = "--help" ]; then
  echo "usage: contextctl hook"
  exit 0
fi
exec sleep 5
STUB
chmod +x "$workdir/contextctl-hanging"
started_at="$(date +%s)"
CONTEXT_MANAGER_HOOK_TIMEOUT_SECONDS=1 CONTEXTCTL_BIN="$workdir/contextctl-hanging" \
  sh plugins/context-manager/scripts/run-context-hook.sh codex session-start \
  >"$workdir/hanging-hook.out" 2>"$workdir/hanging-hook.err" <<EOF_PAYLOAD
$sample_payload
EOF_PAYLOAD
ended_at="$(date +%s)"
elapsed=$((ended_at - started_at))
test "$elapsed" -lt 4
test ! -s "$workdir/hanging-hook.out"
grep -q 'dispatch timed out' "$workdir/hanging-hook.err"

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
direct_mcp_log="$workdir/direct-mcp.log"
CONTEXT_TEST_LOG="$direct_mcp_log" CONTEXT_MCP_BIN="$workdir/context-mcp" CONTEXT_MANAGER_HARNESS="codex" sh plugins/context-manager/scripts/run-context-mcp.sh

grep -q 'serve --adapter codex --stdio' "$direct_mcp_log"

cp "$workdir/context-mcp" "$managed_bin/context-mcp"
managed_mcp_log="$workdir/managed-mcp.log"
CONTEXT_TEST_LOG="$managed_mcp_log" CONTEXT_MANAGER_BIN_DIR="$managed_bin" CONTEXT_MCP_BIN= \
  CONTEXT_MANAGER_HARNESS="codex" sh plugins/context-manager/scripts/run-context-mcp.sh
grep -q 'serve --adapter codex --stdio' "$managed_mcp_log"

cp "$workdir/context-mcp" "$user_bin/context-mcp"
user_mcp_log="$workdir/user-mcp.log"
CONTEXT_TEST_LOG="$user_mcp_log" HOME="$workdir/home" CONTEXT_MCP_BIN= CONTEXT_MANAGER_BIN_DIR= \
  CONTEXT_MANAGER_HARNESS="codex" PATH="/usr/bin:/bin" \
  sh plugins/context-manager/scripts/run-context-mcp.sh
grep -q 'serve --adapter codex --stdio' "$user_mcp_log"

cp "$workdir/context-mcp" "$app_bin/context-mcp"
app_mcp_log="$workdir/app-mcp.log"
CONTEXT_TEST_LOG="$app_mcp_log" HOME="$workdir/app-home" CONTEXT_MCP_BIN= CONTEXT_MANAGER_BIN_DIR= \
  CONTEXT_MANAGER_HARNESS="codex" PATH="/usr/bin:/bin" \
  sh plugins/context-manager/scripts/run-context-mcp.sh
grep -q 'serve --adapter codex --stdio' "$app_mcp_log"

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

cat > "$workdir/context-mcp-hanging" <<'STUB'
#!/bin/sh
set -eu
exec sleep 5
STUB
chmod +x "$workdir/context-mcp-hanging"
started_at="$(date +%s)"
if CONTEXT_MANAGER_MCP_PROBE_TIMEOUT_SECONDS=1 \
  CONTEXT_MCP_BIN="$workdir/context-mcp-hanging" \
  sh plugins/context-manager/scripts/run-context-mcp.sh \
  >"$workdir/hanging-mcp.out" 2>"$workdir/hanging-mcp.err"; then
  echo "expected hanging context-mcp probe to fail" >&2
  exit 1
fi
ended_at="$(date +%s)"
elapsed=$((ended_at - started_at))
test "$elapsed" -lt 4
grep -q 'probe timed out' "$workdir/hanging-mcp.err"

echo "adapter hook tests passed"
