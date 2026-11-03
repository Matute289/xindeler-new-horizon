#!/bin/bash
# Package a built xindeler-voxygen binary into a minimal .app bundle, then a
# .dmg. Signing/notarization (if secrets are configured) happen in the
# workflow around this script — this script only knows how to assemble the
# bundle shape, not about Apple credentials.
#
# Usage: macos-package.sh <binary_path> <arch_label> <version> <assets_dir>
#   arch_label: "arm64" or "x86_64" — used in the bundle id and dmg filename.
#   assets_dir: extracted assets/ tree to bundle into the app (NH-60 — a
#     manual macOS download previously shipped without it, same crash as
#     Windows/Linux; see ../../docs/design/specs/2026-09-09-updater-sync-protocol-contract.md).
set -euo pipefail

BINARY_PATH="$1"
ARCH_LABEL="$2"
VERSION="$3"
ASSETS_DIR="$4"

APP_NAME="Xindeler"
APP_DIR="${APP_NAME}.app"
DMG_NAME="xindeler-voxygen-macos-${ARCH_LABEL}.dmg"

rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
mkdir -p "$APP_DIR/Contents/Resources"

cp "$BINARY_PATH" "$APP_DIR/Contents/MacOS/xindeler-voxygen"
chmod +x "$APP_DIR/Contents/MacOS/xindeler-voxygen"

# Assets go inside Contents/MacOS/ (sibling to the executable), not the more
# idiomatic Contents/Resources/ — common/assets/src/lib.rs's existing search
# order already checks "the executable's own directory + assets", proven in
# production on Linux/Windows. Contents/Resources/ would need a new
# macOS-specific search rule in shipped engine code for no real benefit.
cp -r "$ASSETS_DIR" "$APP_DIR/Contents/MacOS/assets"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>${APP_NAME}</string>
	<key>CFBundleDisplayName</key>
	<string>${APP_NAME}</string>
	<key>CFBundleIdentifier</key>
	<string>com.xindeler.voxygen</string>
	<key>CFBundleVersion</key>
	<string>${VERSION#v}</string>
	<key>CFBundleShortVersionString</key>
	<string>${VERSION#v}</string>
	<key>CFBundleExecutable</key>
	<string>xindeler-voxygen</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>LSMinimumSystemVersion</key>
	<string>10.15</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
PLIST

# No icon yet (NH-58 didn't scope one) — macOS falls back to a generic app
# icon, which is fine for an alpha.

# Without an explicit -size, hdiutil auto-sizes the intermediate disk image
# from the srcfolder, but that estimate runs short in CI often enough to be
# unusable: two v0.25.x release runs both failed with "No space left on
# device" while the actual runner disk had 90+ GB free -- the failure was
# the temp *volume's* internal capacity, not the host filesystem. Compute
# the real size and pad it generously rather than trust the auto-estimate.
APP_SIZE_MB="$(du -sm "$APP_DIR" | cut -f1)"
DMG_SIZE_MB=$((APP_SIZE_MB + APP_SIZE_MB / 2 + 20))
hdiutil create -volname "$APP_NAME" -srcfolder "$APP_DIR" -ov -format UDZO -size "${DMG_SIZE_MB}m" "$DMG_NAME"

echo "Packaged: $DMG_NAME"
