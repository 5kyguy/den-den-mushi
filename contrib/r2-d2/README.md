# r2-d2 theme hook

den-den-mushi does not hardcode a palette. Colors are the dark roles from
`config/theme/palette.toml`, filled the same way as walker and GTK.

The daemon also reads the live theme document at

`$XDG_STATE_HOME/r2-d2/theme.json`

which `r2-d2-theme-sync-live` already publishes. A wallpaper change recolors
the cards even before the stylesheet copy below is wired. `SIGUSR1` and
`den-den-mushi reload` read it again, and the daemon watches the file.

## Install the template

From the r2-d2 repo:

```bash
cp /path/to/den-den-mushi/contrib/r2-d2/den-den-mushi.css \
  config/theme/templates/den-den-mushi.css
```

Append `contrib/r2-d2/templates.toml.snippet` to `config/theme/templates.toml`:

```toml
[templates.den-den-mushi]
input_path = "templates/den-den-mushi.css"
output_path = "../den-den-mushi/style.css"
```

`r2-d2-theme-apply` then renders `config/den-den-mushi/style.css` from the
dark roles. Paths are relative to `config/theme/`, same as the walker and
gtk entries.

`r2-d2-theme-sync-live` only copies the files in its own list. Add this next
to the walker copy:

```bash
mkdir -p ~/.config/den-den-mushi
cp -T "$R2D2_PATH/config/den-den-mushi/style.css" \
  ~/.config/den-den-mushi/style.css
```

Optional, in `bin/r2-d2-theme-reload`, after the walker restart:

```bash
den-den-mushi reload >/dev/null 2>&1 || true
```

The file watch on `theme.json` and on `~/.config/den-den-mushi/style.css`
already applies the new accent. The reload line is the same hook walker uses.

## What the stylesheet uses

| Role | Where |
| --- | --- |
| `bg` | Card body and the notification panel |
| `surface` | Hover, quiet action pills, the panel search field, and the timeout menu |
| `line` | Hairline border on cards and the panel |
| `accent` | App name, the left edge, and the do-not-disturb line |
| `on_accent` | Label of an action pill while it is hovered, and a switch handle while it is on |
| `text` / `muted` | Title, body, Clear all, the search placeholder, and the empty state |
| `danger` / `on_danger` | Critical urgency |
| `warning` / `on_warning` | Low-battery notifications |

Standalone, with no `theme.json` and no rendered stylesheet, the daemon uses
the dark half of the White accent (the same fallback r2-d2 uses when a
wallpaper cannot be read).
