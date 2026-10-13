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

## What is uncommitted

A **Local changes** row sits above the history whenever the working tree has
anything in it, drawn dashed and hollow because it is not a commit. Selecting it
fills the same pane with everything uncommitted, in four groups: Staged, Not
staged, Conflicted and Untracked.

It is a view. Nothing in it stages, commits or resolves anything: for that,
`l` opens a terminal in the repository, in lazygit when it is installed, and
the list is current again when you come back.

Each group folds. Untracked starts folded past ten files, where it stops being
information and starts being noise. Untracked files are shown stepped back and
labelled, apart from the changes to files git already knows. Untracked
directories are recursed into, since "two untracked things" is what the card
already says and this view exists to say which.

A file that is staged and then edited again has two rows, and each shows its
own half: the Staged row is exactly what a commit would take. A submodule whose
checkout has moved is not listed.

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
regains focus — edit in your editor, come back, and the list is current, with
your place kept: the selected file, the keyboard, which groups are folded and
how far it was scrolled. HEAD is looked at in the same glance: when the branch
or its tip has moved, because of a commit made in the terminal `l` opened or a
checkout, the whole page is read again. When the tree went clean while "Local
changes" was the row selected, the selection steps to the newest commit.

The grid is kept honest the same way. Every repository handed to a terminal or
an editor is marked to be read again, each time the window is returned to. One
card is re-read, not the fleet. And a graph page that is open while the
window's own refresh fetches its repository reads itself again, so its labels
are not left describing the state before the pass.

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
