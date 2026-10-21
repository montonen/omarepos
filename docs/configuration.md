# Configuration and theme

## Configuration

Optional, at `~/.config/omarepos/config.toml`. Without one there is a single
root, `~/Work`, searched three levels down.

Everything here is also editable from the settings dialog, which rewrites this
file (and drops your comments when it does).

Edit it while the window is open and the edit is taken up at the next rescan
(`Ctrl+R`, or any scan the app starts itself); a toast says so. Until then the
app will not save over a file that is no longer what it read: change a setting
in the window first and it tells you the file has changed, and to rescan. A
file caught half-edited, one that does not parse, changes nothing: what was in
force stays in force, a banner says where the parser stopped, and it is read
again once mended.

The launcher key brings an open window forward and does not start a second
one. After installing a new build that window is still the old program, and
it says so when it is brought forward: quit and start again for the new one.

```toml
# Directories to scan. `~` is expanded. A root is either a bare path, or a
# table when it needs options.
roots = [
  "~/Work",
  { path = "~/clients/acme", nested = true, tag = "acme", include_self = false },
]

# Individual repositories, included wherever they live.
extra_repos = ["~/.config/omarchy"]

# How many levels below each root to look for a `.git`. Descent stops at the
# first repository found, so nested checkouts stay out of the grid.
max_depth = 3

# Defaults to $TERMINAL, then the first of alacritty, ghostty, kitty, foot,
# wezterm, gnome-terminal, ptyxis and konsole that is installed. When none
# can be started the window says so.
terminal = "alacritty"

# Run in the terminal by "Open in terminal", before the shell takes over.
# Unset follows lazygit when it is installed; "" gives a plain shell.
terminal_runs = "lazygit"

# Defaults to $EDITOR, then nvim. Run as `<editor> .` in the repository, so
# one that opens a directory is what works best; nano, pico, micro and ed are
# started bare.
editor = "nvim"

# Follow the active Omarchy theme and restyle live when it changes.
follow_omarchy_theme = true
```

## Theme

The window's colours come from the active Omarchy theme: `colors.toml` under
`~/.local/state/omarchy/current/theme`, read by the rules of Omarchy's own
resolver and compiled into the window's stylesheet. `omarchy theme set` puts a
new theme in that place; the window watches for it and repaints, the drawn
parts of a card included. Nothing is cached and nothing needs restarting.
`follow_omarchy_theme = false` keeps the palette the window started with.

On a desktop that is not Omarchy there is no theme there to read, and the
window follows the desktop's own light or dark setting, as any libadwaita app
does, with a built-in palette for each; it changes with the desktop's
setting. The built-in palettes are also what a theme that leaves a colour out
gets that colour from.

Three things keep colours of their own. The lanes of a commit graph, and the
dots on tags and on authors, use a fixed set of eight (a darker eight on a
light theme), because they have to stay apart from each other in every theme
and a theme's own handful of hues cannot promise that. A tag's colour is
picked from the eight by its name, so two tags can share one. And the syntax
colours in a diff are the highlighter's.

The roles are used the way Omarchy's own templates use them: `background` for
the page, `lighter_background` for cards, `foreground` for text, `muted` for
borders, `accent`, and `red` and `yellow` for what calls for attention.
The button a page puts forward to be pressed, **Choose folders…** on a
window with nothing to list, is filled with the accent. Small
print is the foreground at seven tenths, which is how those templates write
muted text. It is not `light_foreground`, which in some themes is the brighter
foreground and not the dimmer one.

Coloured text is the theme's own hue where that can be read on every
background a card takes, and otherwise the hue moved toward the foreground
until it can. On a theme whose yellow or accent sits close to its background
the coloured words end up nearly the colour of plain text. The left edge of a
card is the theme's colour as it stands, and carries the state there.

---

[Back to the README](../README.md) · [All documentation](README.md)
