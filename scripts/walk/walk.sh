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
# A tag file giving the first five repositories these tags, in turn.
tags() { : > "$W/config/omarepos/repos.toml"; local i=0 n; n=$(echo "$1" | wc -w)
  for r in alpha beta gamma merges lots; do i=$((i+1)); t=$(echo "$1" | cut -d' ' -f$(( (i-1) % n + 1 )))
    printf '[repos."%s/fleet/%s"]\ntags = ["%s"]\n\n' "$W" "$r" "$t" >> "$W/config/omarepos/repos.toml"; done; }

boot
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
WAIT=2 step "p on a card" "X keys ctrl+l p"
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
  step "$repo: stage a file" "X pick 0:0; X keys Tab space; sleep 0.8"
  step "$repo: unstage it" "X keys space; sleep 0.8"
  step "$repo: c and Escape" "X keys c; X type 'a message'; X keys Escape"
  step "$repo: commit with nothing staged" "X keys c Return; sleep 0.6; X keys Escape"
  step "$repo: close the pane" "X keys ctrl+d"
  step "$repo: move with the pane closed" "X pick 0:2; X pick 0:1"
  step "$repo: open the pane" "X keys ctrl+d; sleep 0.5"
  step "$repo: reload" "X keys ctrl+r; sleep 0.9"
  WAIT=2 step "$repo: fetch" "X pick 0:1; X keys f"
  WAIT=2 step "$repo: update" "X pick 0:1; X keys p"
  step "$repo: o" "X pick 0:1; X keys o"
  step "$repo: shortcuts" "X keys shift+question; sleep 0.3; X keys Escape"
  step "$repo: its contributions" 'X press "Contributions in this repository"; sleep 1.3'
  step "$repo: 14 days" "X choose 1; sleep 1"
  step "$repo: 30 days" "X choose 2; sleep 1"
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

# ---- a commit made and pushed from the pane
step "alpha: stage, commit, push" "open_repo alpha; X pick 0:0; X keys Tab space; sleep 0.8; X keys c; X type 'walked'; X keys Return; sleep 1.5; X press 'Push'; sleep 2"
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
step "14 days" "X choose 1; sleep 1.3"
step "30 days" "X choose 2; sleep 1.5"
step "7 days" "X choose 0; sleep 1.2"
step "an author" 'X press "Fixture"; sleep 0.4'
step "back" "X keys Escape"

# ---- tags: the menu, the editor, the chips
tags "one two three"; step "three tags" "X keys ctrl+r; sleep 1.2"
step "tag menu" 'X press "Tags"'
step "tag menu: tick a tag" 'X press "!one"'
step "tag menu: clear" 'X press "!Clear tag filter"'
step "tag editor from a card" 'X press "!Tags…"; sleep 0.5'
step "tag editor: type and save" "X row 'Tags=one, extra'; X respond save; sleep 0.6"
step "tag editor again, cancel" 'X press "!Tags…"; sleep 0.5; X respond cancel; sleep 0.4'
step "tag editor: empty and save" 'X press "!Tags…"; sleep 0.5; X row "Tags="; X respond save; sleep 0.6'
step "tag the selection" 'X keys ctrl+a; X press "Tags"; X press "!selected…"; sleep 0.5; X row "Tags=open source, a,b"; X respond save; sleep 0.6; X keys ctrl+shift+a'
tags "JM ACME"; step "two tags: chips" "X keys ctrl+r; sleep 1.2"
step "chip on" 'X press "JM"'
step "second chip on" 'X press "ACME"'
step "chips off" 'X press "JM"; X press "ACME"'
step "filter by a tag, then lose the tag" 'X press "ACME"; sleep 0.3'
tags "JM"; step "rescan with the tag gone" "X keys ctrl+r; sleep 1.2"

# ---- themes: light, broken, odd, dark, none
T=$W/state/omarchy/current/theme; mkdir -p "$T"
step "light theme" "printf 'background = \"#eff1f5\"\nforeground = \"#4c4f69\"\naccent = \"#1e66f5\"\n' > $T/colors.toml; sleep 1"
step "light theme: a graph" "open_repo merges"
step "back" "X keys Escape Escape Escape"
step "broken theme file" "printf 'background = \"#zzzzzz\"\nforeground = 12\naccent = \"#äöå\"\n[[[' > $T/colors.toml; sleep 1"
step "odd colours" "printf 'background = \"#fff\"\nforeground = \"#00000\"\naccent = \"é#12345\"\ncolor1 = \"\"\n' > $T/colors.toml; sleep 1"
step "dark theme" "printf 'background = \"#1a1b26\"\nforeground = \"#a9b1d6\"\naccent = \"#7aa2f7\"\n' > $T/colors.toml; sleep 1"
step "no theme file" "rm -f $T/colors.toml; sleep 1"

# ---- settings, row by row
step "settings" "X act settings; sleep 0.6"
for r in "Look inside repositories=on" "Show the containing repository=on" "Show the containing repository=off" \
  "Look inside repositories=off" "Tag what is found here=WORK" "Tag what is found here=two words" "Tag what is found here=" \
  "Levels below each root=1" "Levels below each root=8" "Terminal=" "Run in the terminal=" "Editor=" "Refresh every=5" \
  "Fetch from the remote=on" "Update the code when it is safe=on" "Update the code when it is safe=off" \
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
echo "steps: $STEPS   stopped: $DEATHS   wrong: $WRONG"
[ "$DEATHS" = 0 ] && [ "$WRONG" = 0 ]
