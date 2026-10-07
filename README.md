# omarepos

An Omarchy-styled overview of every git repo on your machine.

A GTK4 / libadwaita desktop app with three views:

- **Fleet** — one card per repository: current branch, working-tree state, how far
  ahead or behind its upstream it is, stashes, language mix, and the last commit.
  A repository that is behind and cannot catch up on its own is marked **held
  back**, with the reason on hover — being behind and being stuck look nothing
  alike once you know which you are looking at.
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
│  main   ~3 ?2   2 behind  held back  │
│ github.com/you/omarchy-plugins       │
│ ███████████▌▌▌▌▎▎                    │
│ ● QML 54%  ● Shell 28%  ● Rust 11%   │
│ 3 hours ago · remote 1 hour ago · …  │
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
o.bind("SUPER + SHIFT + R", "Git repos (omarepos)", { launch = "omarepos" })
```

`SUPER+SHIFT` is Omarchy's tier for applications — plain `SUPER` is window
management, `SUPER+CTRL` is system settings, and a second or third app on the
same letter goes to `SUPER+SHIFT+ALT` and `SUPER+SHIFT+CTRL`. `launch` wraps the
command in `uwsm-app`, which puts it in its own systemd scope. Check the key is
free first with `omarchy menu keybindings --print`, and validate with
`hyprctl reload && hyprctl configerrors`.

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
| `Ctrl+R` / `F5` | Rescan (local only) |
| `Ctrl+Shift+R` | Fetch now |
| `Ctrl+L` | Jump to the grid |
| `Ctrl+F` / `/` | Focus the filter box |
| `Ctrl+Shift+F` | Search commits across these repos |
| `Enter` in the filter | Open the commit graph of the top hit |
| `Ctrl+A` | Select every repo shown; again to deselect |
| `Esc` | Back out: what you are typing, then the page, then the selection, then the filter |
| `Ctrl+G` | Contributions for the selected repos, or all of them |
| `Ctrl+Shift+A` | Clear the selection |
| `lang:rust` in the filter | Narrow by language |
| `Ctrl+,` | Settings |
| `Alt+←` | Back to the grid |
| `Ctrl+Q` | Quit |
| Click a card, or `Enter` on a focused one | Open its commit graph |
| `Enter` on a search result | Open that repository's graph |
| `Enter` on a commit-graph row | Copy its sha |
| `Down` from the commit-search box | Move into the results |
| `Space` on a focused card | Tick it for the contributions scope |
| `F1` or `?` | The whole keyboard, written down |
| `l` on a focused card, or in a graph | Open a terminal there — lazygit |
| `e` | Open it in your editor |
| `p` | Fetch and fast-forward that repo |
| `f` | Fetch only, leaving your files alone |
| `Space` in the changed files | Stage or unstage that file |
| `c` in the changed files | Write a commit message |

The filter matches repo name, path, branch, remote and language. The warning
toggle in the header narrows the grid to repos that are dirty, ahead, behind, or
unreadable. Each card's checkbox adds it to the contributions scope — once
anything is ticked the rest of the grid dims, so the selection stays visible
while you scroll. The `⋮` menu holds the terminal, editor and clipboard actions. **Open in terminal**
starts lazygit in the repository when it is installed, and leaves you in a shell
there when you quit it — so it is both, not one instead of the other. Change or
disable that in settings.

## Adding repositories

Three ways, in order of convenience:

1. **Settings** (`Ctrl+,` or the gear in the header) — add a scan root with a
   folder picker, add a single repository that lives anywhere, or change how deep
   the scan goes. Each root expands to its own options: whether to look inside
   repositories, a tag to apply to what it finds, and whether the containing
   repository itself gets a card. Edits are written to the config file
   immediately and the grid rescans.
2. **The config file**, `~/.config/omarepos/config.toml`: `roots` for folders to
   search, `extra_repos` for individual repositories outside them.
3. **`--root <PATH>`** on the command line, repeatable, for a one-off look at
   somewhere else. It overrides the configured roots for that run and is not saved.

A root is searched `max_depth` levels down and stops descending at the first
repository it finds, so `~/code` picks up `~/code/foo` and `~/code/plugins/bar`
without ever walking into their contents. `extra_repos` entries are included
whatever their depth, which is the escape hatch for a repo in an awkward place.

### Workspace repositories

Some folders are themselves a repository that holds nothing but other people's
clones — a `.gitignore` of `*`, a README, and thirty checkouts beside them.
Stopping at the first `.git` finds the wrapper and misses everything in it.

Mark such a root as a *workspace*, in settings or in the config file:

```toml
roots = [
  "~/code",
  { path = "~/work/acme", nested = true, tag = "acme" },
]
```

`nested` lifts the stop-at-`.git` rule for that folder, and **only** for the
subdirectories the folder's own repository *ignores*. That is the whole test,
and it is the right one: a parked clone is ignored, while a submodule is
tracked. Submodule paths are then excluded outright as well, so an unusual
ignore configuration cannot let one in by accident. Below that first level the
normal rule applies again — a clone's own submodules stay out of the grid.

`tag` tags what is found there, so thirty new cards do not drown the ones you
had. It is reconciled on every scan — at startup, on `Ctrl+R`, and after any
settings edit — and the root that applied each tag is recorded in `repos.toml`,
which is what makes the following all behave:

| You do this | What happens |
| --- | --- |
| Clone a new repo into the workspace | Tagged on the next scan |
| Delete the tag from one repo | Stays deleted; the next scan will not put it back |
| Rename the root's tag | Renamed in place — the old tag is removed, not left beside the new one |
| Rename it on a repo whose tag you deleted | Nothing; a rename follows the tag, it does not resurrect one |
| Clear the root's tag | The tag it applied is retired from every repo under it |
| Tag a repo yourself | Never touched by any of the above |

Entries are keyed by absolute path, and ones whose repository has gone are
dropped after a scan — but only when the parent directory is still there, so an
unmounted disk does not cost you its tags.

`include_self` controls whether the wrapper repository gets a card of its own.
It defaults to false for a workspace, since a repo that ignores everything has
nothing to report.

## Tags

Tags are omarepos's own metadata — nothing is written into your repositories. They
live in `~/.config/omarepos/repos.toml`, keyed by absolute path.

- A root can tag what it discovers — see *Workspace repositories* above.
- Tag one repo from its `⋮` menu, or several at once: tick their checkboxes, then
  **Tags → Tag N selected…**. With nothing ticked, that button tags everything
  currently shown, so the filter box doubles as a bulk selector.
- Editing several at once only replaces the tags they have *in common*; tags
  unique to one of them survive.
- With one or two tags they appear as toggle chips in the header — one click to
  filter instead of three. From three tags on, the **Tags** popover takes over,
  since more than that crowds out the title.
- The popover filters the grid. Several ticked tags is an OR. `tag:work`
  typed in the filter box is an AND, and stacks with the rest of the query —
  `tag:work ruby` means tagged work *and* matching "ruby".
- Tag colours are derived from the tag's own name, so a tag keeps its colour
  across repos and across runs.

## Background refresh

Off by default. Turn it on in settings, or in the config file:

```toml
[refresh]
every_minutes = 15     # 0 turns it off
fetch = true           # run `git fetch`; touches the network, not your files
fast_forward = true    # update the code where that is unambiguously safe
only_tags = ["work"]   # blank means every repository
max_parallel = 4
timeout_seconds = 60
```

With `fetch` off it only re-reads local state. With `fetch` on, ahead/behind
stops going stale. With `fast_forward` on it also moves the checked-out branch.

`Ctrl+Shift+R` runs a pass immediately, whatever the schedule says. The pass
never pushes.

### What it will and will not do

The only write it ever performs is a fast-forward of the checked-out branch. A
repository is left alone, with the reason reported, when any of these hold:

| Condition | Why |
| --- | --- |
| Uncommitted changes | Updating under your edits is not the app's call |
| Commits of its own | A fast-forward would not be one |
| Detached HEAD | No branch to move |
| No upstream | Nothing to move it to |
| Merge or rebase in progress | You are in the middle of something |
| Bare, or no commits yet | Nothing to do |

Cleanliness is checked twice — once before the fetch and again immediately
before the fast-forward — because the fetch takes time and you may have started
editing during it. The update then runs `git merge --ff-only`, which refuses
anything that is not a fast-forward even if those checks were wrong. It never
merges, rebases, stashes, resets, forces, or touches submodules, and it only
moves the branch you have checked out.

Untracked files do not block an update: a fast-forward cannot silently
overwrite one, because git refuses rather than clobbering it.

### Why it shells out to `git`

Authentication lives in credential helpers, SSH agents, `insteadOf` rewrites and
`includeIf` identities — none of which libgit2 reads. Driving the same `git` you
drive means your setup applies unchanged, including per-directory identities. A
repository you cannot fetch by hand is one omarepos does not pretend it can
fetch either; it reports the failure and moves on.

Prompts are disabled (`GIT_TERMINAL_PROMPT=0`, and SSH in batch mode), so a
repository whose credentials have expired fails quickly instead of parking a
worker on a prompt nobody can answer. Each invocation has a timeout as a
backstop.

### Worth knowing

- It does not pause on battery or a metered connection.
- Fetching on a schedule is still traffic to your remotes, and on a work
  machine that traffic is visible to whoever runs them.
- Scope is the whole grid, or the tags in `only_tags` — deliberately not what
  the filter box is showing, so what you are looking at never changes what gets
  fetched.

## Searching commits

`Ctrl+Shift+F`, or the magnifier in the header. One query across every
repository in scope — the ticked ones, or everything the grid is showing, the
same rule contributions uses.

It matches, in that order of preference: the commit subject, the rest of the
message, the author's name or e-mail, and a sha prefix. Each result says which
of those matched, so a hit on an author reads differently from a hit on a
subject.

`Down` from the query moves into the results and `Up` from the first one comes
back. Activating a result — `Enter` or a click — opens that repository's commit graph
**on that commit**: it is sought out however far back it is, highlighted, and
scrolled to, with a window of history around it. The button at the end of the
row copies the full sha.

The graph normally draws the newest 500 commits, but a search reaches much
further than that, so following a result would otherwise land on the newest
commits rather than the one asked for. When a commit is named, the walk
continues until it is found — up to 20,000 — and the page renders a window
around it instead of everything in between.

Searches run on worker threads and are abandoned the moment the query changes,
so typing does not queue up passes over the fleet. A repository stops at 50,000
commits or 200 matches, and the newest 500 results are listed — the status bar
says when there were more.

## Commit graph

Click any card for its history. Lanes are assigned the usual way: a commit takes
the lane that was waiting for it, hands that lane to its first parent, and parks
any further parents in lanes of their own, so merges fan out and converge where
they actually did.

- Filled dot: ordinary commit. Ring: merge commit. Outer ring: something points
  at it.
- Pills are refs — the current branch, other local branches, tags, remote branches.
- Activating a row — `Enter` or a click — copies its full sha.
- The pane below shows what the selected commit changed, and follows the cursor
  as you walk the history. `Ctrl+D` or the header button hides it, and that
  choice is remembered.
- The newest 500 commits are shown; the status bar says so when there are more.
  Lanes past the sixteenth are clipped rather than squeezed.

Every branch is walked, not just the one checked out, so a commit that only exists
on a side branch still appears.

### Pushing

The header carries a **Push N** button, enabled when the branch is ahead of its
upstream and nothing is in the way. A branch that has never been pushed says
**Publish** instead, and only when the repository has exactly one remote —
guessing which remote to publish to is not a guess worth making.

Never forced, never `--all`, never tags, never another branch, and never from
the grid across a selection. It is refused, with the reason, when the branch is
behind (fast-forward first), has nothing ahead, or has no upstream and no single
remote. A `pre-push` hook's refusal is quoted back as git wrote it.

### What is uncommitted

A **Local changes** row sits above the history whenever the working tree has
anything in it, drawn dashed and hollow because it is not a commit. Selecting it
fills the same pane with everything uncommitted, grouped:

| Group | Blocks an update |
| --- | --- |
| Staged, Not staged, Conflicted | yes |
| **Untracked** | **no** |

Each group folds. Untracked starts folded past ten files, where it stops being
information and starts being noise.

The `+` beside a file stages it, `−` unstages it, and the box below commits what
is staged. Whole files only — staging a hunk is the heart of a committing tool
and omarepos is not one; `Open in terminal` starts lazygit for that. The commit
is refused, with the reason, when nothing is staged, the message is empty, or
the repository is mid-merge or mid-rebase.

All of it runs through `git` rather than libgit2, so hooks run, commits are
signed if you sign them, and the identity is whichever `includeIf gitdir:`
resolves for that directory.

That split is the point. Untracked files never stop a fast-forward — git refuses
rather than overwriting one — so they are shown stepped back and labelled,
rather than padding the same list as the changes that really are holding a
repository behind. Untracked directories are recursed into, since "two untracked
things" is what the card already says and this view exists to say which.

An untracked file has nothing to diff against, so its contents are shown as
added, the way `git diff --no-index` would.

The working tree changes outside the app, so it is re-read whenever the window
regains focus — edit in your editor, come back, and the list is current. A
commit cannot change under you, so nothing else reloads.

### What a commit changed

The pane below the graph lists the files, as a tree rather than a flat list —
ten paths sharing `src/Orders.Api.Application/Checkout/` are a wall of
identical prefixes otherwise. A chain of folders holding one thing each
collapses into a single row.

Each file carries its status (added, modified, deleted, renamed), and `+`/`−`.
Renames are detected, so a moved file reads as one fact rather than a delete and
an add. Binary files say so instead of claiming no lines changed.

A merge is diffed **against its first parent**, which shows what the merge
brought in rather than everything on both sides; the totals line says so.

Selecting a file shows its diff beside the tree, with the file's own language
highlighted and added and removed lines marked by a line background rather than
by colouring the text — so what changed and what it says are both readable at
once. Highlighting is syntect's bundled set, which has no TypeScript, JSX or
Bicep; those fall back to JavaScript, which gets strings, comments, keywords and
braces right.

Reading a diff costs more than reading a commit header, and the pane reloads
whenever the cursor moves, so it runs off the main thread, waits for the cursor
to settle, and abandons a read the moment you move on. Holding `Down` through a
history does not queue a hundred diffs.

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
- A row expands to the work itself: every commit, grouped by repository, with
  its sha, churn and age. The newest 100 per repository are kept; past that the
  group says how many older ones there were.

Line counts come from diffing each commit against its first parent, which is the
expensive part; a week across a dozen repos takes well under a second, but a
30-day window over a large fleet will visibly chew.

## Configuration

Optional, at `~/.config/omarepos/config.toml`:

Everything here is also editable from the settings dialog, which rewrites this
file (and drops your comments when it does).

```toml
# Directories to scan. `~` is expanded. A root is either a bare path, or a
# table when it needs options.
roots = [
  "~/code",
  { path = "~/work/acme", nested = true, tag = "acme", include_self = false },
]

