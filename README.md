# omarepos

An Omarchy-styled overview of every git repo on your machine.

A GTK4 / libadwaita desktop app that scans your project directories and shows one
card per repository: current branch, working-tree state, how far ahead or behind
its upstream it is, stashes, language mix, and the last commit. It reads the
active Omarchy theme's `colors.toml`, so it repaints itself when you switch themes.

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
install -Dm644 data/se.alphadev.Omarepos.desktop ~/.local/share/applications/se.alphadev.Omarepos.desktop
install -Dm644 data/se.alphadev.Omarepos.svg ~/.local/share/icons/hicolor/scalable/apps/se.alphadev.Omarepos.svg
```

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
| `Esc` | Clear the filter |
| `Ctrl+Q` | Quit |
| Double-click a card | Open a terminal in that repo |

The filter matches repo name, path, branch, remote and language. The warning
toggle in the header narrows the grid to repos that are dirty, ahead, behind, or
unreadable.

## Configuration

Optional, at `~/.config/omarepos/config.toml`:

```toml
# Directories to scan. `~` is expanded.
roots = ["~/code", "~/work"]

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
| `src/scan.rs` | Worker threads streaming results to the main loop |
| `src/lang.rs` | Extension → language + colour table |
| `src/ui/window.rs` | Header, filter, sort, grid, status bar, theme watcher |
| `src/ui/card.rs` | One repository card |

## Status

Early. The fleet overview works; a per-repo commit graph is the obvious next step.
