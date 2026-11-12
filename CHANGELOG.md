# Changelog

All notable changes to **den-den-mushi** are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

GitHub Releases for each version are published from the matching section below;
see the [Releases page](https://github.com/5kyguy/den-den-mushi/releases).

## [0.1.1] - 2026-11-12

### Added

- Curl-driven install script for GitHub Releases (`scripts/install.sh`), with
  uninstall, version pinning, and a user-unit restart.
- Release automation: pushing a `v*` tag now builds and publishes the GitHub
  release in CI, with notes taken from the matching changelog section.

## [0.1.0] - 2026-10-30

Initial release.

### Added

- Freedesktop notification daemon for Hyprland: cards on a gtk4-layer-shell
  overlay in the top-right, in JetBrainsMono Nerd Font, colored by the r2-d2
  palette.
- Card motion: enter, exit, replace, and critically damped restack animations.
- Do-not-disturb, history, and dismiss-all controls.
- AUR packaging (`den-den-mushi`), MIT license, and release scripts.

[0.1.1]: https://github.com/5kyguy/den-den-mushi/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/5kyguy/den-den-mushi/releases/tag/v0.1.0