# Individual repositories, included wherever they live.
extra_repos = ["~/.config/omarchy"]

# How many levels below each root to look for a `.git`. Descent stops at the
# first repository found, so nested checkouts stay out of the grid.
max_depth = 3

# Defaults to $TERMINAL, then the first of alacritty/ghostty/kitty/foot/wezterm.
terminal = "alacritty"

# Run in the terminal by "Open in terminal", before the shell takes over.
# Unset follows lazygit when it is installed; "" gives a plain shell.
terminal_runs = "lazygit"

# Defaults to $EDITOR, then nvim.
editor = "nvim"

# Follow the active Omarchy theme and restyle live when it changes.
follow_omarchy_theme = true
```

## How it reads your repos

Everything shown comes from libgit2 against the on-disk repository. Reading
never runs `git`, never writes to a repository and never talks to a remote.

Writing is narrow and deliberate. omarepos can stage a file, unstage a file,
commit what is staged, push the branch, and fast-forward a clean branch. It
cannot discard an edit: there is no checkout, no reset, no clean, no amend.
Everything it does is recoverable.

Pushing happens on a button press and never on the timer. Fetching is inward —
a mistake costs you your own time. Publishing is outward, cannot be taken back
without a force push, and on a work remote is visible to everyone. A timer must
not decide that something is ready to be seen.
Ahead/behind counts compare your local branch to the *last fetched* state of its
upstream, exactly like `git status` does; they go stale until something fetches.
When a branch is behind, the card also gives the age of the upstream tip — the
local commit's age alone reads as "nothing has happened here", which can be the
opposite of the truth.

The one exception is the background refresh below, which is off until you turn
it on.

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
| `src/search.rs` | Commit search across many repositories |
| `src/scan.rs` | Worker threads streaming results to the main loop |
| `src/refresh.rs` | Background fetch and fast-forward, with the safety rules |
| `src/lang.rs` | Extension → language + colour table |
| `src/ui/window.rs` | Header, filter, sort, grid, selection, status bar, theme watcher |
| `src/ui/card.rs` | One repository card |
| `src/ui/graph.rs` | Commit graph page and its cairo row renderer |
| `src/ui/stats.rs` | Contributions page |
| `src/ui/search.rs` | Commit search page |
| `src/ui/dialogs.rs` | Settings dialog and the tag editor |

## Status

Early, but the three views are complete and usable. Known rough edges: ahead/behind
and the graph both reflect the last fetch, nothing refreshes itself while the window
is open except on `Ctrl+R`, and the graph has no diff view yet.
