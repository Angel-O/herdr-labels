#!/bin/sh

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version=$("$root/scripts/check-version.sh")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
release_dir="$tmp/releases/v$version"
mkdir -p "$release_dir"

targets="
aarch64-apple-darwin
x86_64-apple-darwin
aarch64-unknown-linux-musl
x86_64-unknown-linux-musl
"

for target in $targets; do
  asset="herdr-labels-$target"
  printf '#!/bin/sh\nprintf "%%s\\n" "%s"\n' "$target" > "$release_dir/$asset"
  if command -v sha256sum >/dev/null 2>&1; then
    hash=$(sha256sum "$release_dir/$asset" | awk '{print $1}')
  else
    hash=$(shasum -a 256 "$release_dir/$asset" | awk '{print $1}')
  fi
  printf '%s  %s\n' "$hash" "$asset" >> "$release_dir/SHA256SUMS"
done

run_case() {
  os=$1
  arch=$2
  expected=$3
  destination="$tmp/install/$expected"
  HERDR_LABELS_OS="$os" \
    HERDR_LABELS_ARCH="$arch" \
    HERDR_LABELS_RELEASE_BASE_URL="file://$tmp/releases" \
    HERDR_LABELS_INSTALL_PATH="$destination" \
    "$root/scripts/install.sh" >/dev/null
  [ -x "$destination" ] || { echo "$expected was not installed as executable" >&2; exit 1; }
  [ "$("$destination")" = "$expected" ] || { echo "$expected installed wrong content" >&2; exit 1; }
}

run_case Darwin arm64 aarch64-apple-darwin
run_case Darwin x86_64 x86_64-apple-darwin
run_case Linux aarch64 aarch64-unknown-linux-musl
run_case Linux x86_64 x86_64-unknown-linux-musl

if HERDR_LABELS_OS=Plan9 HERDR_LABELS_ARCH=mips HERDR_LABELS_RELEASE_BASE_URL="file://$tmp/releases" \
  HERDR_LABELS_INSTALL_PATH="$tmp/unsupported" "$root/scripts/install.sh" >/dev/null 2>&1; then
  echo "unsupported platforms must fail" >&2
  exit 1
fi

replacement="$tmp/replacement"
printf 'old\n' > "$replacement"
HERDR_LABELS_OS=Linux HERDR_LABELS_ARCH=x86_64 HERDR_LABELS_RELEASE_BASE_URL="file://$tmp/releases" \
  HERDR_LABELS_INSTALL_PATH="$replacement" "$root/scripts/install.sh" >/dev/null
[ "$("$replacement")" = "x86_64-unknown-linux-musl" ] || {
  echo "an existing binary was not replaced" >&2
  exit 1
}

cp "$release_dir/SHA256SUMS" "$tmp/checksums"
printf '0' >> "$release_dir/herdr-labels-x86_64-unknown-linux-musl"
printf 'keep\n' > "$tmp/corrupt"
if HERDR_LABELS_OS=Linux HERDR_LABELS_ARCH=x86_64 HERDR_LABELS_RELEASE_BASE_URL="file://$tmp/releases" \
  HERDR_LABELS_INSTALL_PATH="$tmp/corrupt" "$root/scripts/install.sh" >/dev/null 2>&1; then
  echo "a checksum mismatch must fail" >&2
  exit 1
fi
[ "$(cat "$tmp/corrupt")" = "keep" ] || {
  echo "a checksum failure replaced the existing binary" >&2
  exit 1
}
mv "$tmp/checksums" "$release_dir/SHA256SUMS"

printf 'keep\n' > "$tmp/download-failure"
if HERDR_LABELS_OS=Linux HERDR_LABELS_ARCH=x86_64 HERDR_LABELS_RELEASE_BASE_URL="file://$tmp/missing" \
  HERDR_LABELS_INSTALL_PATH="$tmp/download-failure" "$root/scripts/install.sh" >/dev/null 2>&1; then
  echo "a failed download must fail" >&2
  exit 1
fi
[ "$(cat "$tmp/download-failure")" = "keep" ] || {
  echo "a failed download replaced the existing binary" >&2
  exit 1
}

printf 'installer tests passed\n'
