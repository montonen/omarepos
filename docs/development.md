# Development

## Build and run

```bash
cargo run --release
```

Runtime dependencies: GTK 4.12+, libadwaita 1.5+, and `git` itself, which
does the fetching. Build dependencies: Rust 1.92 or newer, a C toolchain,
`pkg-config`, and the GTK 4 and libadwaita development files. libgit2
is linked from the system when it is installed, and built from the copy `git2`
carries when it is not.

On Arch / Omarchy:

```bash
sudo pacman -S --needed gtk4 libadwaita libgit2 git rust base-devel
```

`cargo test` builds its own repositories under `target/test-fixtures`, each
with a bare remote beside it and a home directory of its own, and touches
nothing else on the machine. It needs `git` and no display. Every push and
pull request is built and tested on GitHub
(`.github/workflows/ci.yml`), in an Arch container with the packages above.

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
grid and its filter, every commit and file of every repository, a fetch of
each, the keys that used to stage and commit, the commit search,
contributions, tags and the tag editor, light, broken and missing themes,
Settings row by row, and the config and tag files broken and mended under the
running window. After each of some five hundred steps it runs the drawing code
and checks that the program is still there; it exits non-zero, with the panic,
if it is not. And since omarepos only reads and fetches, but for `p`, it takes
down every fixture's branches, tags, index and files before it starts and
compares them at the end: anything that had been written would show there.
The one fixture `p` is pressed on while clean and behind is checked instead
to have been brought level with its upstream, and clean. A window driven this
way opens no browser: it writes down the page it would have opened, and the
walk checks that each way of asking wrote the right one. `walk.sh quick` does
one repository instead of eight. It needs Hyprland, `python3`, `gdbus` and
`busctl`, takes about ten minutes, puts nothing on screen and touches nothing
of yours: everything goes over D-Bus to actions that only a debug build has,
and only when it is started with `OMAREPOS_DUMP=<file>` (`src/ui/debug.rs`).
`scripts/walk/drive.py` is the same thing by hand: `dump` what is on screen
as text, `press` a button by what it says, hand the app a `key`, set a
settings `row`, `pick` a list row, `point` a pointer it does not have at a
card, `paint`, and `shot` for a picture of the window as a PNG, which is the
one way to see what a change looks like without putting the window on screen.
A release build has none of it.

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

## The arcade

