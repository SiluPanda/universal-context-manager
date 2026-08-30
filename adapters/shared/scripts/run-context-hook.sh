#!/bin/sh
set -eu

adapter="${1:-}"
mode="${2:-}"

if [ -z "$adapter" ] || [ -z "$mode" ]; then
  echo "[context-manager] missing adapter or mode; skipping hook" >&2
  exit 0
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/context-manager-hook.XXXXXX" 2>/dev/null || true)"
if [ -z "$tmp_dir" ]; then
  echo "[context-manager] unable to create a private hook workspace; skipping ${adapter}/${mode}" >&2
  exit 0
fi
payload_file="$tmp_dir/payload.json"
stdout_file="$tmp_dir/stdout"
stderr_file="$tmp_dir/stderr"
timeout_marker="$tmp_dir/timed-out"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

command_timeout_seconds="${CONTEXT_MANAGER_HOOK_TIMEOUT_SECONDS:-10}"
case "$command_timeout_seconds" in
  ''|*[!0-9]*|0)
    command_timeout_seconds=10
    ;;
esac

# Bound host input and child commands without relying on GNU timeout.
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

if run_with_timeout "$timeout_marker" "$command_timeout_seconds" cat >"$payload_file"; then
  :
else
  payload_status=$?
  if [ "$payload_status" -eq 124 ]; then
    echo "[context-manager] timed out reading hook input; skipping ${adapter}/${mode}" >&2
  else
    echo "[context-manager] unable to read hook input; skipping ${adapter}/${mode}" >&2
  fi
  exit 0
fi

contextctl_bin="${CONTEXTCTL_BIN:-}"
if [ -n "$contextctl_bin" ] && [ ! -x "$contextctl_bin" ]; then
  echo "[context-manager] CONTEXTCTL_BIN is not executable: $contextctl_bin" >&2
  exit 0
fi
if [ -z "$contextctl_bin" ] && [ -n "${CONTEXT_MANAGER_BIN_DIR:-}" ]; then
  candidate="${CONTEXT_MANAGER_BIN_DIR}/contextctl"
  if [ -x "$candidate" ]; then
    contextctl_bin="$candidate"
  fi
fi
if [ -z "$contextctl_bin" ]; then
  contextctl_bin="$(command -v contextctl 2>/dev/null || true)"
fi
if [ -z "$contextctl_bin" ] && [ -n "${HOME:-}" ]; then
  candidate="${HOME}/.local/bin/contextctl"
  if [ -x "$candidate" ]; then
    contextctl_bin="$candidate"
  fi
fi
if [ -z "$contextctl_bin" ] && [ -n "${HOME:-}" ]; then
  candidate="${HOME}/Applications/Universal Context Manager.app/Contents/MacOS/contextctl"
  if [ -x "$candidate" ]; then
    contextctl_bin="$candidate"
  fi
fi
if [ -z "$contextctl_bin" ]; then
  bundled_contextctl="/Applications/Universal Context Manager.app/Contents/MacOS/contextctl"
  if [ -x "$bundled_contextctl" ]; then
    contextctl_bin="$bundled_contextctl"
  fi
fi
if [ -z "$contextctl_bin" ]; then
  echo "[context-manager] contextctl not found; skipping ${adapter}/${mode}" >&2
  exit 0
fi

if run_with_timeout "$timeout_marker" "$command_timeout_seconds" \
  "$contextctl_bin" --help >"$stdout_file" 2>"$stderr_file"; then
  :
else
  probe_status=$?
  if [ "$probe_status" -eq 124 ]; then
    echo "[context-manager] contextctl probe timed out; skipping ${adapter}/${mode}" >&2
  else
    echo "[context-manager] contextctl exists but is not runnable; skipping ${adapter}/${mode}" >&2
  fi
  if [ -s "$stderr_file" ]; then
    cat "$stderr_file" >&2
  fi
  exit 0
fi

help_text="$(cat "$stdout_file" 2>/dev/null || true)"
case "$help_text" in
  *hook*) ;;
  *)
    echo "[context-manager] contextctl does not advertise hook support yet; skipping ${adapter}/${mode}" >&2
    exit 0
    ;;
esac

plugin_root="${PLUGIN_ROOT:-${CLAUDE_PLUGIN_ROOT:-}}"
plugin_data="${PLUGIN_DATA:-${CLAUDE_PLUGIN_DATA:-}}"
project_dir="${CLAUDE_PROJECT_DIR:-$PWD}"

run_contextctl_hook() {
  if [ -n "$plugin_root" ] && [ -n "$plugin_data" ]; then
    run_with_timeout "$timeout_marker" "$command_timeout_seconds" \
      "$contextctl_bin" hook \
      --adapter "$adapter" \
      --mode "$mode" \
      --payload-file "$payload_file" \
      --project-dir "$project_dir" \
      --plugin-root "$plugin_root" \
      --plugin-data "$plugin_data" \
      >"$stdout_file" 2>"$stderr_file"
  elif [ -n "$plugin_root" ]; then
    run_with_timeout "$timeout_marker" "$command_timeout_seconds" \
      "$contextctl_bin" hook \
      --adapter "$adapter" \
      --mode "$mode" \
      --payload-file "$payload_file" \
      --project-dir "$project_dir" \
      --plugin-root "$plugin_root" \
      >"$stdout_file" 2>"$stderr_file"
  else
    run_with_timeout "$timeout_marker" "$command_timeout_seconds" \
      "$contextctl_bin" hook \
      --adapter "$adapter" \
      --mode "$mode" \
      --payload-file "$payload_file" \
      --project-dir "$project_dir" \
      >"$stdout_file" 2>"$stderr_file"
  fi
}

if run_contextctl_hook; then
  if [ -s "$stderr_file" ]; then
    cat "$stderr_file" >&2
  fi
  if [ -s "$stdout_file" ]; then
    cat "$stdout_file"
  fi
  exit 0
else
  dispatch_status=$?
fi

if [ -s "$stderr_file" ]; then
  cat "$stderr_file" >&2
fi
if [ "$dispatch_status" -eq 124 ]; then
  echo "[context-manager] contextctl hook dispatch timed out for ${adapter}/${mode}; continuing without persistence" >&2
else
  echo "[context-manager] contextctl hook dispatch failed for ${adapter}/${mode}; continuing without persistence" >&2
fi
exit 0
