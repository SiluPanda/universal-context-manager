#!/bin/sh
set -eu

mode="${1:-sync}"
root="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
cd "$root"

copy_or_check() {
  src="$1"
  dest="$2"
  perms="$3"

  if [ "$mode" = "--check" ]; then
    if ! cmp -s "$src" "$dest"; then
      echo "shared asset drift: $dest" >&2
      return 1
    fi
    return 0
  fi

  mkdir -p "$(dirname "$dest")"
  install -m "$perms" "$src" "$dest"
}

status=0
copy_or_check adapters/shared/skills/context-manager/SKILL.md plugins/context-manager/skills/context-manager/SKILL.md 0644 || status=1
copy_or_check adapters/shared/skills/context-manager/SKILL.md adapters/claude-code/skills/context-manager/SKILL.md 0644 || status=1
copy_or_check adapters/shared/scripts/run-context-hook.sh plugins/context-manager/scripts/run-context-hook.sh 0755 || status=1
copy_or_check adapters/shared/scripts/run-context-hook.sh adapters/claude-code/scripts/run-context-hook.sh 0755 || status=1
copy_or_check adapters/shared/scripts/run-context-mcp.sh plugins/context-manager/scripts/run-context-mcp.sh 0755 || status=1
copy_or_check adapters/shared/scripts/run-context-mcp.sh adapters/claude-code/scripts/run-context-mcp.sh 0755 || status=1

exit "$status"
