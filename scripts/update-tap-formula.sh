#!/usr/bin/env bash
# Point the Linux Homebrew formula at a release's Linux tarballs.
# Run after the "Linux release" workflow has attached them to the release.
# Usage: scripts/update-tap-formula.sh <version> [tap-dir]
#   e.g. scripts/update-tap-formula.sh 0.0.4
set -euo pipefail

VERSION="${1:?usage: $0 <version> [tap-dir]}"
TAP_DIR="${2:-$HOME/Sites/homebrew-tap}"
FORMULA="$TAP_DIR/Formula/mado.rb"
[ -f "$FORMULA" ] || { echo "error: $FORMULA not found (pass the tap dir)" >&2; exit 1; }

sha_for() {
    local url="https://github.com/tomdringer/mado/releases/download/v${VERSION}/mado-${VERSION}-linux-$1.tar.gz"
    curl -fsSL "$url" | shasum -a 256 | cut -d' ' -f1
}
SHA_X86=$(sha_for x86_64)
SHA_ARM=$(sha_for aarch64)

# perl -pi works the same on macOS and Linux (sed -i doesn't).
perl -pi -e "s/^  version \".*\"/  version \"${VERSION}\"/" "$FORMULA"
perl -pi -e "s/sha256 \"[0-9a-f]{64}\" # x86_64/sha256 \"${SHA_X86}\" # x86_64/" "$FORMULA"
perl -pi -e "s/sha256 \"[0-9a-f]{64}\" # aarch64/sha256 \"${SHA_ARM}\" # aarch64/" "$FORMULA"

echo "Formula updated to ${VERSION}:"
echo "  x86_64  ${SHA_X86}"
echo "  aarch64 ${SHA_ARM}"
echo "Review, then: cd $TAP_DIR && git commit -am \"Update mado formula to v${VERSION}\" && git push"
