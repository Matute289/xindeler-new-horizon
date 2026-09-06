#!/bin/bash
# Build the downloads manifest.json that xindeler-web-api reads to resolve a
# platform's download URL. See docs/design/specs/2026-09-06-client-downloads-contract.md
# for the schema this must match.
#
# Usage: generate-manifest.sh <version> <packages_dir> <output_file>
#   packages_dir must contain one file per platform, named
#   xindeler-voxygen-<os>-<arch>.<ext>, plus assets.tar.gz.
set -euo pipefail

VERSION="$1"
PACKAGES_DIR="$2"
OUTPUT_FILE="$3"
RELEASED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Deliberately not a bash 4+ associative array (`declare -A`) — macOS ships
# bash 3.2 (GPLv2, no assoc-array support), and this script is meant to be
# runnable locally for verification, not just on the ubuntu-latest runner
# that actually invokes it in CI.
os_arch_for() {
  case "$1" in
    xindeler-voxygen-linux-x86_64.tar.gz) echo "linux x86_64" ;;
    xindeler-voxygen-linux-aarch64.tar.gz) echo "linux aarch64" ;;
    xindeler-voxygen-macos-arm64.dmg|xindeler-voxygen-macos-arm64-unsigned.dmg) echo "macos arm64" ;;
    xindeler-voxygen-macos-x86_64.dmg|xindeler-voxygen-macos-x86_64-unsigned.dmg) echo "macos x86_64" ;;
    xindeler-voxygen-windows-x86_64.zip) echo "windows x86_64" ;;
    xindeler-voxygen-windows-aarch64.zip) echo "windows aarch64" ;;
    *) echo "" ;;
  esac
}

platforms_json="[]"
for file in "$PACKAGES_DIR"/xindeler-voxygen-*; do
  [ -f "$file" ] || continue
  name="$(basename "$file")"
  os_arch="$(os_arch_for "$name")"
  if [ -z "$os_arch" ]; then
    echo "generate-manifest.sh: unrecognized package name '$name', skipping" >&2
    continue
  fi
  os="${os_arch% *}"
  arch="${os_arch#* }"
  size="$(stat -c%s "$file" 2>/dev/null || stat -f%z "$file")"
  sha256="$(sha256sum "$file" 2>/dev/null | cut -d' ' -f1 || shasum -a 256 "$file" | cut -d' ' -f1)"
  # Only meaningful for macOS today (Linux/Windows aren't signed at all in
  # this phase) — null rather than a misleading "true" for those.
  signed="null"
  if [ "$os" = "macos" ]; then
    signed="true"
    [[ "$name" == *-unsigned.* ]] && signed="false"
  fi
  platforms_json="$(jq -c \
    --arg os "$os" --arg arch "$arch" --arg file "$name" \
    --argjson size "$size" --arg sha256 "$sha256" --argjson signed "$signed" \
    '. + [{os: $os, arch: $arch, file: $file, size: $size, sha256: $sha256, signed: $signed}]' \
    <<<"$platforms_json")"
done

assets_size=0
assets_sha256=""
if [ -f "$PACKAGES_DIR/assets.tar.gz" ]; then
  assets_size="$(stat -c%s "$PACKAGES_DIR/assets.tar.gz" 2>/dev/null || stat -f%z "$PACKAGES_DIR/assets.tar.gz")"
  assets_sha256="$(sha256sum "$PACKAGES_DIR/assets.tar.gz" 2>/dev/null | cut -d' ' -f1 || shasum -a 256 "$PACKAGES_DIR/assets.tar.gz" | cut -d' ' -f1)"
fi

jq -n \
  --arg version "$VERSION" \
  --arg released_at "$RELEASED_AT" \
  --argjson platforms "$platforms_json" \
  --arg assets_file "assets.tar.gz" \
  --argjson assets_size "$assets_size" \
  --arg assets_sha256 "$assets_sha256" \
  '{
    version: $version,
    released_at: $released_at,
    assets: {file: $assets_file, size: $assets_size, sha256: $assets_sha256},
    platforms: $platforms
  }' > "$OUTPUT_FILE"

echo "Wrote $OUTPUT_FILE:"
cat "$OUTPUT_FILE"
