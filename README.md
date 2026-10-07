# omarepos

An Omarchy-styled overview of every git repo on your machine.

A GTK4 / libadwaita desktop app with three views:

- **Fleet** — one card per repository: current branch, working-tree state, how far
  ahead or behind its upstream it is, stashes, language mix, and the last commit.
- **Commit graph** — the DAG of any one repository, with branch and tag labels.
- **Contributions** — who committed what over the last week, across all of your
  repositories or just the ones you tick.

Repositories can be tagged, and the grid filtered by tag.

It reads the active Omarchy theme's `colors.toml`, so it repaints itself when you
switch themes.

```
┌──────────────────────────────────────┐
│ omarchy-plugins                   ⋮  │
│ ~/code/omarchy-plugins               │
│  main   ~3 ?2   ↑2                   │
│ github.com/you/omarchy-plugins       │
│ ███████████▌▌▌▌▎▎                    │
│ ● QML 54%  ● Shell 28%  ● Rust 11%   │
│ 3 hours ago · a1b2c3d · you          │
│ fix bar widget repaint on theme swap │
└──────────────────────────────────────┘
```

## Build and run

```bash
cargo run --release
```

Runtime dependencies: GTK 4.12+, libadwaita 1.4+. Build dependencies: a C
toolchain and `pkg-config` (libgit2 is vendored by `git2`).

On Arch / Omarchy:

```bash
sudo pacman -S --needed gtk4 libadwaita rust base-devel
```

## Install

```bash
cargo install --path .
ln -sf ~/.cargo/bin/omarepos ~/.local/bin/omarepos
install -Dm644 data/se.alphadev.Omarepos.desktop ~/.local/share/applications/se.alphadev.Omarepos.desktop
install -Dm644 data/se.alphadev.Omarepos.svg ~/.local/share/icons/hicolor/scalable/apps/se.alphadev.Omarepos.svg
```

The symlink matters on Omarchy: the Hyprland/uwsm session `PATH` carries
`~/.local/bin` but not `~/.cargo/bin`, so without it the desktop entry and any
keybinding both fail to find the binary even though your shell can run it.

### Hyprland keybinding

In `~/.config/hypr/bindings.lua`:

```lua
o.bind("SUPER + R", "Git repos (omarepos)", { launch = "omarepos" })
```

`launch` wraps the command in `uwsm-app`, which puts it in its own systemd
scope. Check the key is free first with `omarchy menu keybindings --print`, and
validate with `hyprctl reload && hyprctl configerrors`.

## Usage

```
omarepos [options]

  -r, --root <PATH>   Directory to scan (repeatable; overrides the config file)
  -d, --depth <N>     How deep below each root to look for repos
  -h, --help          Show help
  -V, --version       Show the version
```

| Key | Action |
| --- | --- |
| `Ctrl+R` / `F5` | Rescan |
| `Ctrl+F` / `/` | Focus the filter box |
| `Enter` in the filter | Open the commit graph of the top hit |
| `Esc` | Clear the filter |
| `Ctrl+G` | Contributions for the selected repos, or all of them |
| `Ctrl+Shift+A` | Clear the selection |
| `Ctrl+,` | Settings |
| `Alt+←` | Back to the grid |
| `Ctrl+Q` | Quit |
| Click a card, or `Enter` on a focused one | Open its commit graph |
| `Space` on a focused card | Tick it for the contributions scope |

The filter matches repo name, path, branch, remote and language. The warning
toggle in the header narrows the grid to repos that are dirty, ahead, behind, or
unreadable. Each card's checkbox adds it to the contributions scope — once
anything is ticked the rest of the grid dims, so the selection stays visible
while you scroll. The `⋮` menu holds the terminal, editor and clipboard actions.

## Adding repositories

Three ways, in order of convenience:

1. **Settings** (`Ctrl+,` or the gear in the header) — add a scan root with a
   folder picker, add a single repository that lives anywhere, or change how deep
   the scan goes. Edits are written to the config file immediately and the grid
   rescans.
2. **The config file**, `~/.config/omarepos/config.toml`: `roots` for folders to
   search, `extra_repos` for individual repositories outside them.
3. **`--root <PATH>`** on the command line, repeatable, for a one-off look at
   somewhere else. It overrides the configured roots for that run and is not saved.

A root is searched `max_depth` levels down and stops descending at the first
repository it finds, so `~/code` picks up `~/code/foo` and `~/code/plugins/bar`
without ever walking into their contents. `extra_repos` entries are included
whatever their depth, which is the escape hatch for a repo in an awkward place.

## Tags

