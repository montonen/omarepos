# Read-only

Everything shown comes from libgit2 against the on-disk repository. Reading
never runs `git`, never writes to a repository and never talks to a remote.
Where libgit2 cannot read what git can (an index written with
`core.splitIndex`, a sparse index, a damaged one) the card says `status
unreadable`, in red, and not `clean`; a history it cannot walk is said the same
way. Neither is taken for a repository with nothing in it.

omarepos does one thing to a repository: it fetches. When you press `f`, when
you run the refresh with `Ctrl+Shift+R`, or on a timer if you switch that on
([Fetching](refresh.md)). A fetch records what the remote has. It moves no
branch of yours, changes no file in your working tree, and sends nothing.

That is all of it. It does not stage, commit, push, merge, fast-forward,
rebase, stash, check out or reset, so there is nothing in it that could
discard an edit or publish one. Earlier versions staged, committed, pushed and
fast-forwarded a clean branch. Those were taken out, so that what a program
pointed at every repository on a machine may do to them fits in a sentence.
For anything that changes a repository there is `l`, which opens a terminal in
it, in lazygit when that is installed, and `e` for your editor.

Fetching is inward: a mistake costs you your own time, with the one exception
about `--force-with-lease` under [*Worth knowing*](refresh.md#worth-knowing).
Ahead/behind counts compare your local branch to the *last fetched* state of its
upstream, exactly like `git status` does; they go stale until something fetches.
When a branch is behind, the card also gives the age of the upstream tip — the
local commit's age alone reads as "nothing has happened here", which can be the
opposite of the truth.

What omarepos keeps for itself is in files of its own, never inside a
repository: `config.toml` and `repos.toml` (your tags) under
`~/.config/omarepos`, and under `~/.local/state/omarepos` the time of the last
fetch, what a fetch could not do, and the list of repositories it has seen.

Discovery skips `node_modules`, `vendor`, `target` and dotted directories, and
the language mix is computed from the git index, so untracked build output never
counts.

---

[Back to the README](../README.md) · [All documentation](README.md)
