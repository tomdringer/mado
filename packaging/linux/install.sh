#!/usr/bin/env bash
# Install Mado from the release tarball into ~/.local (no sudo needed).
#   ./install.sh            install or upgrade
#   ./install.sh --remove   uninstall
set -euo pipefail
cd "$(dirname "$0")"

PREFIX="${PREFIX:-$HOME/.local}"
LIB="$PREFIX/lib/mado"
BIN="$PREFIX/bin"
APPS="$PREFIX/share/applications"
ICONS="$PREFIX/share/icons/hicolor/512x512/apps"

if [ "${1:-}" = "--remove" ]; then
    rm -rf "$LIB"
    rm -f "$BIN/mado" "$BIN/mado-claude" "$APPS/mado.desktop" "$ICONS/mado.png"
    echo "Mado removed. Your settings in ~/.config/mado are untouched."
    exit 0
fi

mkdir -p "$LIB" "$BIN" "$APPS" "$ICONS"
# Rename into place rather than overwrite, so a running Mado isn't disturbed.
for b in mado mado-webview mado-claude; do
    install -m 755 "$b" "$LIB/$b.new"
    mv -f "$LIB/$b.new" "$LIB/$b"
done
ln -sf "$LIB/mado" "$BIN/mado"
ln -sf "$LIB/mado-claude" "$BIN/mado-claude"
install -m 644 mado.png "$ICONS/mado.png"
sed "s|^Exec=mado$|Exec=$LIB/mado|" mado.desktop > "$APPS/mado.desktop"

echo "Mado installed to $LIB"
case ":$PATH:" in
    *":$BIN:"*) ;;
    *) echo "Add $BIN to your PATH to run 'mado' from a terminal." ;;
esac
if ! ldconfig -p 2>/dev/null | grep libwebkit2gtk-4.1 >/dev/null; then
    echo "The browser panel needs WebKitGTK 4.1:"
    echo "  Debian/Ubuntu: sudo apt install libwebkit2gtk-4.1-0"
    echo "  Fedora:        sudo dnf install webkit2gtk4.1"
    echo "  Arch:          sudo pacman -S webkit2gtk-4.1"
fi
