#!/bin/sh
set -eu

adapter="${CONTEXT_MANAGER_HARNESS:-generic}"
context_mcp_bin="${CONTEXT_MCP_BIN:-${CONTEXT_MANAGER_CONTEXT_MCP:-}}"
probe_timeout_seconds="${CONTEXT_MANAGER_MCP_PROBE_TIMEOUT_SECONDS:-5}"
case "$probe_timeout_seconds" in
  ''|*[!0-9]*|0)
    probe_timeout_seconds=5
    ;;
esac

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/context-manager-mcp.XXXXXX" 2>/dev/null || true)"
if [ -z "$tmp_dir" ]; then
  echo "[context-manager] unable to create a private MCP launcher workspace." >&2
  exit 78
fi
help_file="$tmp_dir/help"
timeout_marker="$tmp_dir/timed-out"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

# Bound compatibility probes without relying on GNU timeout.
run_with_timeout() (
  marker_file="$1"
  timeout_seconds="$2"
  shift 2

  rm -f "$marker_file"
  exec 3<&0
  "$@" <&3 3<&- &
  command_pid=$!
  exec 3<&-
  (
    timer_sleep_pid=""
    stop_timer() {
      if [ -n "$timer_sleep_pid" ]; then
        kill -TERM "$timer_sleep_pid" 2>/dev/null || true
        wait "$timer_sleep_pid" 2>/dev/null || true
      fi
      exit 0
    }
    trap stop_timer HUP INT TERM

    sleep "$timeout_seconds" &
    timer_sleep_pid=$!
    wait "$timer_sleep_pid" 2>/dev/null || exit 0
    timer_sleep_pid=""
    if kill -0 "$command_pid" 2>/dev/null; then
      : >"$marker_file"
      kill -TERM "$command_pid" 2>/dev/null || true
      sleep 1 &
      timer_sleep_pid=$!
      wait "$timer_sleep_pid" 2>/dev/null || exit 0
      timer_sleep_pid=""
      kill -KILL "$command_pid" 2>/dev/null || true
    fi
  ) &
  timer_pid=$!

  if wait "$command_pid" 2>/dev/null; then
    command_status=0
  else
    command_status=$?
  fi
  kill "$timer_pid" 2>/dev/null || true
  wait "$timer_pid" 2>/dev/null || true

  if [ -f "$marker_file" ]; then
    exit 124
  fi
  exit "$command_status"
)

if [ -n "$context_mcp_bin" ] && [ ! -x "$context_mcp_bin" ]; then
  echo "[context-manager] CONTEXT_MCP_BIN is not executable: $context_mcp_bin" >&2
  exit 78
fi
if [ -z "$context_mcp_bin" ] && [ -n "${CONTEXT_MANAGER_BIN_DIR:-}" ]; then
  candidate="${CONTEXT_MANAGER_BIN_DIR}/context-mcp"
  if [ -x "$candidate" ]; then
    context_mcp_bin="$candidate"
  fi
fi
if [ -z "$context_mcp_bin" ]; then
  context_mcp_bin="$(command -v context-mcp 2>/dev/null || true)"
fi
if [ -z "$context_mcp_bin" ] && [ -n "${HOME:-}" ]; then
  candidate="${HOME}/.local/bin/context-mcp"
  if [ -x "$candidate" ]; then
    context_mcp_bin="$candidate"
  fi
fi
if [ -z "$context_mcp_bin" ] && [ -n "${HOME:-}" ]; then
  candidate="${HOME}/Applications/Universal Context Manager.app/Contents/MacOS/context-mcp"
  if [ -x "$candidate" ]; then
    context_mcp_bin="$candidate"
  fi
fi
if [ -z "$context_mcp_bin" ]; then
  bundled_context_mcp="/Applications/Universal Context Manager.app/Contents/MacOS/context-mcp"
  if [ -x "$bundled_context_mcp" ]; then
    context_mcp_bin="$bundled_context_mcp"
  fi
fi
if [ -z "$context_mcp_bin" ]; then
  echo "[context-manager] context-mcp not found. Build/install it or set CONTEXT_MCP_BIN." >&2
  exit 78
fi

if run_with_timeout "$timeout_marker" "$probe_timeout_seconds" \
  "$context_mcp_bin" --help </dev/null >"$help_file" 2>/dev/null; then
  help_text="$(cat "$help_file" 2>/dev/null || true)"
else
  probe_status=$?
  if [ "$probe_status" -eq 124 ]; then
    echo "[context-manager] context-mcp compatibility probe timed out." >&2
  else
    echo "[context-manager] context-mcp is present but could not be probed." >&2
  fi
  exit 78
fi
case "$help_text" in
  *stdio*|*serve*) ;;
  *)
    echo "[context-manager] context-mcp is present but does not advertise stdio serving; refusing to launch." >&2
    exit 78
    ;;
esac

if run_with_timeout "$timeout_marker" "$probe_timeout_seconds" \
  "$context_mcp_bin" serve --help </dev/null >/dev/null 2>&1; then
  cleanup
  trap - EXIT HUP INT TERM
  exec "$context_mcp_bin" serve --adapter "$adapter" --stdio "$@"
else
  serve_probe_status=$?
  if [ "$serve_probe_status" -eq 124 ]; then
    echo "[context-manager] context-mcp serve probe timed out." >&2
    exit 78
  fi
fi

cleanup
trap - EXIT HUP INT TERM
exec "$context_mcp_bin" --adapter "$adapter" --stdio "$@"
