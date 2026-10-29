# den-den-mushi

A freedesktop notification daemon for Hyprland. Cards sit in the top-right
on a layer-shell overlay, in JetBrainsMono Nerd Font, colored by the r2-d2
palette.

The look follows the Hokum notification cards (stacked, flat, app line,
title, body, quiet action pills). This is an independent GTK program. It
does not use Hokum's code or Anomaly's Rune.

## Stack

GTK 4, [gtk4-layer-shell](https://github.com/wmww/gtk4-layer-shell), and
zbus.

Rune can host a Wayland layer-shell overlay, and Hokum is built on it.
That overlay is placed on the top or bottom edge of the primary display.
The chapter shipped with Rune still says interactive Wayland overlays are
unavailable; a later note wakes idle clicks, and that still is not a
top-right stack under a bar, inset by Hyprland's gaps, with a freedesktop
notification server. Rune would mean compiling that whole engine and then
building the shell behavior on top of it.

GTK 4 is the same CSS world as the walker and GTK templates in r2-d2. The
stylesheet in `contrib/r2-d2/` is filled by `r2-d2-theme-apply` with
`{{colors.<role>.default.hex}}`, then loaded through a `CssProvider`. zbus
owns `org.freedesktop.Notifications`. The layer namespace is
`r2-d2-notifications`, which is the namespace the Hyprland blur rule in
r2-d2 already matches.

## Build

Arch, which is what r2-d2 runs:

```bash
sudo pacman -S gtk4 gtk4-layer-shell ttf-jetbrains-mono-nerd
cargo build --release
install -Dm755 target/release/den-den-mushi ~/.local/bin/den-den-mushi
```

Rust 1.85 or newer. The font family the stylesheet asks for is
`JetBrainsMono Nerd Font`. Without that font the text still renders, and the
app glyphs do not.

Ubuntu 24.04 does not package gtk4-layer-shell. Build
[v1.1.1](https://github.com/wmww/gtk4-layer-shell/releases/tag/v1.1.1) or
newer (`-Dvapi=false` if `vapigen` is absent) and install it somewhere
`pkg-config` can see `gtk4-layer-shell-0`.

## Run

```bash
den-den-mushi
```

The process takes `org.freedesktop.Notifications` on the session bus. The
r2-d2 shell's Quickshell notification server claims that same name. Run one
of them. `den-den-mushi --replace` asks to replace the current owner.

```bash
notify-send -a Spotify "Now playing" "Jet Plane — local file"
notify-send -u critical -a System "Disk failing" "Back up the home volume"
notify-send -a Venmo -A pay=Pay -A decline=Decline "Gary requested \$40" "for the thing"
```

Left click runs the `default` action. Right click, or the ×, dismisses.
Action buttons invoke their own keys. A thin accent line along the bottom
tracks the timeout and pauses while the pointer is on the card. Critical
notifications with no client timeout stay until dismissed. `replaces_id`
updates the open card.

Low-battery notifications (category `device.battery`, or a "low battery"
summary) use the warning color. Critical uses danger.

### Do not disturb

```bash
den-den-mushi dnd          # toggle
den-den-mushi dnd on
den-den-mushi dnd off
den-den-mushi dnd status
```

Low and normal notifications are dropped. Critical notifications still show,
and so do toasts whose app name is `r2-d2-action`, `k2so`, or
`den-den-mushi`, matching the desktop's own action confirmations. The choice
is stored in `$XDG_STATE_HOME/den-den-mushi/dnd`.

### Hyprland

In `hyprland.conf`:

```
exec-once = den-den-mushi
```

In r2-d2's `config/hypr/autostart.lua`, next to the other session programs:

```lua
hl.exec_cmd("uwsm-app -- den-den-mushi")
```

Cards stack in the top-right with 8px between them. The screen inset is half
of `general:gaps_out` (the same inset the r2-d2 shell uses) plus the
monitor's reserved top, so a top bar is cleared. Override the inset in
`~/.config/den-den-mushi/config.toml`:

```toml
margin = 8
gap = 8
namespace = "r2-d2-notifications"
timeout_low_ms = 4000
timeout_normal_ms = 5000
```

### systemd --user

```bash
mkdir -p ~/.config/systemd/user ~/.local/bin
install -Dm755 target/release/den-den-mushi ~/.local/bin/den-den-mushi
cp contrib/systemd/den-den-mushi.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now den-den-mushi.service
```

`contrib/dbus/org.freedesktop.Notifications.service` is an optional session
service file for D-Bus activation. Do not install it while another
notification server is still starting.

## Theme

See [contrib/r2-d2/README.md](contrib/r2-d2/README.md). Short version: copy
the template into r2-d2's `config/theme/templates/`, register it in
`templates.toml`, and let `r2-d2-theme-apply` fill the dark roles. The
running daemon follows `$XDG_STATE_HOME/r2-d2/theme.json`, the rendered
stylesheet, and `den-den-mushi reload` / `SIGUSR1`.

## What was verified

On a headless sway (`WLR_BACKENDS=headless`, pixman), `notify-send` talks to
this daemon: capabilities, server info, a normal card, a critical card, a
stack of three (glyph, image, low battery), action buttons, `replaces_id`,
do-not-disturb, and a live recolor from `theme.json`. Screenshots are the
grim captures of that session. Pointer clicks were not driven; there is no
pointer device on that headless seat. Left click, right click, and the
action buttons are GTK gestures and buttons on the card.
