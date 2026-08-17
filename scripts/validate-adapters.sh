#!/bin/sh
set -eu

root="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
plugin_creator_root="${PLUGIN_CREATOR_ROOT:-$HOME/.codex/skills/.system/plugin-creator}"
plugin_validator="$plugin_creator_root/scripts/validate_plugin.py"
strict_external="${STRICT_EXTERNAL_VALIDATORS:-0}"

cd "$root"

./scripts/sync-shared-assets.sh --check
./scripts/test-hooks.sh
./scripts/e2e-smoke.sh

warn_or_fail() {
  message="$1"
  if [ "$strict_external" = "1" ]; then
    echo "$message" >&2
    exit 1
  fi
  echo "warning: $message" >&2
}

if [ -f "$plugin_validator" ]; then
  if python3 -c 'import yaml' >/dev/null 2>&1; then
    python3 "$plugin_validator" "$root/plugins/context-manager"
  else
    warn_or_fail "PyYAML is required for the optional Codex plugin validator."
  fi
else
  warn_or_fail "Codex plugin validator not found at $plugin_validator."
fi

if command -v claude >/dev/null 2>&1; then
  claude plugin validate "$root/adapters/claude-code"
  claude plugin validate "$root"
else
  warn_or_fail "claude CLI not found; skipping optional Claude plugin validation."
fi
