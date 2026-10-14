# Fetching

A fetch asks the remote what it has and records the answer, which is what
`origin/main` and its like are. No branch of yours moves, no file in your
working tree changes, and nothing is sent anywhere. Four things start one:

| What | Does |
| --- | --- |
| `f` on a card, or in a commit graph | Fetches that repository, now |
| `p` on a card | Fetches that repository, then fast-forwards it if it is clean and only behind |
| `Ctrl+Shift+R` | Fetches every repository in scope, now |
| The background refresh | Fetches every repository in scope, on a timer |

Bringing fetched commits into a branch that has work in progress, or commits
of its own, is yours to do with your own tools: `l` on the card opens a
terminal in the repository, in lazygit when it is installed.

## `p`: fetch, then fast-forward

`p` on a card is the one thing omarepos does that changes a working tree, and
it does it only for the repository under the keyboard, when you press it. The
timer, the catch-up at launch and `Ctrl+Shift+R` never do.

It fetches first, always. Then it fast-forwards the checked-out branch, and
only if all of this holds; otherwise it says which did not:

- nothing is staged, modified or in conflict (an untracked file is fine: git
  refuses rather than overwrite one);
- a branch is checked out, it tracks an upstream, and it is only behind it,
  with no commits of its own;
- no merge, rebase, cherry-pick or bisect is under way;
- the update does not move the pointer of a submodule that is checked out
  here, since `git merge` would leave the submodule's files behind.

The update itself is your own `git` running
`git -c merge.autoStash=false -c pull.twohead=recursive merge --ff-only
--no-squash --no-autostash --no-overwrite-ignore @{u}`, with the branch's own
`mergeoptions` set aside for the command. Each of those was found making a
plain `--ff-only` do something else and exit 0: a merge commit, a squash, a
stash, an ignored `.env` overwritten. A branch that insists on signed commits
still does. Afterwards the branch is read back, and anything but the
upstream's commit is reported, not trusted. Your hooks and filters run, as
they would for your own pull.

When an update stops part-way (a file it cannot write, a filter that fails),
git has already written some of the files and leaves the branch where it was.
omarepos says which files hold the incoming version, so they are not taken for
your work, and it goes on saying so, on the next fetch too, until they are put
back.

## The background refresh

Off by default. Turn it on in settings, or in the config file:

```toml
[refresh]
every_minutes = 15     # 0 turns it off
fetch = true           # run `git fetch`; touches the network, not your files
only_tags = []         # e.g. ["work"]; blank means every repository
max_parallel = 4
timeout_seconds = 60
```

With `fetch` off it only re-reads local state. With `fetch` on, ahead/behind
stops going stale. A file written by an earlier version may also say
`fast_forward`; it is read without complaint and does nothing.

It is a timer in the window, not a service: nothing is fetched while omarepos
is closed. A pass that fell due in the meantime runs at the next launch, once
the first scan has finished. Opened and closed like a launcher, that makes it a
fetch at launch more than a fetch every fifteen minutes.

`Ctrl+Shift+R` runs a pass immediately, whatever the schedule says. Changing a
refresh setting, or the scan roots, calls off a pass that is under way:
repositories it had not reached are left alone.

The status bar says what the timer is doing, and what it is not. `fetched 5
minutes ago` is the last pass that reached a remote: a pass run offline does
not count as one, so the next launch tries again. With `only_tags` set the
stamp names the tags it covered, and when nothing carries any of them it reads
`auto fetch idle: nothing is tagged work` in place of a schedule that would
fetch nothing. `2 not fetched` counts the cards that say `fetch failed`.

A remote that turned a fetch away (a login refused, a repository that is not
there any more) is not asked again by the timer. Trying every few minutes with
the same credential changes nothing, and is how an account gets locked. The
card keeps its `fetch failed` flag; `f` or `Ctrl+Shift+R` try again, and a
fetch that works clears it. A remote that could not be reached is different,
and is tried on every pass.

When a remote says a repository is not found, the card and the toast say
`repository not found, or the account git signs in with has no access to it`,
with git's own line under it. Both are possible and git names only the first:
a host with private repositories answers one the account may not see exactly
as it answers one that is not there. If the repository exists, the address is
probably right (git prints it with a slash on the end either way) and the
thing to check is which account or key git uses for that remote.

## What a fetch does, and does not

It runs `git fetch --progress --no-write-fetch-head --no-recurse-submodules`
in the repository, and reports what it found: `fetched, 3 behind`. A
repository with a detached HEAD, in the middle of a merge, or diverged from
its upstream is fetched like any other, since a fetch touches none of that.