The Contributions page has an easter egg: the Konami code (↑ ↑ ↓ ↓ ← → ← → B
A) typed there turns it into an arcade's high-score table, with stages and
badges, and Escape is game over. It is left out of the README, the keyboard
list and the user docs on purpose. `src/ui/arcade.rs` says how a score is
made and who plays (co-authors from `Co-authored-by` trailers, which
`src/stats.rs` reads for it and leaves out of the page's own figures). It
reads nothing the page has not; the one thing it writes is its font, Press
Start 2P, built into the binary and put in `$XDG_RUNTIME_DIR/omarepos` for
Pango the first time the code is typed. On a Pango older than 1.56 it is in
monospace instead. The walk types the code and checks the table.

## Layout

| Path | What's in it |
| --- | --- |
| `src/main.rs` | CLI parsing, which hands `--json` over before anything of GTK's starts; app bootstrap, the shared `CssProvider` |
| `src/config.rs` | `config.toml`, path expansion, terminal/editor resolution |
| `src/theme.rs` | Reads Omarchy's `colors.toml`, compiles the GTK stylesheet |
| `src/git.rs` | Repository discovery and inspection |
| `src/gitcmd.rs` | The one way a git command is run, which is only ever a fetch: time limit, process group, what git said |
| `src/commit.rs` | Reading a commit or the working tree: the changed files and their diffs |
| `src/graph.rs` | Revwalk → lane assignment for the commit graph |
| `src/meta.rs` | Tag store (`repos.toml`) |
| `src/stats.rs` | Contribution aggregation over a day window |
| `src/ui/arcade.rs` | The Contributions page's easter egg: see above |
| `src/search.rs` | Commit search across many repositories |
| `src/scan.rs` | Worker threads streaming results to the main loop, or to `--json` |
| `src/snapshot.rs` | The last scan's readings, kept in the state directory so a launch can show the grid at once |
| `src/fleet_json.rs` | `--json`: the fleet as JSON on standard output, read as the grid reads it, in a schema with a version of its own |
| `src/refresh.rs` | Fetching: one repository, or a pass across the fleet with what may run on it fenced off |
| `src/lang.rs` | Extension → language table |
| `src/testkit.rs` | Test fixtures: scratch repositories with a bare remote, an isolated home |
| `src/ui/window/` | The main window. `mod.rs` composes the grid and its pages; `grid.rs` keeps the cards up to date by touching only those that changed, `scanning.rs` fills the grid, from the last run's readings first, `query.rs` is the filter and the sort, `keyboard.rs` the keyboard and the pointer, `refreshing.rs` the window's side of fetching, `tags.rs` the tag chrome |
| `src/ui/card.rs` | One repository card |
| `src/ui/graph.rs` | Commit graph page and its cairo row renderer |
| `src/ui/commit.rs` | The pane under the graph: changed files and their diff |
| `src/ui/shortcuts.rs` | The keyboard, written down |
| `src/ui/debug.rs` | Debug builds only: what `scripts/walk` drives the window through |
| `scripts/walk/` | The walk: fixtures, the driver, the script that uses everything once, and the one that checks what needs frames |
| `scripts/shots/` | The README's pictures and the race's video: the public repositories they are taken of, and the scripts that take them |
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
`~/Work` there: a copy of this one, and clones of the fifteen public
repositories that had the most stars on GitHub under a search for "omarchy"
when the pictures were last taken. Those are cloned without signing in, and
git is given a configuration that reaches no credential helper. So that there
is something to show, three of the copies are then wound back a few commits,
with what has happened since still to pull; three are given uncommitted edits;
one is put on a branch of its own and one on a release. That is done to the
copies, never to anything else, and only under a directory the script made
itself.

`shoot.sh` starts a debug build there on a display of its own (the one
`frames.sh` uses), with its own home, configuration, state and tags, and in a
stock Omarchy theme. It walks the window through each page with `drive.py`,
takes each picture at twice its size with `shot`, and writes them to
`docs/img`. What a picture shows of the machine it was taken on is this
repository and the name on its last commit, which is who the pictures are
taken as. No other repository, tag, theme or account of whoever runs it is in
one. Nothing is fetched or pushed while it runs: git, as that window runs it,
is pointed at an address that is not there. It needs Omarchy's stock themes
and ImageMagick besides what `frames.sh` needs. About 1.2 GB of clones, and
two minutes to take the pictures. Delete the scratch directory afterwards.

The same window can be driven by hand for a picture of anything else:
`drive.py resize 1360x1010`, `drive.py split 440` for the divider of a commit
graph, and `drive.py shot FILE.png 2`.

A video of the race (Contributions, `r`) over the same fleet is a third:

```bash
OMAREPOS_SHOTS_DIR=/some/scratch/dir scripts/shots/race.sh
```

It opens Contributions over the last 12 months, starts the race, and moves it
on a thirtieth of a second at a time with `race.frame`, a page action only a
debug build has, which also stops the race's own clock. Each frame is drawn
with `render` and not `shot`. `shot` is of the last frame the display drew,
which is gone from the moment anything changes until it draws the next, and
that display draws about once a second; `render` lays the window out and
draws it there and then. 601 frames at twice the size, scaled down and
encoded with ffmpeg as `race.mp4` and `race.webm`, with the last frame held
three seconds and kept as `race-poster.png`. About six minutes. `drive.py act
page-act race.frame=start` (and `=33.3`) is the same by hand: `page-act`
reaches any action of the page in front.

---

[Back to the README](../README.md) · [All documentation](README.md)
