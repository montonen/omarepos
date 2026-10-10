<h1 align="center">
  <img src="data/se.alphadev.Omarepos.svg" width="76" alt=""><br>
  omarepos
</h1>

<p align="center">
  <strong>Every git repository on your machine, on one screen.</strong><br>
  A keyboard-first overview for <a href="https://omarchy.org">Omarchy</a>: what is uncommitted, what is unpushed
  and what is waiting to be pulled, in the theme you are using.
</p>

<p align="center">
  <a href="#a-tour">Tour</a> ·
  <a href="#install">Install</a> ·
  <a href="#keyboard">Keyboard</a> ·
  <a href="#what-it-will-and-will-not-do">What it will and will not do</a> ·
  <a href="docs/README.md">Documentation</a>
</p>

![omarepos: sixteen repositories as a grid of cards, in the Tokyo Night theme](docs/img/fleet.png)

omarepos is a GTK4 / libadwaita desktop app. It scans the folders you point it
at and gives every repository it finds a card: where it is, its branch and what
is uncommitted, what exists only here and what is waiting on the remote, the
last commit, and four weeks of activity. **A repository that is clean and level
with its remote is grey, and a coloured edge means there is something to do.**

From a card it is one key to the repository's commit graph, a terminal, your
editor or its page on the web. From the graph you can stage, commit and push.
And the whole fleet can be fetched on a timer and fast-forwarded where that is
unambiguously safe. There is nothing in it that discards work.

