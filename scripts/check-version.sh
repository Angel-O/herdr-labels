#!/bin/sh

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cargo_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
manifest_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/herdr-plugin.toml" | head -n 1)

[ -n "$cargo_version" ] || { echo "could not read Cargo.toml version" >&2; exit 1; }
[ -n "$manifest_version" ] || { echo "could not read herdr-plugin.toml version" >&2; exit 1; }
[ "$cargo_version" = "$manifest_version" ] || {
  echo "version mismatch: Cargo.toml=$cargo_version herdr-plugin.toml=$manifest_version" >&2
  exit 1
}

if [ "$#" -gt 1 ]; then
  echo "usage: scripts/check-version.sh [vVERSION]" >&2
  exit 2
fi

if [ "$#" -eq 1 ]; then
  tag_version=${1#v}
  [ "$tag_version" = "$cargo_version" ] || {
    echo "version mismatch: tag=$1 source=$cargo_version" >&2
    exit 1
  }
fi

printf '%s\n' "$cargo_version"
