#!/usr/bin/env bash
set -e
cd "$(dirname "$0")/.."

cargo build --release
cp target/release/mado Mado.app/Contents/MacOS/mado
codesign --force --deep --sign - Mado.app

# Keep the CLI symlink pointing at the app binary.
# First-time setup: sudo ln -sf "$(pwd)/Mado.app/Contents/MacOS/mado" /usr/local/bin/mado
if [ -L /usr/local/bin/mado ]; then
  ln -sf "$(pwd)/Mado.app/Contents/MacOS/mado" /usr/local/bin/mado
fi

pkill -x mado 2>/dev/null || true
sleep 0.3
open Mado.app
