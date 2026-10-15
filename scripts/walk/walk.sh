#!/bin/bash
# Use every page and every action of omarepos once, and say whether the
# program was still running after each.
#
# A panic inside a GTK callback ends the program, and most of what a window
# does cannot be reached by a unit test. This starts a debug build on a hidden
# Hyprland workspace, on fixture repositories and a scratch configuration,
# and drives it over D-Bus: nothing goes through the compositor, nothing of
# yours is read or written, and nothing appears on screen. After every step
# the drawing code is run and the process is checked.
#
#     scripts/walk/walk.sh            the whole walk, about ten minutes
#     scripts/walk/walk.sh quick      one repository instead of eight
#
# Needs Hyprland (hyprctl), python3, gdbus and busctl. Exits non-zero when
# the program stopped anywhere, having printed where and the panic, or did
# not say what it was expected to.
HERE=$(dirname "$(readlink -f "$0")")
REPO=$(dirname "$(dirname "$HERE")")
export OMAREPOS_WALK_DIR=${OMAREPOS_WALK_DIR:-$REPO/target/walk}
W=$OMAREPOS_WALK_DIR
QUICK=$1

( cd "$REPO" && cargo build --offline 2>&1 | tail -1 ) || exit 2
"$HERE/fixtures.sh" || exit 2

X() { python3 "$HERE/drive.py" "$@" 2>&1 | grep -v "^$"; }
STEPS=0; DEATHS=0; WRONG=0
config() { printf 'roots = [\n    { path = "%s/fleet", tag = "JM" },\n]\nmax_depth = 3\nterminal = "/nonexistent/no-such-terminal"\n' "$W" > "$W/config/omarepos/config.toml"; }
boot() { config; X start >/dev/null; }
step() { # what it is, then the commands
  STEPS=$((STEPS+1)); local what="$1"; shift
  eval "$@" >/dev/null 2>&1
  sleep "${WAIT:-0.5}"
  python3 "$HERE/drive.py" paint >/dev/null 2>&1; sleep 0.15
  if ! python3 "$HERE/drive.py" alive 2>/dev/null | grep -q "^alive"; then
    DEATHS=$((DEATHS+1)); echo "STOPPED at: $what"
    grep -m2 -A1 "panicked at" "$W/app.log" | cut -c1-220
    cp "$W/app.log" "$W/stopped-$DEATHS.log"; boot
  fi
}
# How often the program has said it would open the one page there is among
# the fixtures, had it been allowed to leave the window; and whether the step
# just taken added as many to that as it should have.
asked() { grep -cF "would open https://git.example.invalid/acme/widgets" "$W/app.log"; }
asks() { [ "$(asked)" = $(( $1 + $2 )) ] || { WRONG=$((WRONG+1)); echo "WRONG: $3"; }; }
open_repo() { X keys Escape Escape Escape ctrl+f; X type "$1"; sleep 0.35; X keys Return; sleep 1.3; }
# A tag file giving the first six repositories these tags, in turn.
tags() { : > "$W/config/omarepos/repos.toml"; local i=0 n; n=$(echo "$1" | wc -w)
  for r in alpha beta gamma merges lots widgets; do i=$((i+1)); t=$(echo "$1" | cut -d' ' -f$(( (i-1) % n + 1 )))
    printf '[repos."%s/fleet/%s"]\ntags = ["%s"]\n\n' "$W" "$r" "$t" >> "$W/config/omarepos/repos.toml"; done; }

