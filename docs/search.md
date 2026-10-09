# Searching commits

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

---

[Back to the README](../README.md) · [All documentation](README.md)
