#!/usr/bin/env bash
# Mado release script
# Usage: ./scripts/release.sh <version>
# e.g.:  ./scripts/release.sh 0.0.4
set -e
cd "$(dirname "$0")/.."

WIKI_DIR="$HOME/Sites/mado-wiki"
TAP_DIR="$HOME/Sites/homebrew-tap"

# ── Args ──────────────────────────────────────────────────────────────────────

if [ -z "$1" ]; then
    echo "Usage: $0 <version>  (e.g. 0.0.4)"
    exit 1
fi
VERSION="$1"

# ── Clean working tree check ──────────────────────────────────────────────────

if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "error: mado working tree has uncommitted changes — commit or stash first"
    exit 1
fi

# ── Wiki: check for missing plugin pages ─────────────────────────────────────

echo ""
echo "Checking wiki for missing plugin pages..."
PLUGINS=$(grep -r '"type".*"pixel"\|"type".*"terminal"' ~/.config/mado/plugins/*.toml 2>/dev/null \
    | grep -o '"id".*"[^"]*"' | grep -o '"[^"]*"$' | tr -d '"' || true)

MISSING_PAGES=0
for plugin in $PLUGINS; do
    page="$WIKI_DIR/Plugins-${plugin}.md"
    # Check for a dedicated page or a section in Plugins.md
    if [ ! -f "$page" ] && ! grep -qi "## ${plugin}" "$WIKI_DIR/Plugins.md" 2>/dev/null; then
        echo "  warning: no wiki page found for plugin '${plugin}'"
        MISSING_PAGES=1
    fi
done

if [ "$MISSING_PAGES" -eq 1 ]; then
    echo ""
    read -p "Continue without wiki pages for the above plugins? [y/N] " yn
    [[ "$yn" =~ ^[Yy]$ ]] || exit 1
fi

# ── Wiki: commit any pending changes ─────────────────────────────────────────

echo ""
cd "$WIKI_DIR"
if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "Wiki has uncommitted changes:"
    git status --short
    echo ""
    read -p "Wiki commit message: " wiki_msg
    git add -A
    git commit -m "$wiki_msg"
fi

# ── Bump version in Cargo.toml ────────────────────────────────────────────────

cd "$(dirname "$0")/.."
echo ""
echo "Bumping version to ${VERSION}..."
sed -i '' "s/^version = \".*\"/version = \"${VERSION}\"/" Cargo.toml

# ── CHANGELOG ─────────────────────────────────────────────────────────────────

DATE=$(date +%Y-%m-%d)
CHANGELOG_ENTRY="## [${VERSION}] - ${DATE}"

echo ""
echo "Opening CHANGELOG.md — add your entry under the top line, save and close."
echo "Press enter to open..."
read -r
${EDITOR:-nano} CHANGELOG.md

# Verify entry was added
if ! grep -q "\[${VERSION}\]" CHANGELOG.md; then
    echo "error: no [${VERSION}] entry found in CHANGELOG.md — aborting"
    exit 1
fi

# ── Commit version bump + changelog ──────────────────────────────────────────

echo ""
read -p "Commit message (default: 'v${VERSION}'): " commit_msg
commit_msg="${commit_msg:-v${VERSION}}"

git add Cargo.toml CHANGELOG.md Mado.app/Contents/Info.plist
git commit -m "$commit_msg"

# ── Build, notarize, create DMG ───────────────────────────────────────────────

echo ""
echo "Building and notarizing..."
./scripts/bundle.sh

# ── GitHub release ────────────────────────────────────────────────────────────

echo ""
echo "Creating GitHub release v${VERSION}..."

# Extract changelog entry for this version as release notes
NOTES=$(awk "/^\#\# \[${VERSION}\]/{found=1; next} found && /^\#\# \[/{exit} found{print}" CHANGELOG.md)

gh release create "v${VERSION}" Mado.dmg \
    --title "v${VERSION}" \
    --notes "$NOTES" \
    --repo tomdringer/mado

# ── Homebrew tap ──────────────────────────────────────────────────────────────

echo ""
echo "Updating Homebrew tap..."
SHA=$(shasum -a 256 Mado.dmg | awk '{print $1}')

cd "$TAP_DIR"
git pull --rebase origin main

sed -i '' "s/version \".*\"/version \"${VERSION}\"/" Casks/mado.rb
sed -i '' "s/sha256 \".*\"/sha256 \"${SHA}\"/" Casks/mado.rb

git add Casks/mado.rb
git commit -m "Update mado cask to v${VERSION}"
git push origin main

# ── Tag and push mado ─────────────────────────────────────────────────────────

echo ""
cd "$(dirname "$0")/.."
git tag "v${VERSION}"
git push origin main
git push origin "v${VERSION}"

# ── Tag and push wiki ─────────────────────────────────────────────────────────

echo ""
echo "Tagging wiki..."
cd "$WIKI_DIR"
git push origin master 2>/dev/null || git push origin main 2>/dev/null
git tag "v${VERSION}"
git push origin "v${VERSION}"

# ── Done ──────────────────────────────────────────────────────────────────────

echo ""
echo "✓ Released v${VERSION}"
echo "  https://github.com/tomdringer/mado/releases/tag/v${VERSION}"