<sub>Pictured: this repository and fifteen public Omarchy projects, in a
scratch home directory of their own. [How the pictures are taken](docs/development.md#the-readmes-pictures).</sub>

## A tour

### A card says one thing in each place

![Three cards: one with commits that are only here, one held back by uncommitted work, one quiet](docs/img/cards.png)

Every fact has a place of its own, the same on every card, so where a thing is
says what it is.

| Row | What it says |
| --- | --- |
| Name | The repository |
| Where | The folder it lies in, and the remote it belongs to |
| Branch | The branch, and on the right what is uncommitted: `+1 ~3 ?4` is one staged, three modified, four untracked |
| Sync | `↑ 3 ahead` is what exists only here. `↓ 28 behind` is what is waiting, and **held back** is added when the refresh would have brought it in and something is in the way |
| Commit | The last commit: how old, what it says, and who wrote it when that was not you |
| Lately | Commits a day over the last four weeks, yours solid and other people's fainter |
| Tags | Your own labels, each with a dot in the tag's colour: one of eight, chosen by its name |

Apart from the dot on a tag, colour is kept for what needs you. The left edge
is yellow for uncommitted work, the accent colour for commits that exist
nowhere else, and red for trouble. The card `Enter` would open from the
filter, the first one in the picture at the top, shows the two commits before
the last in place of the four weeks. [Reading a card](docs/cards.md) has the
rest.

### Type, then Enter

![The filter narrowed to one tag, with the card Enter would open marked](docs/img/filter.png)

The filter has the keyboard when the window opens, and the launcher key brings
an open window back with the keyboard there again, unless a commit graph is in
front. Type a few letters of a name, a path, a branch or
a remote; `tag:work` and `lang:rust` narrow by tag and by language; `Enter`
opens the top hit. With up to five tags, each has a button in the header. The
warning triangle keeps only the repositories that need something.

### One repository

![The commit graph of a repository, with the files a commit changed and a diff below](docs/img/graph.png)

`Enter` on a card opens its commit graph: every branch, with lanes that fan out
and converge where the merges really were, and the branches, tags and remote
branches named on their commits. The pane below shows what the selected commit
changed, as a tree of files and a highlighted diff, and follows the cursor as
you walk the history. More in [Commit graph](docs/commit-graph.md).

### Stage, commit, push

![Local changes: a staged file, an unstaged one and an untracked one, and the commit box](docs/img/changes.png)

Uncommitted work is the first row of the graph. `Tab` goes into the changed
files, `Space` stages or unstages one, `c` goes to the message, `Enter`
commits, and **Push** sends the branch when it is ahead. All of it runs
through your own `git`, so your hooks, your signing and your per-directory
identities apply. Whole files only: for hunks, `l` opens a terminal in the
repository, in lazygit when it is installed.

### Every history, searched at once

![Commit search across all sixteen repositories](docs/img/search.png)

`Ctrl+Shift+F` searches the commits of every repository on the grid, or of the
ones you ticked. A commit matches on its subject, or else on the rest of its
message, its author or a sha prefix, and each result says which; the newest
come first. `Enter` opens that repository's graph on that commit. More in
[Searching commits](docs/search.md).

### Who did what

![Contributions over the last seven days, by author](docs/img/contributions.png)

`Ctrl+G` adds up the last 7, 14 or 30 days, across the grid or the
repositories you ticked: commits and lines per author, and commits per day,
with a mailmap honoured. A row opens to the commits themselves. A second view
counts every branch and not only the checked-out ones. More in
[Contributions](docs/contributions.md).

### A refresh that knows when to leave things alone

Off until you give it an interval. A timer in the window then reads the fleet
again, fetches it when `fetch` is on, and with `fast_forward` on moves the
checked-out branch of each repository where nothing is in the way. A
repository that is behind and has uncommitted changes or commits of its own is
fetched and left where it was: its card says **held back**, with the reason on
hover and on the card that has the keyboard. The timer never pushes, runs no
hooks, and only enters repositories you have already seen. More in
[Background refresh](docs/refresh.md).

### Your theme

![The same four cards in Catppuccin Latte, Gruvbox, Ethereal and Rose Pine](docs/img/themes.png)

The window, the cards and the text take their colours from the active Omarchy
theme, used the way Omarchy's own templates use it. `omarchy theme set`
repaints the window as it stands: nothing is cached and nothing needs
restarting. The lanes of a graph and the dots on tags and authors keep a fixed
set of eight colours, which a theme's own handful of hues could not keep
apart. More in [Configuration and theme](docs/configuration.md#theme).

## What it will and will not do

A tool that looks at every repository you have, and can write to them, should
say exactly what it does.

- **Reading never writes to a repository and never talks to a remote.** What
  is shown is read from disk with libgit2. The one question put to `git`
  itself is who a commit would be recorded as, for the name beside the Commit
  button.
- **Besides fetching, it does five things to a repository:** stage a file,
  unstage a file, commit what is staged, push the branch, and fast-forward the
  checked-out branch. The timer does the last only where the working tree is
  clean. `p` leaves it to git, which keeps your edits where the update does
  not touch the same files and refuses, changing nothing, where it does.
- **It cannot discard an edit.** There is no checkout, no reset of a branch,
  no clean, no amend and no stash. The only merge it runs is `--ff-only`, and
  nothing is rebased or forced.
- **Pushing happens on a button press and never on the timer.** One branch at
  a time: never forced, never `--all`, never tags.
- **Writes go through the `git` you use yourself,** with your credential
  helpers, SSH setup and identities. A repository you cannot fetch by hand is
  one omarepos does not pretend it can fetch either.
- **Tags are omarepos's own.** Nothing is written into your repositories to
  keep them.

The reasoning, and the cases at the edges, are in
[Reading and writing](docs/reading-and-writing.md) and
[Background refresh](docs/refresh.md#what-it-will-and-will-not-do).

## Install

omarepos is built from source. It needs GTK 4.12 or newer, libadwaita 1.5 or
newer and `git` 2.29 or newer to run; and to build, Rust 1.92 or newer, a C
toolchain, `pkg-config` and the GTK 4 and libadwaita development files. On
Arch and Omarchy that is:

```bash
sudo pacman -S --needed gtk4 libadwaita libgit2 git rust base-devel
```

```bash
git clone https://github.com/montonen/omarepos.git
cd omarepos
cargo install --path .
mkdir -p ~/.local/bin
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
o.bind("SUPER + SHIFT + R", "Git repos (omarepos)", { launch = "omarepos" })
```

`SUPER+SHIFT` is Omarchy's tier for applications — plain `SUPER` is window
management, `SUPER+CTRL` is system settings, and a second or third app on the
same letter goes to `SUPER+SHIFT+ALT` and `SUPER+SHIFT+CTRL`. `launch` wraps the
command in `uwsm-app`, which puts it in its own systemd scope. Check the key is
free first with `omarchy menu keybindings --print`, and validate with
`hyprctl reload && hyprctl configerrors`.

### Or have your agent install it

If a coding agent works on your machine (Claude Code, or any other that can
run commands there), give it this prompt:

```text
Install omarepos from https://github.com/montonen/omarepos on this machine.

1. Clone it to ~/code/omarepos, creating ~/code if it is not there. If
   ~/code/omarepos exists already, stop and ask me.
2. Read the "Install" section of README.md in the clone and do what it says:
   the packages (ask me before running anything with sudo), then
   `cargo install --path .`, the symlink into ~/.local/bin, the desktop entry
   and the icon.
3. Add the Hyprland keybinding the README gives, on SUPER + SHIFT + R. First
   check that the key is free with `omarchy menu keybindings --print`. If it
   is taken, tell me what has it and propose another key. Back up
   ~/.config/hypr/bindings.lua before editing it, then run
   `hyprctl reload && hyprctl configerrors` and show me what they say.
4. Ask me which folders hold my git repositories. If the answer is anything
   but ~/code alone, write them as `roots` in ~/.config/omarepos/config.toml,
   after the pattern of config.example.toml in the clone.
5. Check that `~/.local/bin/omarepos --version` runs. Do not start the app,
   and change nothing else on the machine.

Then tell me what you installed and where, and anything you skipped.
```

It is built from source, so give it a few minutes. To update later, ask the
agent to pull in `~/code/omarepos` and run `cargo install --path .` again. A
window that was open through the update is still the old program, and says so
when you bring it forward.

## Getting started

Start it. With no configuration it looks for repositories under `~/code`,
three levels down.

To look elsewhere, open **Settings** (`Ctrl+,`) and add a folder to scan or a
single repository, or write the same thing in
`~/.config/omarepos/config.toml`:

```toml
roots = [
  "~/code",
  { path = "~/work/acme", nested = true, tag = "acme" },
]
extra_repos = ["~/.config/omarchy"]
```

`omarepos --root <PATH>` is a one-off look at somewhere else, in a window of its
own: the path is not saved, no root tags are applied and no background refresh
runs. [Repositories and tags](docs/repositories.md) covers scan roots, folders
that hold nothing but other clones, and tagging.

## Keyboard

`F1`, or `?` on the grid and in a graph, lists the keys. The ones that carry
most of the work:

| Key | Does |
| --- | --- |
| Type, then `Enter` | Narrow the grid, and open the top hit's commit graph |
| `Down` in the filter | Move into the cards |
| `Enter` on a card | Open its commit graph |
| `l` · `e` · `o` | Open a terminal there (lazygit), your editor, or its page on the web |
| `p` · `f` | Fetch and fast-forward that repository · fetch only |
| `Space` on a card | Tick it, to scope the search and contributions |
| `Ctrl+Shift+F` | Search commits across the repositories |
| `Ctrl+G` | Contributions |
| `Ctrl+R` · `Ctrl+Shift+R` | Rescan, local only · run the refresh now |
| `Tab` in a graph, then `Space` · `c` · `Enter` | Into the changed files, then stage or unstage · write the message · commit |
| `Ctrl+,` | Settings |
| `Esc` | Back out, one step at a time |

More of them are in [Usage](docs/usage.md).

## Documentation

| Page | What is in it |
| --- | --- |
| [Usage](docs/usage.md) | Command line, the keys, how the filter matches |
| [Reading a card](docs/cards.md) | Each row of a card, what the colours mean, the four weeks |
| [Repositories and tags](docs/repositories.md) | Scan roots, workspace folders, tags |
| [Commit graph](docs/commit-graph.md) | The graph, pushing, staging and committing, what a commit changed |
| [Searching commits](docs/search.md) | What is matched, and how far back |
| [Contributions](docs/contributions.md) | What is counted, and the all-branches view |
| [Background refresh](docs/refresh.md) | The timer, what it updates and what it leaves alone, and why it runs `git` |
| [Configuration and theme](docs/configuration.md) | `config.toml`, and how the Omarchy theme is used |
| [Reading and writing](docs/reading-and-writing.md) | What is read, the few things that are written, and why pushing is a button |
| [Development](docs/development.md) | Building, the tests, the walk, the source layout, the pictures |

## Development

```bash
cargo run --release      # build and run
cargo test               # builds its own scratch repositories; touches nothing else
scripts/walk/walk.sh     # drives a debug build through every page and action
```

A panic inside a GTK callback ends the program, and most of what a window does
cannot be reached by a unit test. So besides the unit tests there is a walk: a
debug build on a hidden workspace, driven over D-Bus through some five hundred
steps on fixture repositories. [Development](docs/development.md) has the
details, and the source layout.

## Status

Early, and in daily use by one person. The grid, the commit graph with its
diff pane, contributions, commit search, the background refresh and the
stage-commit-push path are all there. Ahead and behind are as fresh as the last
fetch: with the refresh off, that is the last time you pressed `f`, `p` or
`Ctrl+Shift+R`.

## License

MIT. See [LICENSE](LICENSE).
