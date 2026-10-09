# Background refresh

Off by default. Turn it on in settings, or in the config file:

```toml
[refresh]
every_minutes = 15     # 0 turns it off
fetch = true           # run `git fetch`; touches the network, not your files
fast_forward = true    # update the code where that is unambiguously safe
only_tags = []          # e.g. ["work"]; blank means every repository
max_parallel = 4
timeout_seconds = 60
```

With `fetch` off it only re-reads local state. With `fetch` on, ahead/behind
stops going stale. With `fast_forward` on it also moves the checked-out branch,
and fetches to do it whatever `fetch` says.

It is a timer in the window, not a service: nothing is fetched while omarepos
is closed. A pass that fell due in the meantime runs at the next launch, once
the first scan has finished. Opened and closed like a launcher, that makes it a
fetch at launch more than a fetch every fifteen minutes.

`Ctrl+Shift+R` runs a pass immediately, whatever the schedule says. The pass
never pushes. Changing a refresh setting, or the scan roots, calls off a pass
that is under way: repositories it had not reached are left alone, and one
that was being fetched is not then fast-forwarded.

The status bar says what the timer is doing, and what it is not. `fetched 5
minutes ago` is the last pass that reached a remote: a pass run offline does
not count as one, so the next launch tries again. With `only_tags` set the
stamp names the tags it covered, and when nothing carries any of them it reads
`auto fetch idle: nothing is tagged work` in place of a schedule that would
fetch nothing. `2 not fetched` and `1 not updated` count the cards that say
`fetch failed` and `held back`.

A remote that turned a fetch away (a login refused, a repository that is not
there any more) is not asked again by the timer. Trying every few minutes with
the same credential changes nothing, and is how an account gets locked. The
card keeps its `fetch failed` flag; `f`, `p` or `Ctrl+Shift+R` try again, and a
fetch that works clears it. A remote that could not be reached is different,
and is tried on every pass.

When a remote says a repository is not found, the card and the toast say
`repository not found, or the account git signs in with has no access to it`,
with git's own line under it. Both are possible and git names only the first:
a host with private repositories answers one the account may not see exactly
as it answers one that is not there. If the repository exists, the address is
probably right (git prints it with a slash on the end either way) and the
thing to check is which account or key git uses for that remote.

## What it will and will not do

The only write it ever performs is a fast-forward of the checked-out branch. A
repository is left alone, with the reason reported, when any of these hold:

| Condition | Why |
| --- | --- |
| Uncommitted changes | Updating under your edits is not the app's call — on a pass across the fleet. `p` on that repository asks git, which keeps your changes where the update does not touch the same files and refuses where it does |
| Commits of its own | A fast-forward would not be one |
| Detached HEAD | No branch to move |
| No upstream | Nothing to move it to |
| Merge or rebase in progress | You are in the middle of something |
| Bare, or no commits yet | Nothing to do |
| The update would move a checked-out submodule | The submodule's own checkout would be left behind it. Pull that one yourself |
| The branch was moved back by hand | You undid an update (`git reset --hard ORIG_HEAD`); a pass across the fleet does not put it back. `p` does |
| The upstream was wound back | Commits you have were taken off the remote. That is not "ahead", and nothing here will push them back |

These are about *updating*. A pass that only fetches (`f`, or the refresh with
`fast_forward` off) fetches a detached, mid-merge or diverged repository like
any other, and reports what it found: `fetched, 3 behind`.

Cleanliness is checked after the fetch, immediately before the fast-forward,
because the fetch takes time and you may have started editing during it. The
update then runs `git merge --ff-only` with the settings that could make it
something else overridden for that one command (`branch.<name>.mergeoptions`,
`pull.twohead`, squash, autostash), and afterwards the branch is read back:
only "now at its upstream" counts as updated. It never merges, rebases,
stashes, resets or forces, never goes into submodules, and only moves the
branch you have checked out.

A fast-forward that fails part-way (a full disk, a file held open) leaves some
files already rewritten under a branch that has not moved. The card says "held
back", and its tooltip names those files and how to put them right, and goes
on saying it on later passes until it is.

Untracked files do not block an update: a fast-forward cannot silently
overwrite one, because git refuses rather than clobbering it. Ignored files are
another matter — left to itself, git *does* overwrite an ignored file that is
in the way of an incoming one, and says nothing. The fast-forward therefore
runs with `--no-overwrite-ignore`, and git refuses for those too. When a file
of yours is in the way the repository is fetched and left where it was; its
card says "held back", and the tooltip quotes git naming the file.

