#!/usr/bin/env bash
# Build the portable Linux release artifacts:
#   - terraria-content-extractor-<version>-x86_64-linux-gnu.tar.gz
#   - terraria-content-extractor_<version>_amd64.deb
#   - Terraria_Content_Extractor-<version>-x86_64.AppImage
#
# Usage:
#   ./scripts/package-linux.sh              build inside Docker (Ubuntu 22.04)
#   ./scripts/package-linux.sh --native     build with the local toolchain
#
# The Docker path keeps a glibc 2.35 baseline. The native path uses whatever
# glibc the host has (Arch here: 2.44), so those artifacts only run on very
# recent distributions.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MODE="${1:-}"
case "$MODE" in
    "" | --docker) MODE=docker ;;
    --native) MODE=native ;;
    --inside-container) MODE=inside ;;
    *)
        echo "usage: $0 [--docker|--native]" >&2
        exit 2
        ;;
esac

VERSION="$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)"
if [[ -z "$VERSION" ]]; then
    echo "error: could not read the package version from Cargo.toml" >&2
    exit 1
fi

IMAGE="terraria-content-extractor-linux-builder"
CARGO_VOLUME="terraria-content-extractor-cargo"
TARGET_VOLUME="terraria-content-extractor-target"
CACHE_VOLUME="terraria-content-extractor-cache"

if [[ "$MODE" == docker ]]; then
    if ! command -v docker >/dev/null 2>&1; then
        echo "error: docker is not installed; rerun with --native" >&2
        exit 1
    fi
    if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
        echo "==> Building $IMAGE (first run only)"
        docker build -t "$IMAGE" -f scripts/docker/linux-builder.Dockerfile scripts/docker
    fi
    echo "==> Building Linux artifacts inside Docker"
    docker run --rm \
        -v "$ROOT:/src" \
        -v "$CARGO_VOLUME:/usr/local/cargo" \
        -v "$TARGET_VOLUME:/build/target" \
        -v "$CACHE_VOLUME:/root/.cache/terraria-content-extractor" \
        -e CARGO_TARGET_DIR=/build/target \
        -e HOME=/root \
        -e HOST_UID="$(id -u)" \
        -e HOST_GID="$(id -g)" \
        "$IMAGE" /src/scripts/package-linux.sh --inside-container
    exit 0
fi

# --- from here on we run on the build host (native or inside the container) ---

CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
BINARY="$CARGO_TARGET_DIR/release/terraria-content-extractor"
DIST="$ROOT/dist"
STAGE="$DIST/stage-linux"
PACKAGE="terraria-content-extractor-$VERSION-x86_64-linux-gnu"
DEB="terraria-content-extractor_${VERSION}_amd64.deb"
APPIMAGE="Terraria_Content_Extractor-${VERSION}-x86_64.AppImage"
DESKTOP="assets/terraria-content-extractor.desktop"
ICON="assets/icon.png"

APPIMAGETOOL_URL="https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage"
APPIMAGETOOL_SHA256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "error: $1 is required but not installed" >&2
        exit 1
    }
}

need cargo
need tar
need gzip
need sha256sum
need curl
need dpkg-deb

mkdir -p "$DIST"
rm -rf "$STAGE"
mkdir -p "$STAGE"

echo "==> cargo build --release"
cargo build --release --locked --bin terraria-content-extractor

# --- tarball -----------------------------------------------------------------

echo "==> tarball"
TAR_STAGE="$STAGE/tar/$PACKAGE"
mkdir -p "$TAR_STAGE"
install -m 755 "$BINARY" "$TAR_STAGE/terraria-content-extractor"
install -m 644 LICENSE "$TAR_STAGE/LICENSE"
install -m 644 README.md "$TAR_STAGE/README.md"
install -m 644 "$DESKTOP" "$TAR_STAGE/terraria-content-extractor.desktop"
install -m 644 "$ICON" "$TAR_STAGE/terraria-content-extractor.png"

tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
    -cf - -C "$STAGE/tar" "$PACKAGE" | gzip -n > "$DIST/$PACKAGE.tar.gz"

# --- .deb --------------------------------------------------------------------

