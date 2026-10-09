# Contributions

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
- **All branches**, in the drop-down beside the time window, is the other
  view: every branch a repository has, its own and the ones it has fetched, as
  `git shortlog HEAD --branches --remotes` would. It shows the work on a
  branch nobody has checked out, which the first view cannot, and it does
  count a rewritten change once for each copy of it: after a squash merge the
  branch's commits and the squash are both there until the branch is deleted
  and pruned. The page says so while it is showing. Read it as who is busy
  where, and take numbers from the first view. A page opens on the first view
  every time.
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

---

[Back to the README](../README.md) · [All documentation](README.md)
