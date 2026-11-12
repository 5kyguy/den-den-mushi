#!/usr/bin/env bash
# Cut a den-den-mushi release.
#
#   scripts/release.sh
#
# Checks the tree, tags v<version> from Cargo.toml>, pushes the commit and
# the tag. CI (.github/workflows/release.yml) then builds and publishes the
# GitHub release from the matching CHANGELOG.md section with:
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

[ -n "$VERSION" ] || fail "could not read the version from Cargo.toml"

# --- preflight -------------------------------------------------------------

[ -z "$(git status --porcelain)" ] || fail "working tree is not clean"
[ "$(git branch --show-current)" = "main" ] || fail "not on main"
git fetch --quiet origin
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || fail "main is not in sync with origin"
! git ls-remote --exit-code --tags origin "refs/tags/$TAG" >/dev/null 2>&1 \
  || fail "$TAG already exists on origin"
grep -q "^## \[$VERSION\]" CHANGELOG.md \
  || fail "CHANGELOG.md has no section for $VERSION (expected \"## [$VERSION]\")"

echo "release: checking (fmt, clippy, build)"
cargo fmt --check
cargo clippy --frozen --all-targets -- -D warnings
cargo build --frozen --release

# --- tag and push -----------------------------------------------------------

echo "release: tagging $TAG (signed)"
git tag -s "$TAG" -m "den-den-mushi $TAG"
git push origin main "$TAG"

echo "release: $TAG pushed; CI publishes the GitHub release"
echo "release: https://github.com/5kyguy/den-den-mushi/actions"
echo "release: next: scripts/aur-publish.sh (after CI finishes)"
