# Reading a card

Every fact has a place of its own on a card, the same on every card, and the
cards are all one height: where a thing is says what it is. Top to bottom:

| Row | Left | Right |
| --- | --- | --- |
| Name | The repository | The **flag**, when there is trouble |
| Where | The folder it lies in, and the remote it belongs to | |
| Branch | The branch (or the commit a detached HEAD is on), and `+2 branches` when other local branches hold commits that are on no remote | What is uncommitted: `+1 ~3 ?4` is 1 staged, 3 modified, 4 untracked; `!2` is 2 in conflict; `clean`; stashes |
| Sync | `↑` what exists only here: `2 ahead`, `3 unpublished`, `no remote` | `↓` what is waiting: `12 behind` |
| Commit | The last commit: how old, what it says, and who wrote it if that was not you. `↑` in front when it is on no remote | |
| Lately | Commits a day over the last four weeks, today at the right, and under them `14 this week · 2 people` or `quiet for 3 weeks` | `remote tip 3h ago`, when behind |
| Tags | The repository's tags, each with the dot of its own colour; `new` on one found since you last looked | Keys, on the card with the keyboard |

The folder and the remote are written without the repository's name, which is
on the line above: `~/clients/acme/ · github.com/acme/`. A remote called
something else is written whole, since the difference is then the thing to
see. The full path and the address are in the tooltip, with the mix of
languages (`lang:` in the filter still finds by it).

**Colour means something to do.** A card for a repository that is clean and
level with its remote has none. Yellow is uncommitted work, the accent is
commits that exist nowhere else, red is trouble. The left edge of the card
takes the first of those that applies, and says where your work is: red when
the repository itself is in trouble, yellow when there is uncommitted work,
the accent when there are commits only here. The flag beside the name is the
one solid patch of colour on a card, and goes to one trouble at a time: a
repository that cannot be read, files in conflict, a working tree whose state
is not known, or a fetch that failed. A failed fetch never takes the edge, so
a day without a network does not hide which cards have work in them. The one
colour on a card that is not about its state is the dot on a tag: a tag has
one colour wherever it is shown, the same on a card as on its chip in the
header.

**The four weeks.** One column a day, weeks set apart, on one scale for every
card (a column is full at six commits), so a tall column means the same
everywhere. Your commits are drawn solid from the baseline and other people's
fainter on top of them; "you" is whoever that repository is configured to
commit as. Days are 24-hour windows back from the scan, not calendar days. The
count takes in the branch's upstream as well as HEAD, so a checkout that is
behind does not look asleep. It reads at most 200 commits per repository; where
that does not reach back four weeks, the baseline stops where the reading did,
so that "not read" is not drawn as "nothing happened". It knows only what has
been fetched.

**Your own name is not printed.** A name on a commit means somebody else. So
when your own name does turn up, that repository is set up to commit as
someone you are not, and you see it on the grid before you see it in a push.
Where who you are is in doubt (the environment or `author.*` outranks
`user.email`), nobody is "you": everyone is named and the days are drawn in one
tone.

**The card with the keyboard** shows the two commits before the last in place
of the four weeks, or, when it is flagged, the reason in git's own words; and
the keys that act on it. The card `Enter` would open from the filter
shows the same, and says `Enter`. Nothing changes size as the keyboard moves.

**The card under the pointer** shows the same two commits, or the reason, once
you have moved the pointer or turned the wheel. A pointer that was left over
the window when it opened changes nothing, and neither does one lying where
it was while you type or use the arrow keys: the card under it shows its four
weeks until the pointer is touched again. It shows no keys. Those act on the
card the keyboard is on, wherever the pointer is.

"Ahead" is measured against the branch's upstream, and says nothing when there
is none. So a card also says when HEAD has commits that exist on no remote at
all — `3 unpublished` for a branch that was never pushed or whose upstream is
gone, `no remote` for a repository that has nowhere to push — and `+2 branches`
when other local branches hold such commits. Those are the cases that used to
read "clean". Each card's checkbox adds it to the contributions scope — once
anything is ticked the rest of the grid dims, so the selection stays visible
while you scroll.

A card faded to half is the last run's: at launch the grid is put up at once
from what the last scan read, and each card stays faded until the scan has
read its repository again, a second or two later. The status bar says how
many are still waiting and how old their reading is (`12 as read 3 hours
ago`). Nothing is fetched until the scan is done, and a repository the scan
no longer finds leaves the grid when it ends. The `⋮` menu holds the terminal, editor, web page and clipboard actions. **Open in terminal**
starts lazygit in the repository when it is installed, and leaves you in a shell
there when you quit it — so it is both, not one instead of the other. Change or
disable that in settings.

**Open on GitHub** — in the same menu, on `o`, and on the globe in a commit
graph's header — sends your browser to the repository's page. The address is
worked out from the remote the card names (`origin`, or the first one there
is): `git@github.com:acme/widgets.git` and `https://github.com/acme/widgets.git`
both become `https://github.com/acme/widgets`. A user name or token written
into the remote is left out, and so is an ssh port; an `http://` remote keeps
its port, being a web address already. The tooltip shows exactly what will be
opened, and the menu names the site: GitHub, GitLab, Bitbucket, Codeberg and
Azure DevOps by name, anything else by its host.

It is not offered where no page can be worked out: a repository with no remote,
a remote that is a directory, and an ssh remote whose host has no dot in it —
that is a name from `~/.ssh/config`, and only ssh knows which host it stands
for. Pressing `o` there says so. Elsewhere the address is the convention every
common forge keeps (the host, then the path), which a server of your own may
not follow.

---

[Back to the README](../README.md) · [All documentation](README.md)
