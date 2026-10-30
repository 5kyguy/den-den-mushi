#!/usr/bin/env bash
# Cut a den-den-mushi release.
#
#   scripts/release.sh
#
# Checks the tree, builds, tags v<version from Cargo.toml>, pushes the commit
# and the tag, then creates the GitHub release with:
#   - den-den-mushi-<version>-x86_64-linux.tar.gz  (binary + contrib + docs)
#   - PKGBUILD                                      (copy of packaging/PKGBUILD)
#   - SHA256SUMS
#
# Publishing the AUR package afterwards is a separate step:
#   scripts/aur-publish.sh
#
# This script never touches a running desktop. Users update through the
# release, the AUR package, or the manual build in the README.

set -euo pipefail
cd "$(dirname "$0")/.."

fail() { echo "release: $*" >&2; exit 1; }

version() { sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1; }

VERSION="$(version)"
TAG="v$VERSION"
DIST="target/dist"
STAGE="$DIST/den-den-mushi-$VERSION"

[ -n "$VERSION" ] || fail "could not read the version from Cargo.toml"

# --- preflight -------------------------------------------------------------

[ -z "$(git status --porcelain)" ] || fail "working tree is not clean"
[ "$(git branch --show-current)" = "main" ] || fail "not on main"
git fetch --quiet origin
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || fail "main is not in sync with origin"
! git ls-remote --exit-code --tags origin "refs/tags/$TAG" >/dev/null 2>&1 \
  || fail "$TAG already exists on origin"
gh auth status >/dev/null 2>&1 || fail "gh is not authenticated"

echo "release: checking (fmt, clippy, build)"
cargo fmt --check
cargo clippy --frozen --all-targets -- -D warnings
cargo build --frozen --release

# --- tag and push -----------------------------------------------------------

echo "release: tagging $TAG (signed)"
git tag -s "$TAG" -m "den-den-mushi $TAG"
git push origin main "$TAG"

# --- stage release assets ----------------------------------------------------

echo "release: staging assets in $DIST"
rm -rf "$STAGE" "$DIST/$TAG-x86_64-linux.tar.gz" "$DIST/PKGBUILD" "$DIST/SHA256SUMS"
mkdir -p "$STAGE/contrib"
cp "target/release/den-den-mushi" "$STAGE/"
cp -r contrib/. "$STAGE/contrib/"
cp README.md LICENSE "$STAGE/"
cp packaging/PKGBUILD "$DIST/PKGBUILD"

tar -C "$DIST" -czf "$DIST/den-den-mushi-$VERSION-x86_64-linux.tar.gz" \
  "den-den-mushi-$VERSION"

(
  cd "$DIST"
  sha256sum "den-den-mushi-$VERSION-x86_64-linux.tar.gz" PKGBUILD > SHA256SUMS
)

# --- publish -----------------------------------------------------------------

echo "release: creating the GitHub release"
gh release create "$TAG" \
  --title "den-den-mushi $TAG" \
  --generate-notes \
  "$DIST/den-den-mushi-$VERSION-x86_64-linux.tar.gz" \
  "$DIST/PKGBUILD" \
  "$DIST/SHA256SUMS"

echo "release: $TAG published"
echo "release: next: scripts/aur-publish.sh"
