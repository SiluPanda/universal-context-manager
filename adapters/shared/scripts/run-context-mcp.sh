#!/bin/sh
set -eu

adapter="${CONTEXT_MANAGER_HARNESS:-generic}"
context_mcp_bin="${CONTEXT_MCP_BIN:-${CONTEXT_MANAGER_CONTEXT_MCP:-}}"

if [ -n "$context_mcp_bin" ] && [ ! -x "$context_mcp_bin" ]; then
  echo "[context-manager] CONTEXT_MCP_BIN is not executable: $context_mcp_bin" >&2
  exit 78
fi
if [ -z "$context_mcp_bin" ]; then
  context_mcp_bin="$(command -v context-mcp 2>/dev/null || true)"
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

help_text="$("$context_mcp_bin" --help 2>/dev/null || true)"
case "$help_text" in
  *stdio*|*serve*) ;;
  *)
    echo "[context-manager] context-mcp is present but does not advertise stdio serving; refusing to launch." >&2
    exit 78
    ;;
esac

if "$context_mcp_bin" serve --help >/dev/null 2>&1; then
  exec "$context_mcp_bin" serve --adapter "$adapter" --stdio "$@"
fi

exec "$context_mcp_bin" --adapter "$adapter" --stdio "$@"
