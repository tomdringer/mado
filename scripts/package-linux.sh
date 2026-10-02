#!/usr/bin/env bash
# Package Linux release files from binaries that are already built.
# Usage: scripts/package-linux.sh <version> [bin-dir]
#   e.g. cargo build --release && scripts/package-linux.sh 0.0.4
# Writes to dist/:
#   mado-<version>-linux-<arch>.tar.gz   binaries + install.sh (installs to ~/.local)
#   mado_<version>_<debarch>.deb         installs to /usr/lib/mado, mado on PATH
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${1:?usage: $0 <version> [bin-dir]}"
BIN_DIR="${2:-target/release}"
BINS="mado mado-webview mado-claude"

DEB_ARCH=$(dpkg --print-architecture)
case "$DEB_ARCH" in
    amd64) ARCH=x86_64 ;;
    arm64) ARCH=aarch64 ;;
    *) echo "unsupported architecture: $DEB_ARCH" >&2; exit 1 ;;
esac
# Debian sorts "~" before anything, so 0.0.4~rc1 comes before 0.0.4.
DEB_VERSION="${VERSION//-/\~}"

OUT=dist
rm -rf "$OUT"
mkdir -p "$OUT"

# ── Tarball ───────────────────────────────────────────────────────────────────
NAME="mado-$VERSION-linux-$ARCH"
STAGE="$OUT/$NAME"
mkdir -p "$STAGE"
for b in $BINS; do
    install -m 755 "$BIN_DIR/$b" "$STAGE/$b"
    strip "$STAGE/$b"
done
install -m 755 packaging/linux/install.sh "$STAGE/install.sh"
install -m 644 packaging/linux/mado.desktop "$STAGE/mado.desktop"
install -m 644 packaging/linux/mado-512.png "$STAGE/mado.png"
install -m 644 LICENSE "$STAGE/LICENSE"
tar -C "$OUT" -czf "$OUT/$NAME.tar.gz" "$NAME"

# ── .deb ──────────────────────────────────────────────────────────────────────
ROOT="$OUT/deb-root"
mkdir -p "$ROOT/DEBIAN" "$ROOT/usr/lib/mado" "$ROOT/usr/bin" \
         "$ROOT/usr/share/applications" "$ROOT/usr/share/icons/hicolor/512x512/apps" \
         "$ROOT/usr/share/doc/mado"
for b in $BINS; do
    install -m 755 "$STAGE/$b" "$ROOT/usr/lib/mado/$b"
done
# mado finds mado-webview next to its real path, so symlinking onto PATH is safe.
ln -s ../lib/mado/mado        "$ROOT/usr/bin/mado"
ln -s ../lib/mado/mado-claude "$ROOT/usr/bin/mado-claude"
# Full path, so the menu can't pick up another "mado" earlier on PATH.
sed 's|^Exec=mado$|Exec=/usr/lib/mado/mado|' packaging/linux/mado.desktop > "$ROOT/usr/share/applications/mado.desktop"
chmod 644 "$ROOT/usr/share/applications/mado.desktop"
install -m 644 packaging/linux/mado-512.png "$ROOT/usr/share/icons/hicolor/512x512/apps/mado.png"
install -m 644 LICENSE "$ROOT/usr/share/doc/mado/copyright"

# Work out library dependencies from the binaries themselves.
SHLIB_DIR=$(mktemp -d)
mkdir -p "$SHLIB_DIR/debian"
echo "Source: mado" > "$SHLIB_DIR/debian/control"
DEPENDS=$(cd "$SHLIB_DIR" && dpkg-shlibdeps -O $(for b in $BINS; do echo "$OLDPWD/$ROOT/usr/lib/mado/$b"; done) \
    | sed -n 's/^shlibs:Depends=//p')
rm -rf "$SHLIB_DIR"

cat > "$ROOT/DEBIAN/control" <<EOF
Package: mado
Version: $DEB_VERSION
Architecture: $DEB_ARCH
Maintainer: Tom Dringer <47415157+tomdringer@users.noreply.github.com>
Depends: $DEPENDS
Section: x11
Priority: optional
Homepage: https://github.com/tomdringer/mado
Description: Terminal multiplexer with sidebar plugins
 Mado is a terminal multiplexer with split panes, workspaces, Tasku task
 integration, sidebar plugins and a built-in browser panel.
EOF

find "$ROOT" -type d -exec chmod 755 {} +
dpkg-deb --root-owner-group --build "$ROOT" "$OUT/mado_${DEB_VERSION}_${DEB_ARCH}.deb" >/dev/null
rm -rf "$ROOT" "$STAGE"

ls -la "$OUT"
