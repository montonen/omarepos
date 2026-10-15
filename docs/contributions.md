# Contributions

`Ctrl+G`, the header button, or the `⋮` menu of a single repo. Tick cards first to
scope it; with nothing ticked it covers whatever the grid is currently showing, so
the filter box doubles as the repo picker.

Default window is the last 7 days; the drop-down in the header has 14, 30 and
90 days, this year (from the first of January) and the last 12 months. Days are
local calendar days, bucketed at local midnight. Over 90 days the overview and
each author's strip draw a week to a column, and over this year or the last 12
months a month, so that a year is a dozen columns and not 365.

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
- Each author's strip is scaled against the busiest any author had a day (or a
  week, or a month) of, so rows are comparable.
- A row expands to the work itself: every commit, grouped by repository, with
  its sha, churn and age. `Enter` or a click on one opens that repository's
  graph on that commit. The newest 100 per repository are kept; past that the
  group says how many older ones there were.
- An author can be set aside with the button at the end of their row, which
  shows when the row is pointed at or reached with `Tab`. They move under the
  list, after a gap, and out of the figures and the days above it, so a
  screenshot taken above the gap is of everybody else (yourself, most
  likely). The status bar keeps everybody's figures and says how many are set
  aside. The same button on their row brings them back. Who is set aside is
  remembered, by address, in `~/.local/state/omarepos/set-aside`.

Line counts come from diffing each commit against its first parent, which is the
expensive part. Only the paths a commit changed are diffed, so the cost follows
the size of the change and not of the repository; a week across a dozen repos
takes well under a second, and a 30-day window over a large fleet is the case
that can still be felt.

## The race

`r` on Contributions, or the ▶ button in its header, plays the authors'
commits as a race: each author's running total is a bar, the bars grow day by
day and overtake one another, and the date counts up in the corner. It is of
the page as it stands when asked for: the same repositories, the same
branches, and without whoever is set aside, so that it can be recorded with
yourself left out. The repositories it covers are named under it.

It opens on the period Contributions is showing, and has the same six to
choose from. A race lasts 20 seconds at 1×, whatever the period,
and the button beside the progress bar makes it 2× or 4×. `Space` plays and
pauses, `←` `→` go a day back or on, and `Home` and `End` go to the start and
the end; the progress bar can be dragged to any day. With animations turned
off on the desktop it opens at the end and moves only when told to.

It counts commits, not lines: a commit's lines are what reading it costs, and
a year across a fleet is read in seconds without them.

---

[Back to the README](../README.md) · [All documentation](README.md)
