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
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

cat > "$payload_file"

contextctl_bin="${CONTEXTCTL_BIN:-}"
if [ -n "$contextctl_bin" ] && [ ! -x "$contextctl_bin" ]; then
  echo "[context-manager] CONTEXTCTL_BIN is not executable: $contextctl_bin" >&2
  exit 0
fi
if [ -z "$contextctl_bin" ]; then
  contextctl_bin="$(command -v contextctl 2>/dev/null || true)"
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

if ! "$contextctl_bin" --help >"$stdout_file" 2>"$stderr_file"; then
  echo "[context-manager] contextctl exists but is not runnable; skipping ${adapter}/${mode}" >&2
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
    "$contextctl_bin" hook \
      --adapter "$adapter" \
      --mode "$mode" \
      --payload-file "$payload_file" \
      --project-dir "$project_dir" \
      --plugin-root "$plugin_root" \
      --plugin-data "$plugin_data" \
      >"$stdout_file" 2>"$stderr_file"
  elif [ -n "$plugin_root" ]; then
    "$contextctl_bin" hook \
      --adapter "$adapter" \
      --mode "$mode" \
      --payload-file "$payload_file" \
      --project-dir "$project_dir" \
      --plugin-root "$plugin_root" \
      >"$stdout_file" 2>"$stderr_file"
  else
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
fi

if [ -s "$stderr_file" ]; then
  cat "$stderr_file" >&2
fi
echo "[context-manager] contextctl hook dispatch failed for ${adapter}/${mode}; continuing without persistence" >&2
exit 0
