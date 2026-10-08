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
settings `row`, `pick` a list row, `paint`. A release build has none of it.

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

## Install

```bash
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

## Usage

```
omarepos [options]

  -r, --root <PATH>   Directory to scan (repeatable; overrides the config file)
  -d, --depth <N>     How deep below each root to look for repos
  -h, --help          Show help
  -V, --version       Show the version
```

The filter has the keyboard when the window opens, so a repository is its
name and `Enter` away: type, and the grid narrows; `Enter` opens the top hit.
The launcher key does the same to a window that was already open, with what
was left in the filter selected, so that what you type replaces it. (Only with
the grid in front. A commit graph you left open keeps the keyboard.) `Down`
moves from the filter into the cards.

| Key | Action |
| --- | --- |
| `Ctrl+R` / `F5` | Rescan (local only). In a commit graph: read that repository again |
| `Ctrl+Shift+R` | Run the refresh now: fetch, and fast-forward if that is switched on |
| `Ctrl+L` | Jump to the grid |
| `Ctrl+F`, or `/` with the keyboard on a card | Focus the filter box |
| `Ctrl+Shift+F` | Search commits across these repos |
| `Enter` in the filter | Open the commit graph of the top hit |
| `Down` in the filter | Move into the cards |
| `Ctrl+A` | Select every repo shown; again to deselect |
| `Esc` | Back out: a dialog or menu, the filter you are typing in, then the page, then the selection, then the filter |
| `Esc` in the commit message | Step out of the field. The message stays |
| `Ctrl+G` | Contributions for the selected repos, or all of them |
| `Ctrl+Shift+A` | Clear the selection |
| `lang:rust` in the filter | Narrow by language |
| `Ctrl+,` | Settings |
| `Alt+←` | Back to the grid |
| `Ctrl+Q` | Quit |
| Click a card, or `Enter` on a focused one | Open its commit graph |
| `Enter` on a search result | Open that repository's graph |
| `Enter` on a commit-graph row, or a double click | Copy its sha |
| `Down` from the commit-search box | Move into the results |
| `Space` on a focused card | Tick it for the contributions scope |
| `F1`, or `?` on the grid and in a graph | The whole keyboard, written down |
| `l` on a focused card, or in a graph | Open a terminal there — lazygit |
| `e` | Open it in your editor |
| `o` | Open its page on the web — GitHub, or wherever the remote is |
| `p` | Fetch and fast-forward that repo |
| `f` | Fetch only, leaving your files alone |
| `Space` in the changed files | Stage or unstage that file |
| `c` in the changed files | Write a commit message |
| `Enter` in the message | Commit what is staged |

`l`, `e`, `o`, `p` and `f` act once for each press. A key held down, or one
that sticks, does not open a terminal per repeat or start a second fetch.

The filter matches repo name, path, branch and remote. Every word typed has to
be found, in any order, so `acme billing` finds the billing repository under
`acme`. `tag:work` and `lang:rust` narrow by tag and by language. The warning
toggle in the header narrows the grid to repos that are dirty, ahead, behind,
unreadable, holding commits that are on no remote, or that the last fetch or
update failed on.

"Ahead" is measured against the branch's upstream, and says nothing when there
is none. So a card also says when HEAD has commits that exist on no remote at
all — `3 unpublished` for a branch that was never pushed or whose upstream is
gone, `no remote` for a repository that has nowhere to push — and `+2 branches`
when other local branches hold such commits. Those are the cases that used to
read "clean". Each card's checkbox adds it to the contributions scope — once
anything is ticked the rest of the grid dims, so the selection stays visible
while you scroll. The `⋮` menu holds the terminal, editor, web page and clipboard actions. **Open in terminal**
starts lazygit in the repository when it is installed, and leaves you in a shell
there when you quit it — so it is both, not one instead of the other. Change or
disable that in settings.

**Open on GitHub** — in the same menu, on `o`, and on the globe in a commit
graph's header — sends your browser to the repository's page. The address is
worked out from the remote the card names (`origin`, or the first one there
is): `git@github.com:acme/widgets.git` and `https://github.com/acme/widgets.git`
both become `https://github.com/acme/widgets`. A user name or token written
into the remote is left out, and so is an ssh port; an `http://` remote keeps
its port, being a web address already. The tooltip shows exactly what will be
opened, and the menu names the site: GitHub, GitLab, Bitbucket, Codeberg and
Azure DevOps by name, anything else by its host.

