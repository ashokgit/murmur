#!/bin/bash
set -e

# Colors for output
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

echo -e "${BLUE}=== murmur macOS Packager ===${NC}"

# 1. Check prerequisites
echo -e "\n${BLUE}[1/5] Checking macOS packaging tools...${NC}"
for cmd in sips iconutil hdiutil; do
    if ! command -v $cmd &> /dev/null; then
        echo -e "${RED}Error: Required command '$cmd' is not available. This script must be run on macOS.${NC}"
        exit 1
    fi
done
echo -e "✓ Found sips, iconutil, and hdiutil."

# 2. Build release binary
echo -e "\n${BLUE}[2/5] Compiling murmur in release mode...${NC}"
cargo build --release

# 3. Create App Icon (.icns)
echo -e "\n${BLUE}[3/5] Generating Apple Iconset from logo.png...${NC}"
if [ ! -f "logo.png" ]; then
    echo -e "${RED}Error: logo.png not found in project root. Please ensure the app logo exists.${NC}"
    exit 1
fi

ICONSET_DIR="target/icon.iconset"
mkdir -p "$ICONSET_DIR"

sips -s format png -z 16 16     logo.png --out "$ICONSET_DIR/icon_16x16.png"
sips -s format png -z 32 32     logo.png --out "$ICONSET_DIR/icon_16x16@2x.png"
sips -s format png -z 32 32     logo.png --out "$ICONSET_DIR/icon_32x32.png"
sips -s format png -z 64 64     logo.png --out "$ICONSET_DIR/icon_32x32@2x.png"
sips -s format png -z 128 128   logo.png --out "$ICONSET_DIR/icon_128x128.png"
sips -s format png -z 256 256   logo.png --out "$ICONSET_DIR/icon_128x128@2x.png"
sips -s format png -z 256 256   logo.png --out "$ICONSET_DIR/icon_256x256.png"
sips -s format png -z 512 512   logo.png --out "$ICONSET_DIR/icon_256x256@2x.png"
sips -s format png -z 512 512   logo.png --out "$ICONSET_DIR/icon_512x512.png"
sips -s format png -z 1024 1024 logo.png --out "$ICONSET_DIR/icon_512x512@2x.png"

# Build ICNS
APP_DIR="target/Murmur.app"
mkdir -p "$APP_DIR/Contents/MacOS"
mkdir -p "$APP_DIR/Contents/Resources"

iconutil -c icns "$ICONSET_DIR" -o "$APP_DIR/Contents/Resources/icon.icns"
rm -rf "$ICONSET_DIR"
echo -e "✓ Created target/Murmur.app/Contents/Resources/icon.icns"

# 4. Construct macOS App Bundle structure
echo -e "\n${BLUE}[4/5] Constructing Murmur.app Bundle...${NC}"

# Copy binary
cp target/release/murmur "$APP_DIR/Contents/MacOS/murmur"
chmod +x "$APP_DIR/Contents/MacOS/murmur"

# Create launcher
cat << 'EOF' > "$APP_DIR/Contents/MacOS/murmur-launcher"
#!/bin/bash
DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"

# Propagate environment path to include standard Homebrew locations for cloudflared
export PATH="/opt/homebrew/bin:/usr/local/bin:/opt/homebrew/sbin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"

# Check if cloudflared is installed to show a nice alert on launch if not
if ! command -v cloudflared &> /dev/null; then
    osascript -e 'display dialog "Warning: cloudflared was not found in your PATH.\n\nmurmur will run, but it won\u0027t be able to expose your social feed or sync with other peers unless cloudflared is installed.\n\nYou can install it via: brew install cloudflared" buttons {"OK"} default button "OK" with icon caution'
fi

# Run murmur inside a new terminal window
osascript -e "tell application \"Terminal\"
    activate
    do script \"exec '$DIR/murmur'\"
end tell"
EOF

chmod +x "$APP_DIR/Contents/MacOS/murmur-launcher"

# Create Info.plist
cat << 'EOF' > "$APP_DIR/Contents/Info.plist"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>murmur-launcher</string>
    <key>CFBundleIconFile</key>
    <string>icon.icns</string>
    <key>CFBundleIdentifier</key>
    <string>com.murmur.social</string>
    <key>CFBundleName</key>
    <string>Murmur</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>1.0.0</string>
    <key>CFBundleSignature</key>
    <string>????</string>
    <key>LSMinimumSystemVersion</key>
    <string>10.13</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
EOF

echo -e "✓ Created Murmur.app Bundle."

# 5. Pack into DMG
echo -e "\n${BLUE}[5/5] Packaging into Murmur.dmg...${NC}"
DMG_WORK_DIR="target/dmg_workspace"
rm -rf "$DMG_WORK_DIR"
mkdir -p "$DMG_WORK_DIR"

# Copy App to DMG workspace
cp -R "$APP_DIR" "$DMG_WORK_DIR/"

# Create symlink to /Applications
ln -s /Applications "$DMG_WORK_DIR/Applications"

# Build DMG
rm -f Murmur.dmg
hdiutil create -volname "Murmur" -srcfolder "$DMG_WORK_DIR" -ov -format UDZO Murmur.dmg

# Clean up
rm -rf "$DMG_WORK_DIR"

echo -e "\n${GREEN}=== murmur Packaging Complete! ===${NC}"
echo -e "Generated: ${YELLOW}Murmur.dmg${NC} in project root."
echo -e "You can now open the DMG file, drag Murmur.app to Applications, and share it."
