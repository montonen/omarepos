# Reading and writing

Everything shown comes from libgit2 against the on-disk repository. Reading
never runs `git`, never writes to a repository and never talks to a remote.
(One thing is asked of `git` itself, and only once Local changes is open: who
a commit would be recorded as, `git var GIT_AUTHOR_IDENT`, for the name beside
the Commit button.)
Where libgit2 cannot read what git can (an index written with
`core.splitIndex`, a sparse index, a damaged one) the card says `status
unreadable`, in red, and not `clean`; a history it cannot walk is said the same
way. Neither is taken for a repository with nothing in it.

Writing is narrow and deliberate. omarepos can stage a file, unstage a file,
commit what is staged, push the branch, and fast-forward the checked-out
branch: on a pass across the fleet only where the working tree is clean, and
on `p` wherever git will (see
[Background refresh](refresh.md#what-it-will-and-will-not-do)). It cannot
discard an edit: there is no checkout, no reset, no clean, no amend.
Everything it does is recoverable.

Pushing happens on a button press and never on the timer. Fetching is inward —
a mistake costs you your own time, with the one exception about
`--force-with-lease` under [*Worth knowing*](refresh.md#worth-knowing). Publishing is outward, cannot be taken back
without a force push, and on a work remote is visible to everyone. A timer must
not decide that something is ready to be seen.
Ahead/behind counts compare your local branch to the *last fetched* state of its
upstream, exactly like `git status` does; they go stale until something fetches.
When a branch is behind, the card also gives the age of the upstream tip — the
local commit's age alone reads as "nothing has happened here", which can be the
opposite of the truth.

The one exception is the [background refresh](refresh.md), which is off until
you turn it on.

Discovery skips `node_modules`, `vendor`, `target` and dotted directories, and
the language mix is computed from the git index, so untracked build output never
counts.

---

[Back to the README](../README.md) · [All documentation](README.md)
