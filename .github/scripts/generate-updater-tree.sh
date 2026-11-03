#!/bin/bash
# Build the static "updater/" tree that xindeler-updater's remozipsy-based
# sync reads (Profile::download_url() etc. in that repo's client/src/profiles.rs).
# See docs/design/specs/2026-09-09-updater-sync-protocol-contract.md for the
# full contract this implements.
#
# Usage: generate-updater-tree.sh <version> <channel> <packages_dir> <output_dir>
#   packages_dir must contain the *-updater.zip files the platform jobs
#   upload (see build.yml), plus the Windows manual package (already a real
#   .zip in the right flat shape, reused as-is).
#
# remozipsy requires the remote file to be a genuine ZIP (it reads the ZIP
# central directory via HTTP range requests) — this is why the mapping below
# points at *-updater.zip / the Windows .zip, never the Linux .tar.gz or the
# macOS .dmg that manual downloads use.
set -euo pipefail

VERSION="$1"
CHANNEL="$2"
PACKAGES_DIR="$3"
OUTPUT_DIR="$4"

# os/arch use Rust's own std::env::consts::OS/ARCH naming (xindeler-updater
# reads these directly) — NOT manifest.json's arch, which spells Apple
# Silicon "arm64" (Apple's own naming) instead of Rust's "aarch64".
source_for() {
  case "$1" in
    "linux x86_64") echo "xindeler-voxygen-linux-x86_64-updater.zip" ;;
    "linux aarch64") echo "xindeler-voxygen-linux-aarch64-updater.zip" ;;
    "macos aarch64") echo "xindeler-voxygen-macos-aarch64-updater.zip" ;;
    "macos x86_64") echo "xindeler-voxygen-macos-x86_64-updater.zip" ;;
    "windows x86_64")
      # Reused directly — already a real .zip in the right flat shape
      # (binary + assets/ at the root), no separate updater build needed.
      if [ -f "$PACKAGES_DIR/xindeler-voxygen-windows-x86_64.zip" ]; then
        echo "xindeler-voxygen-windows-x86_64.zip"
      else
        echo "xindeler-voxygen-windows-x86_64-unsigned.zip"
      fi
      ;;
    *) echo "" ;;
  esac
}

for os_arch in "linux x86_64" "linux aarch64" "macos aarch64" "macos x86_64" "windows x86_64"; do
  os="${os_arch% *}"
  arch="${os_arch#* }"
  source_name="$(source_for "$os_arch")"
  source_file="$PACKAGES_DIR/$source_name"

  if [ ! -f "$source_file" ]; then
    echo "generate-updater-tree.sh: missing '$source_name' for $os/$arch, skipping" >&2
    continue
  fi

  mkdir -p "$OUTPUT_DIR/latest/$os/$arch"
  cp "$source_file" "$OUTPUT_DIR/latest/$os/$arch/$CHANNEL"

  mkdir -p "$OUTPUT_DIR/version/$os/$arch"
  printf '%s' "$VERSION" > "$OUTPUT_DIR/version/$os/$arch/$CHANNEL"

  mkdir -p "$OUTPUT_DIR/channels/$os"
  printf '["%s"]' "$CHANNEL" > "$OUTPUT_DIR/channels/$os/$arch"
done

mkdir -p "$OUTPUT_DIR/api"
# Matches xindeler-updater's SUPPORTED_SERVER_API_VERSION const (client/src/consts.rs).
printf '1' > "$OUTPUT_DIR/api/version"

echo "Wrote updater tree to $OUTPUT_DIR:"
find "$OUTPUT_DIR" -type f
