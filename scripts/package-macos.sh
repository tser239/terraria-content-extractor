#!/usr/bin/env bash
# Build a universal (Intel + Apple Silicon) macOS .app bundle and zip it.
#
# Usage:
#   ./scripts/package-macos.sh
#   BUNDLE_ID=com.example.tool ./scripts/package-macos.sh
#
# Must run on macOS: the icon conversion (sips/iconutil), lipo and ditto are
# Apple tooling.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP_NAME="Terraria Content Extractor"
BUNDLE_ID="${BUNDLE_ID:-dev.terraria-content-extractor}"
BINARY_NAME="terraria-content-extractor"
VERSION="$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)"
APP_DIR="$ROOT/dist/$APP_NAME.app"
ZIP="$ROOT/dist/terraria-content-extractor-${VERSION}-universal-macos.zip"
ICON_SOURCE="$ROOT/assets/icon.png"
TARGETS=(x86_64-apple-darwin aarch64-apple-darwin)

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "error: $1 is required but not installed" >&2
        exit 1
    }
}

if [[ "$(uname -s)" != Darwin ]]; then
    echo "error: this script must run on macOS" >&2
    exit 1
fi

need cargo
need sips
need iconutil
need lipo
need ditto
need shasum

if command -v rustup >/dev/null 2>&1; then
    for target in "${TARGETS[@]}"; do
        rustup target add "$target" >/dev/null
    done
fi

echo "==> cargo build (x86_64 and arm64)"
for target in "${TARGETS[@]}"; do
    cargo build --release --locked --target "$target" --bin "$BINARY_NAME"
done

echo "==> lipo universal binary"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
UNIVERSAL="$WORK/$BINARY_NAME"
lipo -create -output "$UNIVERSAL" \
    "target/x86_64-apple-darwin/release/$BINARY_NAME" \
    "target/aarch64-apple-darwin/release/$BINARY_NAME"
lipo -info "$UNIVERSAL"

echo "==> assembling $APP_NAME.app"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
install -m 755 "$UNIVERSAL" "$APP_DIR/Contents/MacOS/$BINARY_NAME"
install -m 644 LICENSE README.md "$APP_DIR/Contents/Resources/"
printf 'APPL????' > "$APP_DIR/Contents/PkgInfo"

ICONSET="$WORK/AppIcon.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$ICON_SOURCE" \
        --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    sips -z "$((size * 2))" "$((size * 2))" "$ICON_SOURCE" \
        --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP_DIR/Contents/Resources/AppIcon.icns"
rm -rf "$ICONSET"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key>
    <string>$BINARY_NAME</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

if command -v codesign >/dev/null 2>&1; then
    echo "==> ad-hoc signing"
    codesign --force --deep --sign - "$APP_DIR" >/dev/null 2>&1 \
        || echo "warning: ad-hoc signing failed; the app may need a right-click Open" >&2
fi

echo "==> zip"
rm -f "$ZIP"
ditto -c -k --sequesterRsrc --keepParent "$APP_DIR" "$ZIP"
(
    cd "$ROOT/dist"
    shasum -a 256 "$(basename "$ZIP")" > SHA256SUMS-macos-universal
)

echo
echo "Created in $ROOT/dist:"
echo "  $APP_NAME.app"
echo "  $(basename "$ZIP")"
echo "  SHA256SUMS-macos-universal"