echo "==> debian package"
DEB_STAGE="$STAGE/deb"
install -Dm755 "$BINARY" "$DEB_STAGE/usr/bin/terraria-content-extractor"
install -Dm644 "$DESKTOP" "$DEB_STAGE/usr/share/applications/terraria-content-extractor.desktop"
install -Dm644 "$ICON" \
    "$DEB_STAGE/usr/share/icons/hicolor/128x128/apps/terraria-content-extractor.png"
install -Dm644 README.md "$DEB_STAGE/usr/share/doc/terraria-content-extractor/README.md"
install -Dm644 LICENSE "$DEB_STAGE/usr/share/doc/terraria-content-extractor/copyright"
install -Dm644 assets/README.md \
    "$DEB_STAGE/usr/share/doc/terraria-content-extractor/icon-notice.md"

mkdir -p "$DEB_STAGE/DEBIAN"
cat > "$DEB_STAGE/DEBIAN/control" <<EOF
Package: terraria-content-extractor
Version: $VERSION
Section: utils
Priority: optional
Architecture: amd64
Maintainer: Terraria Content Extractor
Depends: libc6, libgl1, libegl1, libx11-6, libxcursor1, libxrandr2, libxi6, libxkbcommon0, libwayland-client0, libwayland-cursor0, libwayland-egl1
Recommends: xdg-desktop-portal
Description: Extract Terraria XNB/XWB assets into PNG, WAV and JSON
 Converts the assets shipped in Terraria's Content folder into ordinary files:
 textures, sound effects, font atlases and music. The same binary also has a
 headless --cli mode.
EOF

dpkg-deb --root-owner-group --build "$DEB_STAGE" "$DIST/$DEB" >/dev/null

# --- AppImage ----------------------------------------------------------------

echo "==> AppImage"
APPDIR="$STAGE/AppDir"
install -Dm755 "$BINARY" "$APPDIR/usr/bin/terraria-content-extractor"
install -Dm644 "$DESKTOP" \
    "$APPDIR/usr/share/applications/terraria-content-extractor.desktop"
install -Dm644 "$ICON" \
    "$APPDIR/usr/share/icons/hicolor/128x128/apps/terraria-content-extractor.png"
install -m 644 "$DESKTOP" "$APPDIR/terraria-content-extractor.desktop"
install -m 644 "$ICON" "$APPDIR/terraria-content-extractor.png"
ln -sf terraria-content-extractor.png "$APPDIR/.DirIcon"

cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/terraria-content-extractor" "$@"
EOF
chmod 755 "$APPDIR/AppRun"

TOOLS_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/terraria-content-extractor/tools"
APPIMAGETOOL="$TOOLS_DIR/appimagetool-x86_64.AppImage"
if ! echo "$APPIMAGETOOL_SHA256  $APPIMAGETOOL" | sha256sum -c --status 2>/dev/null; then
    echo "==> Downloading appimagetool 1.9.1"
    mkdir -p "$TOOLS_DIR"
    curl -fsSL --retry 3 -o "$APPIMAGETOOL.part" "$APPIMAGETOOL_URL"
    echo "$APPIMAGETOOL_SHA256  $APPIMAGETOOL.part" | sha256sum -c --status
    mv "$APPIMAGETOOL.part" "$APPIMAGETOOL"
    chmod 755 "$APPIMAGETOOL"
fi

ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 \
    "$APPIMAGETOOL" --no-appstream "$APPDIR" "$DIST/$APPIMAGE"

# --- checksums ---------------------------------------------------------------

echo "==> checksums"
(
    cd "$DIST"
    sha256sum "$PACKAGE.tar.gz" "$DEB" "$APPIMAGE" > SHA256SUMS-linux-x86_64
)

rm -rf "$STAGE"

if [[ -n "${HOST_UID:-}" && -n "${HOST_GID:-}" ]]; then
    chown -R "$HOST_UID:$HOST_GID" "$DIST"
fi

echo
echo "Created in $DIST:"
echo "  $PACKAGE.tar.gz"
echo "  $DEB"
echo "  $APPIMAGE"
echo "  SHA256SUMS-linux-x86_64"