It is not offered where no page can be worked out: a repository with no remote,
a remote that is a directory, and an ssh remote whose host has no dot in it —
that is a name from `~/.ssh/config`, and only ssh knows which host it stands
for. Pressing `o` there says so. Elsewhere the address is the convention every
common forge keeps (the host, then the path), which a server of your own may
not follow.

## Adding repositories

Three ways, in order of convenience:

1. **Settings** (`Ctrl+,` or the gear in the header) — add a scan root with a
   folder picker, add a single repository that lives anywhere, or change how deep
   the scan goes. Each root expands to its own options: whether to look inside
   repositories, a tag to apply to what it finds, and whether the containing
   repository itself gets a card. Edits are written to the config file
   immediately and the grid rescans.
2. **The config file**, `~/.config/omarepos/config.toml`: `roots` for folders to
   search, `extra_repos` for individual repositories outside them. If the file
   cannot be parsed, omarepos says so in a banner, runs on the defaults, and
   saves nothing until it is fixed — it will not write its defaults over a file
   with a typo in it. The same goes for the tag file, `repos.toml`. Each save
   keeps the version it replaced beside it as `config.toml.bak`.
3. **`--root <PATH>`** on the command line, repeatable, for a one-off look at
   somewhere else. It overrides the configured roots for that run and is not
   saved. A one-off look opens its own window even when omarepos is already
   running, applies no root tags, and does no background refresh.

Otherwise there is one window: starting omarepos while it is already open
brings that window forward.

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

`nested` lifts the stop-at-`.git` rule for that folder: the clones its own
repository *ignores* are found, at any depth, so a `.gitignore` of `repos/*`
works as well as one of `*`. A parked clone is ignored; the folder's own
submodules are not what is being looked for, and are left out by path wherever
they sit. Inside a clone found this way the normal rule applies again — its own
submodules stay out of the grid.

A clone the workspace does *not* ignore is left out, and the status bar and the
root's row in Settings say so by name, since that is nearly always a missing
ignore rule and not a decision. The same two places say when a root gave
nothing at all: the folder has been moved, or is not a folder, or holds no
repositories. A workspace whose own repository will not open is searched as a
plain folder, so its clones stay on the grid.

When one root sits inside another, a repository belongs to the narrower one,
whichever was listed first: that is the root whose tag it gets.

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
| Give a root a tag some repos already carry | Theirs stays theirs: the root adopts it there, and never takes it away |
| Remove a root | Its repositories are forgotten, tags and all, unless something else still reaches them. Asked first when hand-made tags would go |

A root's tag is one word: the filter, the tag editor and the refresh scope all
split on spaces and commas, so the field refuses a tag that has either.

Entries are keyed by absolute path, and ones whose repository has gone are
dropped after a scan — but only when the parent directory is still there with
something in it, so neither an unmounted disk nor its empty mount point costs
you its tags.

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
  since more than that crowds out the title. Chips and counts are of the
  repositories on the grid, not of everything the file remembers.
