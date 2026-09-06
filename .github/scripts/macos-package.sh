#!/bin/bash
# Package a built xindeler-voxygen binary into a minimal .app bundle, then a
# .dmg. Signing/notarization (if secrets are configured) happen in the
# workflow around this script — this script only knows how to assemble the
# bundle shape, not about Apple credentials.
#
# Usage: macos-package.sh <binary_path> <arch_label> <version>
#   arch_label: "arm64" or "x86_64" — used in the bundle id and dmg filename.
set -euo pipefail

BINARY_PATH="$1"
ARCH_LABEL="$2"
VERSION="$3"

APP_NAME="Xindeler"
APP_DIR="${APP_NAME}.app"
DMG_NAME="xindeler-voxygen-macos-${ARCH_LABEL}.dmg"

rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
mkdir -p "$APP_DIR/Contents/Resources"

cp "$BINARY_PATH" "$APP_DIR/Contents/MacOS/xindeler-voxygen"
chmod +x "$APP_DIR/Contents/MacOS/xindeler-voxygen"

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

hdiutil create -volname "$APP_NAME" -srcfolder "$APP_DIR" -ov -format UDZO "$DMG_NAME"

echo "Packaged: $DMG_NAME"
