#!/bin/sh
set -eu

command_name="${1:-}"
if [ -z "$command_name" ]; then
  exec tauri
fi
shift

if [ "$command_name" = "build" ]; then
  exec tauri build --config src-tauri/tauri.bundle.conf.json "$@"
fi

exec tauri "$command_name" "$@"