Tags are omarepos's own metadata — nothing is written into your repositories. They
live in `~/.config/omarepos/repos.toml`, keyed by absolute path.

- Tag one repo from its `⋮` menu, or several at once: tick their checkboxes, then
  **Tags → Tag N selected…**. With nothing ticked, that button tags everything
  currently shown, so the filter box doubles as a bulk selector.
- Editing several at once only replaces the tags they have *in common*; tags
  unique to one of them survive.
- The **Tags** popover filters the grid. Several ticked tags is an OR. `tag:work`
  typed in the filter box is an AND, and stacks with the rest of the query —
  `tag:work ruby` means tagged work *and* matching "ruby".
- Tag colours are derived from the tag's own name, so a tag keeps its colour
  across repos and across runs.

## Commit graph

Click any card for its history. Lanes are assigned the usual way: a commit takes
the lane that was waiting for it, hands that lane to its first parent, and parks
any further parents in lanes of their own, so merges fan out and converge where
they actually did.

- Filled dot: ordinary commit. Ring: merge commit. Outer ring: something points
  at it.
- Pills are refs — the current branch, other local branches, tags, remote branches.
- Clicking a row copies its full sha.
- The newest 500 commits are shown; the status bar says so when there are more.
  Lanes past the sixteenth are clipped rather than squeezed.

Every branch is walked, not just the one checked out, so a commit that only exists
on a side branch still appears.

## Contributions

`Ctrl+G`, the header button, or the `⋮` menu of a single repo. Tick cards first to
scope it; with nothing ticked it covers whatever the grid is currently showing, so
the filter box doubles as the repo picker.

Default window is the last 7 days, with 14 and 30 in the header dropdown. Days are
local calendar days, bucketed at local midnight.

- Authors are keyed by e-mail, so one person with two spellings of their name
  still lands in one row.
- Merge commits count as commits but not as lines — their diffs are their parents'
  work, already counted. The per-author tooltip breaks out how many were merges.
- Per-day strips are scaled against the busiest author-day, so rows are comparable.

Line counts come from diffing each commit against its first parent, which is the
expensive part; a week across a dozen repos takes well under a second, but a
30-day window over a large fleet will visibly chew.

## Configuration

Optional, at `~/.config/omarepos/config.toml`:

Everything here is also editable from the settings dialog, which rewrites this
file (and drops your comments when it does).

```toml
# Directories to scan. `~` is expanded.
roots = ["~/code", "~/work"]

# Individual repositories, included wherever they live.
extra_repos = ["~/.config/omarchy"]

# How many levels below each root to look for a `.git`. Descent stops at the
# first repository found, so nested checkouts stay out of the grid.
max_depth = 3

# Defaults to $TERMINAL, then the first of alacritty/ghostty/kitty/foot/wezterm.
terminal = "alacritty"

# Defaults to $EDITOR, then nvim.
editor = "nvim"

# Follow the active Omarchy theme and restyle live when it changes.
follow_omarchy_theme = true
```

## How it reads your repos

Everything comes from libgit2 against the on-disk repository — omarepos never
runs `git`, never writes to a repository, and never talks to a remote. Ahead/
behind counts compare your local branch to the *last fetched* state of its
upstream, exactly like `git status` does; they go stale until you fetch.

Discovery skips `node_modules`, `vendor`, `target` and dotted directories, and
the language mix is computed from the git index, so untracked build output never
counts.

## Layout

| Path | What's in it |
| --- | --- |
| `src/main.rs` | CLI parsing, app bootstrap, the shared `CssProvider` |
| `src/config.rs` | `config.toml`, path expansion, terminal/editor resolution |
| `src/theme.rs` | Reads Omarchy's `colors.toml`, compiles the GTK stylesheet |
| `src/git.rs` | Repository discovery and inspection |
| `src/graph.rs` | Revwalk → lane assignment for the commit graph |
| `src/meta.rs` | Tag store (`repos.toml`) |
| `src/stats.rs` | Contribution aggregation over a day window |
| `src/scan.rs` | Worker threads streaming results to the main loop |
| `src/lang.rs` | Extension → language + colour table |
| `src/ui/window.rs` | Header, filter, sort, grid, selection, status bar, theme watcher |
| `src/ui/card.rs` | One repository card |
| `src/ui/graph.rs` | Commit graph page and its cairo row renderer |
| `src/ui/stats.rs` | Contributions page |
| `src/ui/dialogs.rs` | Settings dialog and the tag editor |

## Status

Early, but the three views are complete and usable. Known rough edges: ahead/behind
and the graph both reflect the last fetch, nothing refreshes itself while the window
is open except on `Ctrl+R`, and the graph has no diff view yet.
