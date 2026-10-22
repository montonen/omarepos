# Usage

```
omarepos [options]

  -r, --root <PATH>   Directory to scan (repeatable; overrides the config file)
  -d, --depth <N>     How deep below each root to look for repos
      --json          Print the repos as JSON and exit, without a window
  -h, --help          Show help
  -V, --version       Show the version
```

`--json` is for scripts: see [The fleet as JSON](#the-fleet-as-json) below.

The filter has the keyboard when the window opens, so a repository is its
name and `Enter` away: type, and the grid narrows; `Enter` opens the top hit.
The launcher key does the same to a window that was already open, with what
was left in the filter selected, so that what you type replaces it. (Only with
the grid in front. A commit graph you left open keeps the keyboard.) `Down`
moves from the filter into the cards.

| Key | Action |
| --- | --- |
| `Ctrl+R` / `F5` | Rescan (local only). In a commit graph: read that repository again |
| `Ctrl+Shift+R` | Run the refresh now: fetch every repository in scope |
| `Ctrl+L` | Jump to the grid |
| `Ctrl+F`, or `/` with the keyboard on a card | Focus the filter box |
| `Ctrl+Shift+F` | Search commits across these repos; on Contributions its repos, in a commit graph that one |
| `Enter` in the filter | Open the commit graph of the top hit |
| `Down` in the filter | Move into the cards |
| `Ctrl+A` | Select every repo shown; again to deselect |
| `Esc` | Back out: a dialog or menu, the filter you are typing in, then the page, then the selection, then the filter |
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
| `f` | Fetch that repo. Its branches and files are left alone |
| `p` | Fetch it, then fast-forward it if it is clean and only behind |
| `Tab` in a commit graph | Into the changed files |
| `Ctrl+D` in a commit graph | Show or hide the pane below it |

`l`, `e`, `o` and `f` act once for each press. A key held down, or one that
sticks, does not open a terminal per repeat or start a second fetch.

The filter matches repo name, path, branch and remote. Every word typed has to
be found, in any order, so `acme billing` finds the billing repository under
`acme`. `tag:work` and `lang:rust` narrow by tag and by language. The warning
toggle in the header narrows the grid to repos that are dirty, ahead, behind,
unreadable, holding commits that are on no remote, or that the last fetch
failed on.

## The fleet as JSON

`omarepos --json` prints what the grid would show as JSON on standard output,
and exits. It opens no window and needs no display, so it can be run over ssh,
from cron or by an agent. The repositories are found and read by the same scan
as the grid's, under the same roots and depth, and `--root` and `--depth` make
it a one-off look as they do for the window. It reads and does nothing else: it
runs no `git`, fetches nothing and writes no file ([What it changes](read-only.md)).

```bash
omarepos --json | jq -r '.repos[] | select(.needs_attention) | .path'
```

What is printed is one object, with `schema_version`, `roots` and `repos` in
it. `schema_version` is 1. It goes up when a key is taken away, renamed or
comes to hold something else, and not when one is added, so a script should
pass over keys it does not know.

Each of `roots` is a root in force, in the order it is given: its `path`, how
many repositories it `found`, and `problem`, which is what the status bar says
of it (`"does not exist"`, `"no repositories found"`) or `null`. A disk that is
not mounted shows up here, and not only as repositories missing from the list.

Each of `repos` is a repository, in the order of their paths, and every one has
every key:

| Key | What it holds |
| --- | --- |
| `name`, `path` | The folder's name, and its absolute path |
| `needs_attention` | Whether the warning toggle keeps it: dirty, ahead, behind, unreadable, holding commits that are on no remote, or the last fetch failed on it |
| `error` | Why the repository could not be read at all |
| `remote` | The remote as a card shows it, `host/owner/repository`: `origin`, or the first remote there is |
| `web` | The remote's page on the web, where its URL says where one is |
| `head` | The branch, or a short commit id when HEAD is detached |
| `detached`, `unborn`, `bare` | HEAD is detached; there are no commits yet, and `head` is the branch the first will be on; there is no working tree |
| `upstream` | The branch it tracks, as `origin/main` |
| `working_tree` | `staged`, `unstaged`, `untracked` and `conflicted`: files, each counted once. One in conflict is `conflicted`, and one staged and edited again since is `staged` |
| `working_tree_unreadable` | Why the index could not be read |
| `stashes` | How many stashes it holds |
| `ahead`, `behind` | Commits the upstream does not have, and commits on it that are not here yet, as of the last fetch. `0` with no upstream |
| `unpushed` | Commits on HEAD that are on no remote at all, counted up to 1000, which then means that many or more |
| `stray_branches` | Other local branches that hold commits on no remote |
| `history_unreadable` | Why the commits between here and the remotes could not be read |
| `fetch_failed` | What git said the last time a fetch of it failed, until one works |
| `last_commit` | The commit HEAD is on: `short_id`, `summary`, `author`, and `when` it was committed, in seconds since 1970 |
| `tags` | Its tags, as the window shows them |

A reading that could not be taken is `null`, never `0`, so that "could not
count" is never read as "nothing to do". Where a card says "unreadable",
everything read from the repository is `null` and `error` says why; its tags
and a failed fetch are omarepos's own, and still there. Where it says "status
unreadable", `working_tree` is `null`, and where it says "history unreadable",
`ahead`, `behind`, `unpushed` and `stray_branches` are. A bare repository has
no `working_tree`, and in it and in a repository with no commits yet,
`unpushed` and `stray_branches` are not counted.

The tags are the ones the window would show: those in `repos.toml`, and a
root's own tag on what it finds, which the window's next scan writes there.
`--json` works that out and writes nothing. A one-off look applies no root's
tag, as in the window.

What is never printed is the URL a remote is configured with, which can carry
a user name and a token. The remote is given tidied, as the card gives it, and
a URL in what a failed fetch said loses whoever it logs in as.

It exits with 0 when it has printed the snapshot, whatever the snapshot says.
If `config.toml` or `repos.toml` is there and cannot be read, it prints nothing
on standard output, says why on standard error and exits with 1. The window
carries on with the defaults under a banner, but a script has no banner to
read, and a snapshot of `~/Work` in place of the roots the file names, or of
every repository without its tags, would be wrong without saying so. Standard
output that cannot be written to is 1 as well.

---

[Back to the README](../README.md) · [All documentation](README.md)
