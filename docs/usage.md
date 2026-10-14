# Usage

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

---

[Back to the README](../README.md) · [All documentation](README.md)