# ---- what must be the same afterwards
# omarepos reads, and it fetches, and `p` fast-forwards one clean branch. So
# whatever is pressed below, no fixture's branches, tags, index or files may
# be any different at the end, but for beta's, where `p` is pressed on a clean
# branch that is only behind: that one is checked on its own at the end. Taken
# with git told not to tidy the index as it looks, or the looking would be the
# change.
as_found() { local r; for r in "$W"/fleet/*/; do [ "$(basename "$r")" = beta ] && continue; ( cd "$r" || exit
  echo "== $(basename "$r")"
  git symbolic-ref -q HEAD || git rev-parse -q --verify HEAD || echo "no commit"
  git for-each-ref --format='%(refname) %(objectname)' refs/heads refs/tags
  GIT_OPTIONAL_LOCKS=0 git status --porcelain=v1 -uall 2>&1
  git stash list | wc -l
  [ -f .git/index ] && sha256sum < .git/index
  find . -path ./.git -prune -o -type f -print0 | sort -z | xargs -0 sha256sum 2>/dev/null | sha256sum
) 2>&1; done; }
export GIT_CONFIG_GLOBAL="$W/gitconfig" GIT_CONFIG_NOSYSTEM=1
as_found > "$W/as-found.txt"

boot
# ---- where the keyboard starts: in the filter, to be typed into at once
keyboard() { X dump "keyboard:" | grep -q "$1" || { WRONG=$((WRONG+1)); echo "WRONG: $2"; }; }
keyboard GtkText "the keyboard is not in the filter at launch"
step "type a name at once" "X type widgets; sleep 0.4"
step "Down into the cards" "X keys Down"
keyboard GtkFlowBoxChild "Down from the filter did not reach a card"
step "the launcher key, with the window open" "X act launched-again"
keyboard GtkText "a second launch did not put the keyboard in the filter"
step "type over what was there" "X type alpha; sleep 0.4"
X dump | grep -q 'SearchEntry "alpha"' || { WRONG=$((WRONG+1)); echo "WRONG: a second launch did not select what was in the filter"; }
step "clear it" "X keys Escape"
# ---- the pointer: lying over a card is not choosing one, being moved is, and
# the keyboard takes the choosing back. `point` is a pointer nobody has; see
# src/ui/debug.rs.
choosing() { X dump "choosing:" | grep -qx "choosing: $1" || { WRONG=$((WRONG+1)); echo "WRONG: $2 ($(X dump choosing:))"; }; }
choosing nothing "something is choosing a card before a key or the pointer has"
step "the pointer, lying over a card" "X point 1"
choosing nothing "a pointer that has not moved is choosing a card"
step "the pointer, moved to another card" "X point 2"
choosing pointer "a pointer that was moved is not choosing a card"
X dump "[pointer" | grep -q "om-card" || { WRONG=$((WRONG+1)); echo "WRONG: no card is under the pointer"; }
step "the pointer, seen again where it is" "X point 2"
choosing pointer "a pointer seen twice in one place stopped choosing"
step "a key on the grid" "X keys ctrl+l space space"
choosing keyboard "a key on the grid did not take the choosing from the pointer"
step "the wheel, turned" "X wheel"
choosing pointer "turning the wheel did not hand the choosing to the pointer"
step "the filter, typed in" "X keys ctrl+f; X type a; sleep 0.5"
choosing nothing "typing in the filter left the pointer choosing"
step "the pointer, moved again and taken away" "X point 0; X point off; X keys Escape"
# ---- the grid
for n in 0 1 2 3 4; do step "sort order $n" "X choose $n"; done
step "attention toggle on" 'X press "Show only repos needing attention"'
step "attention toggle off" 'X press "Show only repos needing attention"'
for q in merges "tag:JM" "lang:markdown" "lang:" "tag:" "zzz-nothing" "merges main" "  "; do
  step "filter '$q'" "X keys ctrl+f; X type '$q'; sleep 0.4; X keys Escape Escape"; done
step "select all" "X keys ctrl+a"
step "contributions for the selection" "X keys ctrl+g; sleep 1.5"
step "back" "X keys Escape"
step "clear selection" "X keys ctrl+shift+a"
step "tick with space" "X keys ctrl+l space"
step "untick with space" "X keys space"
step "shortcuts dialog" "X keys shift+question; sleep 0.3"
step "close it" "X keys Escape"
step "slash to filter" "X keys ctrl+l slash Escape"
step "l with no terminal" "X keys ctrl+l l"
step "e with no terminal" "X keys e"
N=$(asked); step "o where the remote is a directory" "X keys ctrl+f; X type alpha; sleep 0.4; X keys ctrl+l o; sleep 0.3; X keys Escape Escape"
asks $N 0 "o opened a page for a remote that is a directory"
N=$(asked); step "o where there is a page" "X keys ctrl+f; X type widgets; sleep 0.4; X keys ctrl+l o; sleep 0.3; X keys Escape Escape"
asks $N 1 "o on a card did not ask for its page"
N=$(asked); step "card menu: open on the web" 'X press "!Open on git.example.invalid"'
asks $N 1 "the card's menu did not ask for the page"
WAIT=2 step "f on a card" "X keys ctrl+l f"
step "p on a clean card that is behind" "X keys ctrl+f; X type beta; sleep 0.4; X keys Down p; sleep 2.5; X keys Escape Escape"
step "p on a card with commits of its own" "X keys ctrl+f; X type alpha; sleep 0.4; X keys Down p; sleep 2.5; X keys Escape Escape"
WAIT=4 step "refresh the fleet" "X keys ctrl+shift+r"
step "rescan" "X keys ctrl+r; sleep 1"
step "card menu: contributions" 'X press "!Contributions"; sleep 1.2'
step "back" "X keys Escape Escape"

# ---- every kind of repository, opened and walked
REPOS="merges detached conflicted broken lots delta alpha beta"
[ "$QUICK" = quick ] && REPOS="merges"
for repo in $REPOS; do
  step "open $repo" "open_repo $repo"
  for row in 0 1 2 3 4 5 6 7 8 9 10 11 12 13; do WAIT=0.45 step "$repo: commit row $row" "X pick 0:$row"; done
  step "$repo: into the files" "X pick 0:0; X keys Tab"
  for row in 0 1 2 3 4 5 6 7; do WAIT=0.45 step "$repo: file row $row" "X pick 1:$row"; done
  step "$repo: fold a group" 'X press "Untracked"'
  step "$repo: unfold it" 'X press "Untracked"'
  step "$repo: the keys that used to stage and commit" "X pick 0:0; X keys Tab space c Return; sleep 0.5"
  step "$repo: close the pane" "X keys ctrl+d"
  step "$repo: move with the pane closed" "X pick 0:2; X pick 0:1"
  step "$repo: open the pane" "X keys ctrl+d; sleep 0.5"
  step "$repo: reload" "X keys ctrl+r; sleep 0.9"
  WAIT=2 step "$repo: fetch" "X pick 0:1; X keys f"
  step "$repo: p, which is no key on a graph page" "X pick 0:1; X keys p"
  step "$repo: o" "X pick 0:1; X keys o"
  step "$repo: shortcuts" "X keys shift+question; sleep 0.3; X keys Escape"
  # The commit search key searches the repository whose graph is open.
  step "$repo: search its commits" "X keys ctrl+shift+f; sleep 0.8"
  X dump "Search commits" | grep -q "Search commits" || { WRONG=$((WRONG+1)); echo "WRONG: $repo: the commit search key did nothing in a commit graph"; }
  step "$repo: back from the search" "X keys Escape; sleep 0.4"
  step "$repo: its contributions" 'X press "Contributions in this repository"; sleep 1.3'
  step "$repo: 14 days" "X choose 'Time window=1'; sleep 1"
  step "$repo: 30 days" "X choose 'Time window=2'; sleep 1"
  step "$repo: every branch" "X choose 'Which branches=1'; sleep 1.2"
  step "$repo: an author's commits" 'X press "Fixture"; sleep 0.4'
  step "$repo: open a commit from there" "X pick 0:0!; sleep 1.2"
  step "$repo: back out" "X keys Escape Escape Escape Escape"
done

# ---- a repository with a page on the web: the button and the key
step "widgets: open" "open_repo widgets"
N=$(asked); step "widgets: the button in the header" 'X press "Open on git.example.invalid"'
asks $N 1 "the button on a graph page did not ask for the page"
N=$(asked); step "widgets: o" "X pick 0:0; X keys o"
asks $N 1 "o in a graph did not ask for the page"
step "widgets: back" "X keys Escape Escape Escape"

# ---- nothing on a graph page writes: no button to stage, commit or push.
# "Commit" whole, since a card's menu has "Commit graph" and that is a page.
step "alpha: open" "open_repo alpha; X pick 0:0; X keys Tab"
said=$(X dump-all | grep -oE 'Button "(Push|Publish|Stage|Unstage)[^"]*"|Button "Commit"' | head -1)
[ -z "$said" ] || { WRONG=$((WRONG+1)); echo "WRONG: a graph page still has a $said"; }
step "back to the grid" "X keys Escape Escape Escape"

# ---- commit search
step "commit search" "X keys ctrl+shift+f; sleep 0.3"
for q in Add "Commit number" fixture@example Merge zzzz-nothing a; do
  WAIT=1.4 step "search '$q'" "X type '$q'"; step "clear" "X keys Escape"; done
WAIT=1.4 step "search again" "X type 'Start'"
step "open a result" "X pick 0:0!; sleep 1.3"
step "walk the windowed graph" "X pick 0:3; X pick 0:1"
step "back to the grid" "X keys Escape Escape Escape"

# ---- contributions across the fleet
step "contributions" "X keys ctrl+g; sleep 1.6"
step "14 days" "X choose 'Time window=1'; sleep 1.3"
step "30 days" "X choose 'Time window=2'; sleep 1.5"
# The other view: every branch, which on these fixtures is more commits than
# the branches that are checked out have, the merges fixture's two unmerged
# branches among them. And back.
# What was counted is read off the page's status line. Not off every line
# that says "commits": an author's tooltip is one of those, and it also says
# "last commit 21 minutes ago", which is another string a minute later
# whatever was counted. About one walk in twenty reported the figures changed
# for that.
figures() { X dump "om-statusbar"; }
before=$(figures)
step "every branch" "X choose 'Which branches=1'; sleep 1.6"
X dump "all branches" | grep -q "all branches" || { WRONG=$((WRONG+1)); echo "WRONG: the page does not say it is counting all branches"; }
[ "$(figures)" != "$before" ] || { WRONG=$((WRONG+1)); echo "WRONG: every branch counted the same as the checked-out ones ($before)"; }
step "checked-out branches again" "X choose 'Which branches=0'; sleep 1.5"
[ "$(figures)" = "$before" ] || { WRONG=$((WRONG+1)); echo "WRONG: back on the checked-out branches the figures are not what they were ($(figures); before: $before)"; }
step "7 days" "X choose 'Time window=0'; sleep 1.2"
# The arcade, behind the Konami code: the same figures as a high-score table,
# the lots fixture's co-author among the players, and Escape back to the page.
step "the arcade" "X keys Up Up Down Down Left Right Left Right b a; sleep 2.2"
for shown in "HIGH SCORES" "1UP" "CO-OP"; do
  X dump "$shown" | grep -q "$shown" || { WRONG=$((WRONG+1)); echo "WRONG: the arcade does not show $shown"; }
done
step "arcade: stages" "X keys Tab"
step "arcade: badges, the second player's" "X keys shift+Tab shift+Tab Down Tab Tab"
step "arcade: 30 days" "X keys Left; sleep 1.6"
X dump "LAST 30 DAYS" | grep -q "LAST 30 DAYS" || { WRONG=$((WRONG+1)); echo "WRONG: left in the arcade did not change the window of days"; }
step "arcade: 7 days" "X keys Right; sleep 1.6"
step "game over" "X keys Escape; sleep 1.8"
X dump "HIGH SCORES" | grep -q "HIGH SCORES" && { WRONG=$((WRONG+1)); echo "WRONG: Escape did not end the game"; }
X dump "om-statusbar" | grep -q "commits" || { WRONG=$((WRONG+1)); echo "WRONG: Escape in the arcade left the Contributions page"; }
step "an author" 'X press "Fixture"; sleep 0.4'
# Each repository's commits are a list of their own. Down from the last of
# the first (merges, nine commits) goes on to the first of the next (lots).
step "an author's last commit in a repository" "X pick 0:8"
step "down into the next repository" "X keys Down"
X dump | grep -F "[focus]" | grep -q "Commit number" || { WRONG=$((WRONG+1)); echo "WRONG: Down from one repository's last commit did not reach the next one's first"; }
# Setting an author aside: under the list and out of its figures, the bar
# saying so; and back.
step "set an author aside" 'X press "Set aside"; sleep 0.6'
X dump "om-statusbar" | grep -q "1 set aside" || { WRONG=$((WRONG+1)); echo "WRONG: the status bar does not say an author is set aside"; }
X dump "Set aside, and not in the figures" | grep -q "Set aside, and" || { WRONG=$((WRONG+1)); echo "WRONG: nobody is listed under the gap"; }
step "bring them back" 'X press "Bring back"; sleep 0.6'
X dump "om-statusbar" | grep -q "set aside" && { WRONG=$((WRONG+1)); echo "WRONG: an author is still set aside"; }
# The commit search key on Contributions searches the same repositories.
step "search the commits from contributions" "X keys ctrl+shift+f; sleep 0.8"
X dump "Search commits" | grep -q "Search commits" || { WRONG=$((WRONG+1)); echo "WRONG: the commit search key did nothing on Contributions"; }
step "back to contributions" "X keys Escape; sleep 0.4"
step "back" "X keys Escape"

# ---- tags: the menu, the editor, the chips
# Up to five tags have a button each in the header; more than that and the
# menu lists them.
buttons() { [ "$(X dump | grep -c 'om-quick-tag')" = "$1" ] || { WRONG=$((WRONG+1)); echo "WRONG: the header does not have $1 tag buttons"; }; }
tags "one two three four five"; step "five tags: a button each" "X keys ctrl+r; sleep 1.2"
buttons 5
step "a tag's button on" 'X press "three"'
step "and off" 'X press "three"'
step "the menu beside the buttons" 'X press "Tag the selected repos"; sleep 0.3'
step "close it" "X keys Escape"
tags "one two three four five six"; step "six tags: the menu instead" "X keys ctrl+r; sleep 1.2"
buttons 0
step "tag menu" 'X press "Filter by tag"; sleep 0.3'
step "tag menu: clear" 'X press "!Clear tag filter"'
tags "one two three"; step "three tags" "X keys ctrl+r; sleep 1.2"
buttons 3
step "tag editor from a card" 'X press "!Tags…"; sleep 0.5'
step "tag editor: type and save" "X row 'Tags=one, extra'; X respond save; sleep 0.6"
step "tag editor again, cancel" 'X press "!Tags…"; sleep 0.5; X respond cancel; sleep 0.4'
step "tag editor: empty and save" 'X press "!Tags…"; sleep 0.5; X row "Tags="; X respond save; sleep 0.6'
step "tag the selection" 'X keys ctrl+a; X press "Tag the selected repos"; X press "!selected…"; sleep 0.5; X row "Tags=open source, a,b"; X respond save; sleep 0.6; X keys ctrl+shift+a'
tags "JM ACME"; step "two tags: chips" "X keys ctrl+r; sleep 1.2"
# A click shows that tag alone, Ctrl+click adds one. The dump says "[active]"
# of a chip that is on, and its name is a label inside it, one level deeper.
chips() { local on; on=$(X dump | awk '
    /om-quick-tag/ { match($0, /^ */); depth = RLENGTH; on = ($0 ~ /\[active/); next }
    on { match($0, /^ */); if (RLENGTH <= depth) { on = 0; next }
         if (match($0, /Label "[^"]*"/)) print substr($0, RSTART + 7, RLENGTH - 8) }' \
    | grep -xE 'JM|ACME' | sort | tr '\n' ' ')
  [ "$on" = "$1" ] || { WRONG=$((WRONG+1)); echo "WRONG: $2: the chips on are '$on', not '$1'"; }; }
step "chip on" 'X press "JM"'
chips "JM " "a click on a tag did not show it"
step "the other chip instead" 'X press "ACME"'
chips "ACME " "a click on another tag did not switch to it"
step "ctrl+click adds a chip" 'X press "ctrl+JM"'
chips "ACME JM " "ctrl+click did not add a tag"
step "a plain click on one of two" 'X press "ACME"'
chips "ACME " "a plain click on one of two did not show it alone"
step "the chip shown, again: off" 'X press "ACME"'
chips "" "a click on the one tag shown did not show everything again"
step "ctrl+click from nothing, and off again" 'X press "ctrl+ACME"; X press "ctrl+ACME"'
chips "" "ctrl+click twice did not take the tag away again"
step "filter by a tag, then lose the tag" 'X press "ACME"; sleep 0.3'
tags "JM"; step "rescan with the tag gone" "X keys ctrl+r; sleep 1.2"

# ---- themes: light, broken, odd, dark, none
# Switched the way Omarchy switches: the new theme is put together beside the
# old one and moved into its place, and then its name is written. The window
# watches the directory the two are in. Writing colors.toml where it lies,
# which is what this used to do, is not seen there: these steps changed
# nothing, and passed.
C=$W/state/omarchy/current
theme() { rm -rf "$C/next-theme"; mkdir -p "$C/next-theme"; printf "$2" > "$C/next-theme/colors.toml"
  rm -rf "$C/theme"; mv "$C/next-theme" "$C/theme"; echo "$1" > "$C/theme.name"; sleep 1; }
named() { X dump "$1" | grep -q "$1" || { WRONG=$((WRONG+1)); echo "WRONG: the window is not in the theme called $1"; }; }
step "light theme" "theme walk-light 'mode = \"light\"\nbackground = \"#eff1f5\"\nforeground = \"#4c4f69\"\naccent = \"#1e66f5\"\n'"
named walk-light
step "light theme: a graph" "open_repo merges"
step "back" "X keys Escape Escape Escape"
step "broken theme file" "theme walk-broken 'background = \"#zzzzzz\"\nforeground = 12\naccent = \"#äöå\"\n[[['"
step "odd colours" "theme walk-odd 'background = \"#fff\"\nforeground = \"#00000\"\naccent = \"é#12345\"\ncolor1 = \"\"\n'"
step "dark theme" "theme walk-dark 'background = \"#1a1b26\"\nforeground = \"#a9b1d6\"\naccent = \"#7aa2f7\"\n'"
named walk-dark
step "no theme" "rm -rf $C/theme $C/theme.name; sleep 1"

# ---- settings, row by row
step "settings" "X act settings; sleep 0.6"
for r in "Look inside repositories=on" "Show the containing repository=on" "Show the containing repository=off" \
  "Look inside repositories=off" "Tag what is found here=WORK" "Tag what is found here=two words" "Tag what is found here=" \
  "Levels below each root=1" "Levels below each root=8" "Terminal=" "Run in the terminal=" "Editor=" "Refresh every=5" \
  "Fetch from the remote=on" \
  "Fetch from the remote=off" "Only repos tagged=JM" "Only repos tagged=" "Follow the Omarchy theme=off" \
  "Follow the Omarchy theme=on" "Refresh every=0"; do WAIT=0.7 step "settings: $r" "X row '$r'"; done
step "settings: remove the root" 'X press "!Remove this root"; sleep 0.6; X respond remove; sleep 0.6'
step "settings: close" "X keys Escape; sleep 0.5"
step "empty grid" "X keys ctrl+r; sleep 1"
step "empty grid: every key" "X keys ctrl+a ctrl+g ctrl+shift+f Escape ctrl+shift+r ctrl+l Escape"

# ---- the files under the app, changed while it runs
config; step "config put back" "X keys ctrl+r; sleep 1.2"
step "config broken" "printf 'roots = [' > $W/config/omarepos/config.toml; X keys ctrl+r; sleep 1.2"
step "settings with a broken config" "X act settings; sleep 0.5; X row 'Levels below each root=4'; sleep 0.5; X keys Escape"
step "tag file broken" "printf '[repos.' > $W/config/omarepos/repos.toml; X keys ctrl+r; sleep 1.2"
step "tag editor with a broken tag file" 'X press "!Tags…"; sleep 0.4; X row "Tags=x"; X respond save; sleep 0.5'
step "both mended" "config; rm -f $W/config/omarepos/repos.toml; X keys ctrl+r; sleep 1.2"
step "a root that is not there, and one that is a file" "printf 'roots = [\"/nonexistent/place\", \"%s/fleet/alpha/README.md\"]\n' $W > $W/config/omarepos/config.toml; X keys ctrl+r; sleep 1.2"
step "settings on that" "X act settings; sleep 0.5; X keys Escape"

X stop
# ---- and nothing was written, but by the `p` that was asked for
b="$W/fleet/beta"
[ "$(git -C "$b" rev-parse main)" = "$(git -C "$b" rev-parse origin/main)" ] \
  && [ -z "$(GIT_OPTIONAL_LOCKS=0 git -C "$b" status --porcelain)" ] \
  || { WRONG=$((WRONG+1)); echo "WRONG: p did not bring beta level with its upstream, and clean"; }
as_found > "$W/as-left.txt"
if ! cmp -s "$W/as-found.txt" "$W/as-left.txt"; then
  WRONG=$((WRONG+1)); echo "WRONG: a fixture is not as it was found:"
  diff "$W/as-found.txt" "$W/as-left.txt" | head -20
fi
echo "steps: $STEPS   stopped: $DEATHS   wrong: $WRONG"
[ "$DEATHS" = 0 ] && [ "$WRONG" = 0 ]