What it writes is what your own `git fetch` writes, all of it inside `.git`:
the new commits, `origin/*` moved to match, and any tag the remote has on
those commits. A tag of yours by the same name stays where you put it. When
git judges it due it tidies its own object store afterwards, as it does after
a fetch of yours. HEAD, your branches, the index and every file in the working
tree are as they were, and the tests hold it to that.

It does not go into submodules. When a fetch brings in a commit that moves a
submodule's pointer, git would go on to fetch inside the submodule, from the
submodule's own remote: a second repository contacted and written to, and one
whose remote having moved away would fail the fetch of a repository that had
itself fetched fine.

A repository is only touched if it is really the repository *of* the directory
it was found in. A `.git` can claim that its files live somewhere else
(`core.worktree`), or be a file naming another repository's git data; either
would have status read from, and a fetch written into, a place you never
pointed omarepos at. Such a directory gets a card that says so, and nothing
else: no fetch, no working-tree view. Linked worktrees, submodules and
`--separate-git-dir` are told apart from that and work as usual.

## What runs, and what does not

A pass across the fleet — the timer, the catch-up at launch, and
`Ctrl+Shift+R` — runs no git hooks, in any repository, and switches off the
fsmonitor command and the `ext::` transport: the ways a repository's own
config commonly names a command to run. Nobody picked those repositories one
by one, and nobody is watching them.

Switching those off is a list of the known ways, not a proof there are no
others: a repository's own config can name a command in more places than any
list covers. So a pass across the fleet also only enters repositories you have
seen. A directory that turns up under a scan root — an unpacked archive, a
synced folder — gets a card marked `new` and is fetched by nothing unattended
until you have opened it, or pressed `f` on it. The first run takes everything
already on the grid as seen. The list is kept in
`~/.local/state/omarepos/known-repos`.

`f` on a repository is different: you pointed at it, so it is fetched with
your whole setup in force, exactly as your own `git fetch` would.

## Why it shells out to `git`

Authentication lives in credential helpers, SSH agents and `insteadOf` rewrites,
none of which libgit2 uses. Driving the same `git` you drive means your setup
applies unchanged. A repository you cannot fetch by hand is one omarepos does
not pretend it can fetch either; it reports the failure and moves on.

Prompts are disabled (`GIT_TERMINAL_PROMPT=0`, and SSH in batch mode), so a
repository whose credentials have expired fails quickly instead of parking a
worker on a prompt nobody can answer. The batch options are added to the ssh
command you have configured, not put in its place: a `core.sshCommand` that
picks a key per client, set globally or through `includeIf`, is still what
runs. A repository's *own* `core.sshCommand` is believed when you act on that
repository, and not on a pass across the fleet. On such a pass askpass is off
as well (`GIT_ASKPASS`, `SSH_ASKPASS`, `core.askPass`), so the timer cannot
open a password dialog on a desktop nobody is at; `f` can.

`timeout_seconds` is how long a fetch may go without anything arriving, not how
long it may take: a large fetch over a slow link finishes, where it used to be
cut off at the limit on every pass and leave a partial pack behind each time.
There is a ceiling of an hour. A repository and its linked worktrees share one
store of refs, and are fetched one at a time.

Closing the window, or `Ctrl+Q`, stops a fetch that is under way and calls off
a pass across the fleet. Neither loses anything by it. When a fetch is given
up on, git is asked to stop before it is made to — a `SIGTERM` to its whole
process group — because git removes its lock files when asked and leaves them
behind when killed.

## Worth knowing

- It does not pause on battery or a metered connection.
- A fetch moves `origin/*`, here as in a terminal. A bare
  `git push --force-with-lease` trusts those refs to say what you last saw, so
  a scheduled fetch weakens it to a plain force. `git config --global
  push.useForceIfIncludes true` restores the check, and is worth having with
  any tool that fetches for you.
- `FETCH_HEAD` is left alone: fetches here do not write it.
- Your git configuration applies to these fetches as it does to yours. What
  is fetched and where it is put is the repository's own `remote.*.fetch`,
  and `fetch.prune` or `fetch.pruneTags` remove what they would remove for
  you.
- git 2.29 or later is needed, for `--no-write-fetch-head`.
- Fetching on a schedule is still traffic to your remotes, and on a work
  machine that traffic is visible to whoever runs them.
- Scope is the whole grid, or the tags in `only_tags` — deliberately not what
  the filter box is showing, so what you are looking at never changes what gets
  fetched.

---

[Back to the README](../README.md) · [All documentation](README.md)
