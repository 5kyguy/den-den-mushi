#!/usr/bin/env bash
# Publish packaging/PKGBUILD and .SRCINFO to the AUR as den-den-mushi.
#
#   scripts/aur-publish.sh
#
# Requires an AUR account whose SSH key is registered:
#   https://aur.archlinux.org/register/  then  My Account -> SSH PubKeys

set -euo pipefail
cd "$(dirname "$0")/.."

fail() { echo "aur-publish: $*" >&2; exit 1; }

PKGNAME=den-den-mushi
VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
[ -n "$VERSION" ] || fail "could not read the version from Cargo.toml"
[ -f packaging/.SRCINFO ] || fail "packaging/.SRCINFO is missing (run: makepkg --printsrcinfo > packaging/.SRCINFO)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "aur-publish: cloning aur.archlinux.org/$PKGNAME"
git clone --quiet "ssh://aur@aur.archlinux.org/${PKGNAME}.git" "$WORK/$PKGNAME"

cp packaging/PKGBUILD packaging/.SRCINFO "$WORK/$PKGNAME/"
git -C "$WORK/$PKGNAME" add PKGBUILD .SRCINFO
git -C "$WORK/$PKGNAME" commit --quiet -m "v$VERSION-1"

echo "aur-publish: pushing"
git -C "$WORK/$PKGNAME" push --quiet

echo "aur-publish: $PKGNAME $VERSION on the AUR"
echo "aur-publish: https://aur.archlinux.org/packages/$PKGNAME"
