#!/bin/sh
# Build rustflow.app (menu bar app; Dock icon only while the screen window is open) into /Applications.
set -eu
cd "$(dirname "$0")/../.."
cargo build --release
VERSION=$(cargo pkgid -p rustflow | sed 's/.*[#@]//')
APP="/Applications/rustflow.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp target/release/rustflow "$APP/Contents/MacOS/rustflow"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>rustflow</string>
  <key>CFBundleDisplayName</key><string>rustflow</string>
  <key>CFBundleIdentifier</key><string>dev.rustflow.Rustflow</string>
  <key>CFBundleExecutable</key><string>rustflow</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/> <key>NSLocalNetworkUsageDescription</key><string>rustflow connects to your other computer on the local network to share the clipboard.</string>
</dict>
</plist>
PLIST
# A stable identity keeps macOS permissions across rebuilds
# (create it once with make-signing-cert.sh); otherwise sign ad hoc.
IDENTITY="rustflow local signing"
if security find-certificate -c "$IDENTITY" >/dev/null 2>&1; then
  codesign --force --sign "$IDENTITY" --identifier dev.rustflow.Rustflow "$APP"
else
  echo "warning: \"$IDENTITY\" not found, signing ad hoc (permissions reset on every rebuild)"
  codesign --force --sign - "$APP" >/dev/null 2>&1 || true
fi
echo "Built $APP"
