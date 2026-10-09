# Development

## Build and run

```bash
cargo run --release
```

Runtime dependencies: GTK 4.12+, libadwaita 1.5+, and `git` itself, which
makes every write. Build dependencies: Rust 1.92 or newer, a C toolchain,
`pkg-config`, and the GTK 4 and libadwaita development files. libgit2
is linked from the system when it is installed, and built from the copy `git2`
carries when it is not.

On Arch / Omarchy:

```bash
sudo pacman -S --needed gtk4 libadwaita libgit2 git rust base-devel
```

`cargo test` builds its own repositories under `target/test-fixtures`, each
with a bare remote beside it and a home directory of its own, and touches
nothing else on the machine.

A panic inside a GTK callback ends the program, and most of what a window
does cannot be reached by a unit test. So there is a second kind of test, a
walk:

```bash
scripts/walk/walk.sh
```

It builds fixture repositories under `target/walk` (one of each kind: ahead,
behind, no commits yet, merges and long branch names, a detached HEAD, a
conflict, an index nothing can read, odd file names, a binary, a NUL late in
a text file, a remote with a page on the web at a host that does not exist),
starts a debug build on a hidden Hyprland workspace with a
scratch configuration, and then uses every page and every action once: the
grid and its filter, every commit and file of every repository, staging, a
commit and a push, the commit search, contributions, tags and the tag editor,
light, broken and missing themes, Settings row by row, and the config and tag
files broken and mended under the running window. After each of some five
hundred steps it runs the drawing code and checks that the program is still
there; it exits non-zero, with the panic, if it is not. A window driven this
way opens no browser: it writes down the page it would have opened, and the
walk checks that each way of asking wrote the right one. `walk.sh quick` does
one repository instead of eight. It needs Hyprland, `python3`, `gdbus` and
`busctl`, takes about ten minutes, puts nothing on screen and touches nothing
of yours: everything goes over D-Bus to actions that only a debug build has,
and only when it is started with `OMAREPOS_DUMP=<file>` (`src/ui/debug.rs`).
`scripts/walk/drive.py` is the same thing by hand: `dump` what is on screen
as text, `press` a button by what it says, hand the app a `key`, set a
settings `row`, `pick` a list row, `paint`, and `shot` for a picture of the
window as a PNG, which is the one way to see what a change looks like without
putting the window on screen. A release build has none of it.

One thing a walk cannot see is what waits for a frame, because a window on a
hidden workspace is sent none: a change of width across a breakpoint starts
on one frame and ends on a later one. For that there is

```bash
scripts/walk/frames.sh
```

which runs the same build on a display of its own (GTK's Broadway, with its
own daemon and no port open), gives the window widths either side of a
breakpoint, and checks that the keyboard is where it was afterwards: in the
filter with what was typed there, on a card, on a commit. About a minute,
nothing on screen, and no compositor involved (`drive.py start-offscreen` and
`resize` are the same by hand).

Run the walk after changing anything a person can click, and `frames.sh` too
after changing what has the keyboard or how the header is laid out.

## Layout

| Path | What's in it |
| --- | --- |
| `src/main.rs` | CLI parsing, app bootstrap, the shared `CssProvider` |
| `src/config.rs` | `config.toml`, path expansion, terminal/editor resolution |
| `src/theme.rs` | Reads Omarchy's `colors.toml`, compiles the GTK stylesheet |
| `src/git.rs` | Repository discovery and inspection |
| `src/gitcmd.rs` | The one way a git command is run: time limit, process group, what git said |
| `src/commit.rs` | Reading a commit or the working tree, and the stage, commit and push writes |
| `src/graph.rs` | Revwalk → lane assignment for the commit graph |
| `src/meta.rs` | Tag store (`repos.toml`) |
| `src/stats.rs` | Contribution aggregation over a day window |
| `src/search.rs` | Commit search across many repositories |
| `src/scan.rs` | Worker threads streaming results to the main loop |
| `src/refresh.rs` | Background fetch and fast-forward, with the safety rules |
| `src/lang.rs` | Extension → language table |
| `src/testkit.rs` | Test fixtures: scratch repositories with a bare remote, an isolated home |
| `src/ui/window.rs` | Header, filter, sort, grid, selection, status bar, theme watcher |
| `src/ui/card.rs` | One repository card |
| `src/ui/graph.rs` | Commit graph page and its cairo row renderer |
| `src/ui/commit.rs` | The pane under the graph: changed files, diff, stage and commit |
| `src/ui/shortcuts.rs` | The keyboard, written down |
| `src/ui/debug.rs` | Debug builds only: what `scripts/walk` drives the window through |
| `scripts/walk/` | The walk: fixtures, the driver, the script that uses everything once, and the one that checks what needs frames |
| `scripts/shots/` | The README's pictures: the public repositories they are taken of, and the script that takes them |
| `docs/` | These pages, and the README's pictures in `docs/img` |
| `src/ui/stats.rs` | Contributions page |
| `src/ui/search.rs` | Commit search page |
| `src/ui/dialogs.rs` | Settings dialog and the tag editor |

## The README's pictures

The pictures in the README are of the program itself, taken by two scripts:

```bash
OMAREPOS_SHOTS_DIR=/some/scratch/dir scripts/shots/fleet.sh
OMAREPOS_SHOTS_DIR=/some/scratch/dir scripts/shots/shoot.sh
```

`fleet.sh` puts sixteen repositories in a home directory of its own, as
`~/code` there: a copy of this one, and clones of the fifteen public
repositories that had the most stars on GitHub under a search for "omarchy"
when the pictures were last taken. Those are cloned without signing in, and
git is given a configuration that reaches no credential helper. So that there
is something to show, one of the copies
is then wound back two days and given an uncommitted edit, two more are given
edits, one is put on a branch of its own and one on a release. That is done to
the copies, never to anything else, and only under a directory the script made
itself.

`shoot.sh` starts a debug build there on a display of its own (the one
`frames.sh` uses), with its own home, configuration, state and tags, and in a
stock Omarchy theme. It walks the window through each page with `drive.py`,
takes each picture at twice its size with `shot`, and writes them to
`docs/img`. What a picture shows of the machine it was taken on is this
repository, and the name and e-mail address on its last commit, which is who
the pictures are taken as (the commit box prints both, cut short). No other
repository, tag, theme or account of whoever runs it is in one. Nothing is fetched or pushed while it runs: git, as that window runs it,
is pointed at an address that is not there. It needs Omarchy's stock themes
and ImageMagick besides what `frames.sh` needs. About 1.2 GB of clones, and
two minutes to take the pictures. Delete the scratch directory afterwards.

The same window can be driven by hand for a picture of anything else:
`drive.py resize 1360x1010`, `drive.py split 440` for the divider of a commit
graph, and `drive.py shot FILE.png 2`.

---

[Back to the README](../README.md) · [All documentation](README.md)
