#!/usr/bin/env bash
set -e
cd "$(dirname "$0")/.."

cargo build --release

# Kill any running Mado BEFORE overwriting the binary — writing to a running
# executable on macOS produces a 0-byte file.
osascript -e 'tell application "Mado" to quit' 2>/dev/null || true
sleep 0.5
pkill -9 -x mado 2>/dev/null || true
sleep 0.2

cp target/release/mado Mado.app/Contents/MacOS/mado
codesign --force --deep --options runtime \
  --sign "Developer ID Application: Tom Dringer (FAG2987V9S)" \
  Mado.app

# Keep the CLI symlink pointing at the installed app binary.
# First-time setup: sudo ln -sf /Applications/Mado.app/Contents/MacOS/mado /usr/local/bin/mado
if [ -L /usr/local/bin/mado ]; then
  ln -sf /Applications/Mado.app/Contents/MacOS/mado /usr/local/bin/mado
fi

# Build DMG with Applications symlink
mkdir -p dmg_staging
cp -r Mado.app dmg_staging/
ln -s /Applications dmg_staging/Applications
hdiutil create -volname "Mado" -srcfolder dmg_staging -ov -format UDZO Mado.dmg
rm -rf dmg_staging

# Notarize and staple
xcrun notarytool submit Mado.dmg --keychain-profile "mado-notary" --wait
xcrun stapler staple Mado.dmg

rm -rf /Applications/Mado.app && ditto Mado.app /Applications/Mado.app
open /Applications/Mado.app
