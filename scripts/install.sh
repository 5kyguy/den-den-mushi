#!/usr/bin/env bash
# Install den-den-mushi from GitHub Releases (Linux, x86_64).
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/5kyguy/den-den-mushi/main/scripts/install.sh | bash
#   DDM_VERSION=v0.1.0 bash scripts/install.sh
#   curl -fsSL ... | bash -s -- --uninstall
#
# Environment:
#   DDM_VERSION    Release tag (e.g. v0.1.0) or "latest" (default)
#   DDM_BIN_DIR    Binary directory (default: ~/.local/bin)
#   DDM_UNIT_DIR   systemd user unit directory (default: ~/.config/systemd/user)
#
# The script downloads the release tarball, verifies it against the release
# SHA256SUMS, installs the binary and the user unit, and restarts the daemon
# if it was running. It never installs the D-Bus activation service — see the
# README. Uninstall keeps the history, rules, and config.

set -euo pipefail

REPO="5kyguy/den-den-mushi"
BIN_DIR="${DDM_BIN_DIR:-${HOME}/.local/bin}"
UNIT_DIR="${DDM_UNIT_DIR:-${HOME}/.config/systemd/user}"
UNIT="den-den-mushi.service"
work=""
trap 'if [ -n "$work" ]; then rm -rf "$work"; fi' EXIT

err() {
  echo "den-den-mushi install: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || err "missing required command: $1"
}

api_get() {
  curl -fsSL \
    -H "Accept: application/vnd.github+json" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    "https://api.github.com/$1"
}

resolve_release_tag() {
  local version="${DDM_VERSION:-latest}"
  if [ "$version" = "latest" ]; then
    api_get "repos/${REPO}/releases/latest" | jq -r .tag_name
  else
    case "$version" in
      v*) echo "$version" ;;
      *) echo "v${version}" ;;
    esac
  fi
}

find_asset_url() {
  local tag="$1"
  api_get "repos/${REPO}/releases/tags/${tag}" \
    | jq -r '
        .assets[]
        | select(.name | test("^den-den-mushi-.*-x86_64-linux\\.tar\\.gz$"))
        | .browser_download_url
      ' \
    | head -n 1
}

verify_checksum() {
  local tarball_path="$1"
  local filename="$2"
  local tag="$3"
  local sums_url="https://github.com/${REPO}/releases/download/${tag}/SHA256SUMS"
  local sums_path="$4"
  local expected actual

  curl -fsSL "$sums_url" -o "$sums_path" \
    || err "SHA256SUMS not found for ${tag}"

  expected="$(awk -v file="$filename" '$2 == file { print $1; exit }' "$sums_path")"
  [ -n "$expected" ] || err "no checksum entry for ${filename} in SHA256SUMS"

  actual="$(sha256sum "$tarball_path" | awk '{ print $1 }')"
  [ "$actual" = "$expected" ] || err "checksum verification failed"
}

unit_ctl() {
  # systemctl steps warn instead of failing, so the script also works while
  # preparing a home directory without a running session manager.
  systemctl --user "$@" 2>/dev/null || echo "den-den-mushi install: systemctl --user $1 failed (no session manager?)" >&2
}

uninstall() {
  echo "Removing den-den-mushi..."
  if systemctl --user is-active --quiet "$UNIT" 2>/dev/null; then
    unit_ctl stop "$UNIT"
  fi
  if systemctl --user is-enabled --quiet "$UNIT" 2>/dev/null; then
    unit_ctl disable "$UNIT"
  fi
  rm -f "${BIN_DIR}/den-den-mushi" "${UNIT_DIR}/${UNIT}"
  unit_ctl daemon-reload
  echo "Removed ${BIN_DIR}/den-den-mushi and ${UNIT_DIR}/${UNIT}."
  echo "Kept config, rules, history, and do-not-disturb state:"
  echo "  ${XDG_CONFIG_HOME:-${HOME}/.config}/den-den-mushi/"
  echo "  ${XDG_STATE_HOME:-${HOME}/.local/state}/den-den-mushi/"
}

install_release() {
  local tag asset_url filename tarball_dir
  work="$(mktemp -d)"

  tag="$(resolve_release_tag)"
  [ -n "$tag" ] || err "could not resolve the latest release tag"
  asset_url="$(find_asset_url "$tag")"
  [ -n "$asset_url" ] || err "no Linux tarball found for ${tag}; check https://github.com/${REPO}/releases"
  filename="${asset_url##*/}"
  tarball_dir="${filename%-x86_64-linux.tar.gz}"

  echo "Downloading den-den-mushi ${tag}..."
  curl -fsSL "$asset_url" -o "${work}/${filename}"

  echo "Verifying checksum..."
  verify_checksum "${work}/${filename}" "$filename" "$tag" "${work}/SHA256SUMS"

  tar -C "$work" -xzf "${work}/${filename}"
  [ -d "${work}/${tarball_dir}" ] || err "unexpected tarball layout: ${tarball_dir} not found"

  local was_active=0
  if systemctl --user is-active --quiet "$UNIT" 2>/dev/null; then
    was_active=1
  fi

  mkdir -p "$BIN_DIR" "$UNIT_DIR"
  install -m 755 "${work}/${tarball_dir}/den-den-mushi" "${BIN_DIR}/den-den-mushi"
  sed "s|%h/.local/bin/den-den-mushi|${BIN_DIR}/den-den-mushi|" \
    "${work}/${tarball_dir}/contrib/systemd/${UNIT}" > "${UNIT_DIR}/${UNIT}"
  unit_ctl daemon-reload

  if [ "$was_active" = "1" ]; then
    unit_ctl restart "$UNIT"
    echo "Restarted ${UNIT}."
  fi

  if ! echo ":${PATH}:" | grep -q ":${BIN_DIR}:"; then
    echo
    echo "Add ${BIN_DIR} to your PATH, for example:"
    echo "  export PATH=\"${BIN_DIR}:\$PATH\""
  fi

  echo
  echo "den-den-mushi ${tag} installed."
  if [ "$was_active" = "0" ]; then
    echo "Start with: systemctl --user enable --now ${UNIT}"
  fi
  echo "D-Bus activation is not installed; the README explains when to add it."
  echo "Uninstall with: curl -fsSL https://raw.githubusercontent.com/${REPO}/main/scripts/install.sh | bash -s -- --uninstall"
}

main() {
  [ "$(uname -s)" = "Linux" ] || err "den-den-mushi installs on Linux only."
  case "$(uname -m)" in
    x86_64 | amd64) ;;
    *) err "unsupported architecture: $(uname -m) (x86_64 only)" ;;
  esac

  if [ "${1:-}" = "--uninstall" ]; then
    uninstall
    return
  fi
  [ "$#" -eq 0 ] || err "unknown argument: $1 (only --uninstall is supported)"

  need_cmd curl
  need_cmd jq
  need_cmd tar
  need_cmd sha256sum
  install_release
}

main "$@"
