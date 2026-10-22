# Repositories and tags

## Adding repositories

With no configuration omarepos looks under `~/Work`, the folder Omarchy sets
up for work. A window with nothing to list says which folders it looked in and
what it found there, and offers **Choose folders…**, which opens Settings. The
keyboard is on that button, so `Enter` gets there; once there is something to
list, the keyboard is in the filter, as it is at launch.

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
   keeps the version it replaced beside it as `config.toml.bak`. `--json` does
   not run on the defaults: it prints nothing until the file is fixed
   ([Usage](usage.md#the-fleet-as-json)).
3. **`--root <PATH>`** on the command line, repeatable, for a one-off look at
   somewhere else. It overrides the configured roots for that run and is not
   saved. A one-off look opens its own window even when omarepos is already
   running, applies no root tags, and does no background refresh.

Otherwise there is one window: starting omarepos while it is already open
brings that window forward.

A root is searched `max_depth` levels down and stops descending at the first
repository it finds, so `~/Work` picks up `~/Work/foo` and `~/Work/plugins/bar`
without ever walking into their contents. `extra_repos` entries are included
whatever their depth, which is the escape hatch for a repo in an awkward place.

### Workspace repositories

Some folders are themselves a repository that holds nothing but other people's
clones — a `.gitignore` of `*`, a README, and thirty checkouts beside them.
Stopping at the first `.git` finds the wrapper and misses everything in it.

Mark such a root as a *workspace*, in settings or in the config file:

```toml
roots = [
  "~/Work",
  { path = "~/clients/acme", nested = true, tag = "acme" },
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

- A root can tag what it discovers — see [*Workspace repositories*](#workspace-repositories) above.
- Tag one repo from its `⋮` menu, or several at once: tick their checkboxes, then
  **Tags → Tag N selected…**. With nothing ticked, that button tags everything
  currently shown, so the filter box doubles as a bulk selector.
- Editing several at once only replaces the tags they have *in common*; tags
  unique to one of them survive.
- With up to five tags, each has a button of its own in the header — one click
  to filter instead of three, and in the colour its dot has on the cards. A
  click shows the repositories with that tag and no others, the next click on
  another tag shows that one instead, and a click on the tag already shown
  shows everything again. `Ctrl`+click adds a tag to the ones shown, or takes
  it away, for more than one at a time. With
  more than five the **Tags** menu lists them instead. The header gives the
  buttons up on a window too narrow for them (how narrow depends on how long
  the names are), and the menu lists the tags then as well. Buttons and counts
  are of the repositories on the grid, not of everything the file remembers.
- `Enter` in the tag editor saves.
- `repos.toml` can be edited by hand while the window is open, on the same
  terms as the config file: read again at the next rescan, and never saved over
  in between.
- The popover filters the grid. Several ticked tags is an OR. `tag:work`
  typed in the filter box is an AND, and stacks with the rest of the query —
  `tag:work ruby` means tagged work *and* matching "ruby".
- Tag colours are derived from the tag's own name, so a tag keeps its colour
  across repos and across runs.

---

[Back to the README](../README.md) · [All documentation](README.md)