A repository is only touched if it is really the repository *of* the directory
it was found in. A `.git` can claim that its files live somewhere else
(`core.worktree`), or be a file naming another repository's git data; either
would have status read from, files staged from, and a fast-forward written
into, a directory you never pointed omarepos at. Such a directory gets a card
that says so, and nothing else: no fetch, no update, no working-tree view.
Linked worktrees, submodules and `--separate-git-dir` are told apart from that
and work as usual.

## What runs, and what does not

A pass across the fleet — the timer, the catch-up at launch, and
`Ctrl+Shift+R` — runs no git hooks, in any repository. A fast-forward would
otherwise run `post-merge`, and where hooks live in the tree (`core.hooksPath`,
husky) the hook that runs is the one the fast-forward has just written: code
nobody has read yet, in a repository nobody is looking at. The same pass also
switches off the fsmonitor command and the `ext::` transport, the other two
ways a repository's own config commonly names a command to run.

Switching those off is a list of the known ways, not a proof there are no
others: a repository's own config can name a command in more places than any
list covers. So a pass across the fleet also only enters repositories you have
seen. A directory that turns up under a scan root — an unpacked archive, a
synced folder — gets a card marked `new` and is fetched by nothing unattended
until you have opened it, or pressed `p` or `f` on it. The first run takes
everything already on the grid as seen. The list is kept in
`~/.local/state/omarepos/known-repos`.

The cost is that a `post-merge` hook that reinstalls dependencies, or git-lfs's,
does not fire on a background update. Press `p` on a repository to update it
with your whole setup in force, hooks included, exactly as your own `git pull`
would.

## Why it shells out to `git`

Authentication lives in credential helpers, SSH agents and `insteadOf` rewrites,
none of which libgit2 uses, and an identity can hang off a kind of `includeIf`
it does not follow (it reads `gitdir:` rules, not `hasconfig:` or `onbranch:`
ones). Driving the same `git` you drive means your setup applies unchanged,
including per-directory identities. A
repository you cannot fetch by hand is one omarepos does not pretend it can
fetch either; it reports the failure and moves on.

Prompts are disabled (`GIT_TERMINAL_PROMPT=0`, and SSH in batch mode), so a
repository whose credentials have expired fails quickly instead of parking a
worker on a prompt nobody can answer. The batch options are added to the ssh
command you have configured, not put in its place: a `core.sshCommand` that
picks a key per client, set globally or through `includeIf`, is still what
runs. A repository's *own* `core.sshCommand` is believed when you act on that
repository, and not on a pass across the fleet. On such a pass askpass is off
as well (`GIT_ASKPASS`, `SSH_ASKPASS`, `core.askPass`), so the timer cannot
open a password dialog on a desktop nobody is at; `p`, `f` and a push can.

`timeout_seconds` is how long a fetch may go without anything arriving, not how
long it may take: a large fetch over a slow link finishes, where it used to be
cut off at the limit on every pass and leave a partial pack behind each time.
There is a ceiling of an hour. A repository and its linked worktrees share one
store of refs, and are fetched one at a time.

Closing the window, or `Ctrl+Q`, does not cut git off. The window goes at once
and the application stays, without one, until a fast-forward, a commit or a
push that is under way has finished; a fetch is stopped, and a pass across the
fleet called off, since neither loses anything by it. Start omarepos again in
the meantime and you get a window from the same process.

The fast-forward does not share that timeout. It is a local write to your
working tree, and a checkout stopped half-way is worse than one waited for, so
it gets half an hour. When any git command is given up on, git is asked to stop
before it is made to — a `SIGTERM` to its whole process group, hooks and filters
included — because git removes its lock files when asked and leaves an
`index.lock` behind when killed.

## Worth knowing

- It does not pause on battery or a metered connection.
- A fetch moves `origin/*`, here as in a terminal. A bare
  `git push --force-with-lease` trusts those refs to say what you last saw, so
  a scheduled fetch weakens it to a plain force. `git config --global
  push.useForceIfIncludes true` restores the check, and is worth having with
  any tool that fetches for you.
- A fast-forward sets `ORIG_HEAD`, and so does an attempt git refuses.
  `FETCH_HEAD` is left alone: fetches here do not write it.
- git 2.29 or later is needed, for `--no-write-fetch-head`.
- Fetching on a schedule is still traffic to your remotes, and on a work
  machine that traffic is visible to whoever runs them.
- Scope is the whole grid, or the tags in `only_tags` — deliberately not what
  the filter box is showing, so what you are looking at never changes what gets
  fetched.

---

[Back to the README](../README.md) · [All documentation](README.md)
