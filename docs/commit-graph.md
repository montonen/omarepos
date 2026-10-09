# Commit graph

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

## Pushing

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

## What is uncommitted

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

## What a commit changed

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

---

[Back to the README](../README.md) · [All documentation](README.md)
