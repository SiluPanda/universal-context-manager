#!/bin/sh
set -eu

root="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
profile="${1:-release}"

case "$profile" in
  debug)
    cargo_profile_args=""
    profile_dir="debug"
    ;;
  release)
    cargo_profile_args="--release"
    profile_dir="release"
    ;;
  *)
    echo "usage: $0 [debug|release]" >&2
    exit 2
    ;;
esac

target_triple="${CONTEXT_MANAGER_TARGET_TRIPLE:-$(rustc -vV | sed -n 's/^host: //p')}"
if [ -z "$target_triple" ]; then
  echo "unable to determine the Rust host target triple" >&2
  exit 1
fi

cd "$root"
target_dir="${CARGO_TARGET_DIR:-$root/target}"
case "$target_dir" in
  /*) ;;
  *) target_dir="$root/$target_dir" ;;
esac
# shellcheck disable=SC2086
cargo build $cargo_profile_args --target-dir "$target_dir" \
  -p contextd -p contextctl -p context-mcp

source_dir="$target_dir/$profile_dir"
destination_dir="$root/apps/desktop/src-tauri/binaries"
mkdir -p "$destination_dir"

for binary in contextd contextctl context-mcp; do
  source_path="$source_dir/$binary"
  destination_path="$destination_dir/$binary-$target_triple"
  if [ ! -x "$source_path" ]; then
    echo "expected built binary not found: $source_path" >&2
    exit 1
  fi
  cp "$source_path" "$destination_path"
  chmod 0755 "$destination_path"
done

echo "prepared Tauri sidecars for $target_triple ($profile)"
