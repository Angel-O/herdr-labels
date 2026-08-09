#!/bin/sh

set -eu

NAME="herdr-labels"
REPO="Angel-O/herdr-labels"

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$script_dir/.." && pwd)
manifest="$root/herdr-plugin.toml"
destination=${HERDR_LABELS_INSTALL_PATH:-"$root/target/release/$NAME"}
base_url=${HERDR_LABELS_RELEASE_BASE_URL:-"https://github.com/$REPO/releases/download"}

fail() {
  printf '%s: %s\n' "$NAME" "$1" >&2
  exit 1
}

have() {
  command -v "$1" >/dev/null 2>&1
}

download() {
  if have curl; then
    curl -fsSL --retry 5 --retry-delay 2 -o "$2" "$1"
  elif have wget; then
    wget -q -O "$2" "$1"
  else
    fail "curl or wget is required to download a release binary"
  fi
}

sha256_of() {
  if have sha256sum; then
    sha256sum "$1" | awk '{print $1}'
  elif have shasum; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    fail "sha256sum or shasum is required to verify the release binary"
  fi
}

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$manifest" | head -n 1)
[ -n "$version" ] || fail "could not read the version from herdr-plugin.toml"

os=${HERDR_LABELS_OS:-$(uname -s)}
arch=${HERDR_LABELS_ARCH:-$(uname -m)}
case "$os-$arch" in
  Darwin-arm64 | Darwin-aarch64) target="aarch64-apple-darwin" ;;
  Darwin-x86_64 | Darwin-amd64) target="x86_64-apple-darwin" ;;
  Linux-aarch64 | Linux-arm64) target="aarch64-unknown-linux-musl" ;;
  Linux-x86_64 | Linux-amd64) target="x86_64-unknown-linux-musl" ;;
  *) fail "no prebuilt binary is available for $os/$arch" ;;
esac

asset="$NAME-$target"
release_url="$base_url/v$version"
tmp=$(mktemp -d) || fail "could not create a temporary directory"
staged=""
trap 'rm -rf "$tmp"; [ -z "$staged" ] || rm -f "$staged"' EXIT HUP INT TERM

printf '%s: downloading v%s for %s\n' "$NAME" "$version" "$target"
download "$release_url/$asset" "$tmp/$asset"
download "$release_url/SHA256SUMS" "$tmp/SHA256SUMS"

expected=$(awk -v asset="$asset" '$2 == asset || $2 == "*" asset { print $1; exit }' "$tmp/SHA256SUMS")
[ -n "$expected" ] || fail "$asset is not listed in the release checksums"
actual=$(sha256_of "$tmp/$asset")
[ "$actual" = "$expected" ] || fail "checksum mismatch for $asset"

destination_dir=$(dirname -- "$destination")
mkdir -p "$destination_dir"
staged=$(mktemp "$destination_dir/.herdr-labels.XXXXXX") || fail "could not stage the release binary"
cp "$tmp/$asset" "$staged"
chmod 0755 "$staged"
mv -f "$staged" "$destination"
staged=""
printf '%s: installed verified v%s binary at %s\n' "$NAME" "$version" "$destination"