- `Enter` in the tag editor saves.
- `repos.toml` can be edited by hand while the window is open, on the same
  terms as the config file: read again at the next rescan, and never saved over
  in between.
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
only_tags = []          # e.g. ["work"]; blank means every repository
max_parallel = 4
timeout_seconds = 60
```

With `fetch` off it only re-reads local state. With `fetch` on, ahead/behind
stops going stale. With `fast_forward` on it also moves the checked-out branch,
and fetches to do it whatever `fetch` says.

It is a timer in the window, not a service: nothing is fetched while omarepos
is closed. A pass that fell due in the meantime runs at the next launch, once
the first scan has finished. Opened and closed like a launcher, that makes it a
fetch at launch more than a fetch every fifteen minutes.

`Ctrl+Shift+R` runs a pass immediately, whatever the schedule says. The pass
never pushes. Changing a refresh setting, or the scan roots, calls off a pass
that is under way: repositories it had not reached are left alone, and one
that was being fetched is not then fast-forwarded.

The status bar says what the timer is doing, and what it is not. `fetched 5
minutes ago` is the last pass that reached a remote: a pass run offline does
not count as one, so the next launch tries again. With `only_tags` set the
stamp names the tags it covered, and when nothing carries any of them it reads
`auto fetch idle: nothing is tagged work` in place of a schedule that would
fetch nothing. `2 not fetched` and `1 not updated` count the cards carrying
those pills.

A remote that turned a fetch away (a login refused, a repository that is not
there any more) is not asked again by the timer. Trying every few minutes with
the same credential changes nothing, and is how an account gets locked. The
card keeps its `fetch failed` pill; `f`, `p` or `Ctrl+Shift+R` try again, and a
fetch that works clears it. A remote that could not be reached is different,
and is tried on every pass.

### What it will and will not do

The only write it ever performs is a fast-forward of the checked-out branch. A
repository is left alone, with the reason reported, when any of these hold:

| Condition | Why |
| --- | --- |
| Uncommitted changes | Updating under your edits is not the app's call — on a pass across the fleet. `p` on that repository asks git, which keeps your changes where the update does not touch the same files and refuses where it does |
| Commits of its own | A fast-forward would not be one |
| Detached HEAD | No branch to move |
| No upstream | Nothing to move it to |
| Merge or rebase in progress | You are in the middle of something |
| Bare, or no commits yet | Nothing to do |
| The update would move a checked-out submodule | The submodule's own checkout would be left behind it. Pull that one yourself |
| The branch was moved back by hand | You undid an update (`git reset --hard ORIG_HEAD`); a pass across the fleet does not put it back. `p` does |
| The upstream was wound back | Commits you have were taken off the remote. That is not "ahead", and nothing here will push them back |

These are about *updating*. A pass that only fetches (`f`, or the refresh with
`fast_forward` off) fetches a detached, mid-merge or diverged repository like
any other, and reports what it found: `fetched, 3 behind`.

Cleanliness is checked after the fetch, immediately before the fast-forward,
because the fetch takes time and you may have started editing during it. The
update then runs `git merge --ff-only` with the settings that could make it
something else overridden for that one command (`branch.<name>.mergeoptions`,
`pull.twohead`, squash, autostash), and afterwards the branch is read back:
only "now at its upstream" counts as updated. It never merges, rebases,
stashes, resets or forces, never goes into submodules, and only moves the
branch you have checked out.

A fast-forward that fails part-way (a full disk, a file held open) leaves some
files already rewritten under a branch that has not moved. The card says "held
back", and its tooltip names those files and how to put them right, and goes
on saying it on later passes until it is.

Untracked files do not block an update: a fast-forward cannot silently
overwrite one, because git refuses rather than clobbering it. Ignored files are
another matter — left to itself, git *does* overwrite an ignored file that is
in the way of an incoming one, and says nothing. The fast-forward therefore
runs with `--no-overwrite-ignore`, and git refuses for those too. When a file
of yours is in the way the repository is fetched and left where it was; its
card says "held back", and the tooltip quotes git naming the file.

A repository is only touched if it is really the repository *of* the directory
it was found in. A `.git` can claim that its files live somewhere else
(`core.worktree`), or be a file naming another repository's git data; either
would have status read from, files staged from, and a fast-forward written
into, a directory you never pointed omarepos at. Such a directory gets a card
that says so, and nothing else: no fetch, no update, no working-tree view.
Linked worktrees, submodules and `--separate-git-dir` are told apart from that
and work as usual.

### What runs, and what does not

A pass across the fleet — the timer, the catch-up at launch, and
`Ctrl+Shift+R` — runs no git hooks, in any repository. A fast-forward would
otherwise run `post-merge`, and where hooks live in the tree (`core.hooksPath`,
husky) the hook that runs is the one the fast-forward has just written: code
nobody has read yet, in a repository nobody is looking at. The same pass also
switches off the fsmonitor command and the `ext::` transport, the other two
ways a repository's own config commonly names a command to run.

Switching those off is a list of the known ways, not a proof there are no
others: a repository's own config can name a command in more places than any
list covers. So a pass across the fleet also only enters repositories you have
seen. A directory that turns up under a scan root — an unpacked archive, a
synced folder — gets a card marked `new` and is fetched by nothing unattended
until you have opened it, or pressed `p` or `f` on it. The first run takes
everything already on the grid as seen. The list is kept in
`~/.local/state/omarepos/known-repos`.

The cost is that a `post-merge` hook that reinstalls dependencies, or git-lfs's,
does not fire on a background update. Press `p` on a repository to update it
with your whole setup in force, hooks included, exactly as your own `git pull`
would.

### Why it shells out to `git`

Authentication lives in credential helpers, SSH agents and `insteadOf` rewrites,
none of which libgit2 uses, and an identity can hang off a kind of `includeIf`
it does not follow (it reads `gitdir:` rules, not `hasconfig:` or `onbranch:`
ones). Driving the same `git` you drive means your setup applies unchanged,
including per-directory identities. A
repository you cannot fetch by hand is one omarepos does not pretend it can
fetch either; it reports the failure and moves on.

Prompts are disabled (`GIT_TERMINAL_PROMPT=0`, and SSH in batch mode), so a
repository whose credentials have expired fails quickly instead of parking a
worker on a prompt nobody can answer. The batch options are added to the ssh
command you have configured, not put in its place: a `core.sshCommand` that
picks a key per client, set globally or through `includeIf`, is still what
runs. A repository's *own* `core.sshCommand` is believed when you act on that
repository, and not on a pass across the fleet. On such a pass askpass is off
as well (`GIT_ASKPASS`, `SSH_ASKPASS`, `core.askPass`), so the timer cannot
open a password dialog on a desktop nobody is at; `p`, `f` and a push can.

`timeout_seconds` is how long a fetch may go without anything arriving, not how
long it may take: a large fetch over a slow link finishes, where it used to be
cut off at the limit on every pass and leave a partial pack behind each time.
There is a ceiling of an hour. A repository and its linked worktrees share one
store of refs, and are fetched one at a time.

Closing the window, or `Ctrl+Q`, does not cut git off. The window goes at once
and the application stays, without one, until a fast-forward, a commit or a
push that is under way has finished; a fetch is stopped, and a pass across the
fleet called off, since neither loses anything by it. Start omarepos again in
the meantime and you get a window from the same process.

The fast-forward does not share that timeout. It is a local write to your
working tree, and a checkout stopped half-way is worse than one waited for, so
it gets half an hour. When any git command is given up on, git is asked to stop
before it is made to — a `SIGTERM` to its whole process group, hooks and filters
included — because git removes its lock files when asked and leaves an
`index.lock` behind when killed.

### Worth knowing

- It does not pause on battery or a metered connection.
- A fetch moves `origin/*`, here as in a terminal. A bare
  `git push --force-with-lease` trusts those refs to say what you last saw, so
  a scheduled fetch weakens it to a plain force. `git config --global
  push.useForceIfIncludes true` restores the check, and is worth having with
  any tool that fetches for you.
- A fast-forward sets `ORIG_HEAD`, and so does an attempt git refuses.
  `FETCH_HEAD` is left alone: fetches here do not write it.
- git 2.29 or later is needed, for `--no-write-fetch-head`.
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
commits or 500 matches, and the newest 500 results are listed. The status bar
says when either limit was reached: "at least N commits", how many repositories
stopped at the match limit, and how many were read only 50,000 commits back. An
empty page does not claim that nothing matches in a repository it did not
finish reading.

## Commit graph

Click any card for its history. Lanes are assigned the usual way: a commit takes
the lane that was waiting for it, hands that lane to its first parent, and parks
any further parents in lanes of their own, so merges fan out and converge where
they actually did.

- Filled dot: ordinary commit. Ring: merge commit. Outer ring: something points
  at it.
- Pills are refs — the current branch, other local branches, tags, remote branches.
- A click selects a row, which is what fills the pane below. `Enter`, or a
  double click, copies its full sha.
- A row shows at most three ref pills, each shortened to fit; the rest fold
  into `+N`, and the tooltips have the names in full.
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
**Publish** instead: one with no upstream configured and no branch of its name
on the remote, and only when the repository has exactly one remote — guessing
which remote to publish to is not a guess worth making.

Never forced, never `--all`, never tags, never another branch, and never from
the grid across a selection. That is not left to git's defaults: the push names
its one source and one destination in full
(`git push --no-follow-tags -- <remote> refs/heads/<branch>:<upstream ref>`), so
`push.default`, `push.followTags` and a `remote.<name>.push` refspec have no say
in what is sent.

The button is drawn from a page that may have been open a while, with a
terminal one key away. So the repository is asked again at the moment of the
click, and nothing is sent unless it still gives the same answer: HEAD on the
same branch, the branch at the same commit, pushing to the same place. Otherwise
the page says what changed and reads itself again. Coming back to the window
does the same check, so a branch switched in lazygit shows up before the button
is reached.

It is refused, with the reason, when the branch is behind (fast-forward first),
has nothing ahead, has no upstream and no single remote, or HEAD is detached.
Four setups it will not push for at all, and says so: a branch that tracks a
branch of another name (what `git switch -c fix origin/main` leaves you with —
git's own default refuses that push too, and for the same reason: it would put
`fix` on `main`), a branch that tracks one remote but is set to push to another
(`remote.pushDefault`, `pushRemote`), a remote configured as a mirror, and a
branch that tracks a local branch. Those want a terminal. A `pre-push` hook's refusal is quoted back as git wrote it.

"No upstream" is not taken to mean "never published". A branch whose upstream
is configured and gone (deleted on the remote when its pull request merged,
or never fetched by a single-branch clone) is not offered **Publish**: a push
would create the branch again, and whether to is not a button. Neither is a
branch with no upstream whose name the remote already has.

And commits that were on the remote and have been taken off it are not pushed
back. When the remote's branch is wound back or rewritten, the commits you
still have count as "ahead" like any others; the button used to read "Push 2".
It now says what happened, names the commit the remote used to end at, and
leaves putting them back to a terminal.

### What is uncommitted

A **Local changes** row sits above the history whenever the working tree has
anything in it, drawn dashed and hollow because it is not a commit. Selecting it
fills the same pane with everything uncommitted, grouped:

| Group | Holds back the background refresh |
| --- | --- |
| Staged, Not staged, Conflicted | yes |
| **Untracked** | **no** |

"Holds back" is about the pass across the fleet, which will not update under
uncommitted work. `p` on the repository asks git instead, and git manages more
than that: it fast-forwards over changes the incoming commits do not touch and
keeps them, and refuses — naming the files, changing nothing — where they
overlap.

Each group folds. Untracked starts folded past ten files, where it stops being
information and starts being noise.

The `+` beside a file stages it, `−` unstages it, and the box below commits what
is staged: `c` puts the keyboard in the message, `Enter` commits, `Esc` steps
back out and leaves the message. After a stage the keyboard is on the next
file of the same group, so a run of files is `Space`, `Space`, `Space`. What a
write came to, a hook's refusal included, stays on a line of its own above the
message until the next one. The list is read again when the window is
returned to, and keeps your place: the selected file, the keyboard, which
groups are folded and how far it was scrolled. A file that is staged and then edited again has two rows, and each
shows its own half: the Staged row is exactly what the commit will take. The
name beside the Commit button is who git will record it as. A submodule whose
checkout has moved is not listed, and neither it nor a repository nested inside
this one can be staged from here. Whole files only — staging a hunk is the heart of a committing tool
and omarepos is not one; `Open in terminal` starts lazygit for that. The commit
is refused, with the reason, when nothing is staged, the message is empty, the
repository is mid-merge or mid-rebase, there are unresolved conflicts, HEAD is
detached, or the branch is behind its upstream.

That last one is deliberate. A commit on a branch that is behind leaves it
diverged, and the ways out of that are a rebase or a merge, neither of which
omarepos does — so it will not start what it cannot finish. Press `p` first:
the update goes through with your changes in place unless it touches the same
files.

A conflicted file has no `+`. Staging is how git is told a conflict has been
resolved, so the button would settle a merge by accident, markers and all.
Conflicts want a terminal.

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

The list is libgit2's reading of the working tree, and there are two things
libgit2 does not do that git does. In a sparse checkout it reports every file
outside the cone as deleted, and it does not run clean or smudge filters, so a
file with a `filter` or `working-tree-encoding` attribute can show as modified
when git says it is not. Where either applies, a line above the list says how
many rows that may be, and to ask `git status`. And in a partial clone, a
commit whose blobs were never fetched has rows marked `unreadable` and a diff
that says why, where it used to say `+0 −0` and "No textual changes".

The working tree changes outside the app, so it is re-read whenever the window
regains focus — edit in your editor, come back, and the list is current. HEAD
is looked at in the same glance: when the branch or its tip has moved, because
of a commit made in the terminal `l` opened or a checkout, the whole page is
read again. When the tree went clean while "Local changes" was the row
selected, the selection steps to the newest commit.

The grid is kept honest the same way. A stage, a commit or a push made on a
page marks that repository's card to be read again when the grid is next in
front, and so is every repository handed to a terminal or an editor, each time
the window is returned to. One card is re-read, not the fleet. And a graph page
that is open while the window's own refresh fetches or fast-forwards its
repository reads itself again, so its labels and its push button are not left
describing the state before the pass.

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

- What is counted is each repository's checked-out branch and the branch it
  tracks, as `git shortlog HEAD @{u}` would. Other branches are left out: with
  every branch counted, a change that was squashed, rebased or amended was
  counted once for each copy of it. A commit held by several checkouts of the
  same repository counts once, and two repositories with the same folder name
  stay two.
- A commit belongs to the day its author wrote it, not the day it was rebased
  or merged. One dated later than today is left out, and the page says how many.
- Authors are keyed by e-mail, so one person with two spellings of their name
  still lands in one row, and a mailmap is honoured (`.mailmap`,
  `mailmap.blob`, `mailmap.file`).
- Line counts detect renames, as `git log --shortstat` does: a moved file is a
  move, not its whole length removed and added. A commit whose lines cannot be
  read (the edge of a shallow clone, a partial clone) shows `?`, not zero.
- A repository that could not be read is listed with the reason, and is not
  counted as one with no activity.
- Merge commits count as commits but not as lines — their diffs are their parents'
  work, already counted. The per-author tooltip breaks out how many were merges.
- Per-day strips are scaled against the busiest author-day, so rows are comparable.
- A row expands to the work itself: every commit, grouped by repository, with
  its sha, churn and age. `Enter` or a click on one opens that repository's
  graph on that commit. The newest 100 per repository are kept; past that the
  group says how many older ones there were.

Line counts come from diffing each commit against its first parent, which is the
expensive part. Only the paths a commit changed are diffed, so the cost follows
the size of the change and not of the repository; a week across a dozen repos
takes well under a second, and a 30-day window over a large fleet is the case
that can still be felt.

## Configuration

Optional, at `~/.config/omarepos/config.toml`:

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
  "~/code",
  { path = "~/work/acme", nested = true, tag = "acme", include_self = false },
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

## How it reads your repos

Everything shown comes from libgit2 against the on-disk repository. Reading
never runs `git`, never writes to a repository and never talks to a remote.
Where libgit2 cannot read what git can (an index written with
`core.splitIndex`, a sparse index, a damaged one) the card says `status
unreadable`, in red, and not `clean`; a history it cannot walk is said the same
way. Neither is taken for a repository with nothing in it.

Writing is narrow and deliberate. omarepos can stage a file, unstage a file,
commit what is staged, push the branch, and fast-forward a clean branch. It
cannot discard an edit: there is no checkout, no reset, no clean, no amend.
Everything it does is recoverable.

Pushing happens on a button press and never on the timer. Fetching is inward —
a mistake costs you your own time, with the one exception about
`--force-with-lease` under *Worth knowing*. Publishing is outward, cannot be taken back
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
| `src/gitcmd.rs` | The one way a git command is run: time limit, process group, what git said |
| `src/commit.rs` | Reading a commit or the working tree, and the stage, commit and push writes |
| `src/graph.rs` | Revwalk → lane assignment for the commit graph |
| `src/meta.rs` | Tag store (`repos.toml`) |
| `src/stats.rs` | Contribution aggregation over a day window |
| `src/search.rs` | Commit search across many repositories |
| `src/scan.rs` | Worker threads streaming results to the main loop |
| `src/refresh.rs` | Background fetch and fast-forward, with the safety rules |
| `src/lang.rs` | Extension → language + colour table |
| `src/testkit.rs` | Test fixtures: scratch repositories with a bare remote, an isolated home |
| `src/ui/window.rs` | Header, filter, sort, grid, selection, status bar, theme watcher |
| `src/ui/card.rs` | One repository card |
| `src/ui/graph.rs` | Commit graph page and its cairo row renderer |
| `src/ui/commit.rs` | The pane under the graph: changed files, diff, stage and commit |
| `src/ui/shortcuts.rs` | The keyboard, written down |
| `src/ui/debug.rs` | Debug builds only: what `scripts/walk` drives the window through |
| `scripts/walk/` | The walk: fixtures, the driver, the script that uses everything once, and the one that checks what needs frames |
| `src/ui/stats.rs` | Contributions page |
| `src/ui/search.rs` | Commit search page |
| `src/ui/dialogs.rs` | Settings dialog and the tag editor |

## Status

Early, and in daily use by one person. The grid, the commit graph with its
diff pane, contributions, commit search, the background refresh and the
stage-commit-push path are all there. Ahead and behind are as fresh as the last
fetch: with the refresh off, that is the last time you pressed `f`, `p` or
`Ctrl+Shift+R`.
