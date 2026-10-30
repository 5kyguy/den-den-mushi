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

Left click runs the `default` action. Right click dismisses. The × stays
hidden until the pointer is on the card, then it dismisses too. Action
buttons invoke their own keys. A thin accent line along the bottom tracks
the timeout and pauses while the pointer is on the card. Critical
notifications never auto-expire, even if the sender asked for a timeout.
`replaces_id` updates the open card.

Low-battery notifications (category `device.battery`, or a "low battery"
summary) use the warning color. Critical uses danger.

### Do not disturb

```bash
den-den-mushi dnd          # toggle
den-den-mushi dnd on
den-den-mushi dnd off
den-den-mushi dnd status
den-den-mushi dnd status --json
den-den-mushi dnd --json --watch
```

`--json` prints one object per line, `{"dnd":true}` or `{"dnd":false}`.
`--watch` prints the current state, then another line each time it changes,
and exits on SIGINT or when the pipe closes. A bar follows it with
`den-den-mushi dnd --json --watch`. `--watch` on its own does not toggle.
`dnd on --json --watch` turns it on, prints that, then keeps watching.

Low and normal notifications are dropped. Critical notifications still show,
and so do toasts whose app name is `r2-d2-action`, `k2so`, or
`den-den-mushi`, matching the desktop's own action confirmations. The choice
is stored in `$XDG_STATE_HOME/den-den-mushi/dnd`.

### Dismiss and invoke

```bash
den-den-mushi dismiss       # close the newest card
den-den-mushi dismiss --all
den-den-mushi invoke        # default action on the newest card, then close it
```

`dismiss` emits `NotificationClosed` with reason 2 (dismissed by the user).
`invoke` emits `ActionInvoked` with key `default` only when the newest card
offered that action, then closes it with reason 2. A card with no default
action is only dismissed. A left click sends the action in the same case.
Right click on a card dismisses that card, also with reason 2. An empty
stack is a no-op.

### Control API

The commands above are clients of one interface, `com.skyguy.DenDenMushi`,
at `/com/skyguy/DenDenMushi` on the session bus. The daemon that owns
`org.freedesktop.Notifications` serves it. A control dashboard should call
this interface, not a second protocol.

| Member | Role |
| --- | --- |
| `Dismiss(all: bool)` | Newest card, or every card |
| `Invoke()` | Default action when the newest card has one, then close it |
| `SetDoNotDisturb(on: bool)` | Writes do not disturb |
| `DoNotDisturb` | Read-only property. `PropertiesChanged` when it changes |
| `ReloadTheme()`, `Visible()`, `GetDoNotDisturb()` | Theme reload, on-screen ids, and the same flag as the property |

```bash
busctl --user call org.freedesktop.Notifications /com/skyguy/DenDenMushi \
  com.skyguy.DenDenMushi Dismiss b false
busctl --user get-property org.freedesktop.Notifications /com/skyguy/DenDenMushi \
  com.skyguy.DenDenMushi DoNotDisturb
```

### Hyprland

In `hyprland.conf`:

```
exec-once = den-den-mushi
bind = SUPER, N, exec, den-den-mushi dismiss
bind = SUPER SHIFT, N, exec, den-den-mushi dismiss --all
bind = SUPER, A, exec, den-den-mushi invoke
```

Those binds are examples for this daemon. r2-d2's own Hyprland config, and
the quickshell bar that would run `den-den-mushi dnd --json --watch`, are
not changed here.

In r2-d2's `config/hypr/autostart.lua`, next to the other session programs:

```lua
hl.exec_cmd("uwsm-app -- den-den-mushi")
```

Cards anchor to the top-right. The inset from the screen edge is the full
`general:gaps_out` (the outer gap tiled windows already clear) plus the
focused monitor's reserved top from `hyprctl monitors -j`, which is the
bar's exclusive zone. Card padding, the gap between title and body, the
space above the action row, and the 8px between cards are in
`contrib/r2-d2/den-den-mushi.css`, not in the binary.

Override the screen inset in `~/.config/den-den-mushi/config.toml`:

```toml
margin = 8
namespace = "r2-d2-notifications"
timeout_low_ms = 4000
timeout_normal_ms = 5000
```

r2-d2 already blurs this layer. `config/hypr/looknfeel.lua` uses the current
`hl.layer_rule` form (the same one the Hyprland wiki documents for layer
rules), and the namespace matches:

```lua
hl.layer_rule({
  match = { namespace = "^(walker|r2-d2-notifications|r2-d2-osd)$" },
  blur = true,
  ignore_alpha = 0.1,
})
```

`blur` is a boolean and `ignore_alpha` is a float from 0 to 1. The old
`layerrule = blur, <namespace>` keyword is not the current syntax. On a
hyprlang config that does not use the lua helper, the equivalent block is:

```
layerrule {
    match:namespace = ^(r2-d2-notifications)$
    blur = on
    ignore_alpha = 0.1
}
```

This was checked against the Hyprland wiki's layer-rule page (the
`hl.layer_rule` examples) and against r2-d2's `hypr-looknfeel.lua`. It was
not applied on a running Hyprland: this environment has no Hyprland
compositor, so `hyprctl` gaps, reserved zones, and blur were not observed
live. Headless sway has neither Hyprland gaps nor layer rules.

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
