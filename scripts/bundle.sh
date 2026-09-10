#!/usr/bin/env bash
set -e
cd "$(dirname "$0")/.."

cargo build --release
cp target/release/mado Mado.app/Contents/MacOS/mado
codesign --force --deep --options runtime \
  --sign "Developer ID Application: Tom Dringer (FAG2987V9S)" \
  Mado.app

# Keep the CLI symlink pointing at the app binary.
# First-time setup: sudo ln -sf "$(pwd)/Mado.app/Contents/MacOS/mado" /usr/local/bin/mado
if [ -L /usr/local/bin/mado ]; then
  ln -sf "$(pwd)/Mado.app/Contents/MacOS/mado" /usr/local/bin/mado
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

pkill -x mado 2>/dev/null || true
sleep 0.3
open Mado.app
