#!/bin/bash
# What a walk cannot see, because the window it drives is sent no frames:
# what becomes of the keyboard when the window changes width. A change of
# breakpoint is finished on a later frame than it starts on, and in between
# GTK and libadwaita move the keyboard about.
#
# This runs the same debug build on a display of its own instead (GTK's
# Broadway, with its own daemon and no port open), where frames do arrive,
# if only about one a second with no browser attached, and where a window
# can be given a width. Hyprland is not involved, and nothing is put on
# screen. It takes about a minute. The same display is where the README's
# pictures are taken, so what those need of it is checked here as well.
#
#     scripts/walk/frames.sh
#
# Needs gtk4-broadwayd (it comes with GTK 4), python3, gdbus and busctl.
# Exits non-zero when the keyboard was somewhere it should not have been.
HERE=$(dirname "$(readlink -f "$0")")
REPO=$(dirname "$(dirname "$HERE")")
export OMAREPOS_WALK_DIR=${OMAREPOS_WALK_DIR:-$REPO/target/walk}
W=$OMAREPOS_WALK_DIR
command -v gtk4-broadwayd >/dev/null || { echo "needs gtk4-broadwayd"; exit 2; }

( cd "$REPO" && cargo build --offline 2>&1 | tail -1 ) || exit 2
"$HERE/fixtures.sh" || exit 2
printf 'roots = [\n    { path = "%s/fleet", tag = "JM" },\n]\nmax_depth = 3\nterminal = "/nonexistent/no-such-terminal"\n' "$W" > "$W/config/omarepos/config.toml"

X() { python3 "$HERE/drive.py" "$@" 2>&1 | grep -v "^$"; }
WRONG=0
wrong() { WRONG=$((WRONG+1)); echo "WRONG: $1"; }
keyboard() { X dump "keyboard:" | grep -q "$1" || wrong "$2 ($(X dump keyboard:))"; }
# Across the width where the header changes, and back. A change takes two or
# three frames to be over, and they come one a second here. Where that width
# lies depends on what is in the header (820 px with no tag buttons in it), so
# this also checks that the two sizes are on either side of it: the header
# needs less on the narrow side, having given something up. Sizes that had
# both come to lie on one side would change nothing, and pass. That can be
# read off only with the grid in front, whose header it is: `across grid`.
across() {
  X resize 760x700; sleep 3.5; local narrow; narrow=$(X dump "width:")
  X resize 900x700; sleep 3.5; local wide; wide=$(X dump "width:")
  [ "$1" != grid ] || [ "${narrow#*needs }" != "${wide#*needs }" ] \
    || wrong "no breakpoint between the two widths ($narrow; $wide)"
}

python3 "$HERE/drive.py" start-offscreen || exit 2
keyboard GtkText "the keyboard is not in the filter at launch"

# The first change of width since launch, with the keyboard somewhere other
# than where it started: it must come back to where it was, not to there.
X type mer; sleep 1.2
X keys Down
keyboard GtkFlowBoxChild "Down from the filter did not reach a card"
across grid
keyboard GtkFlowBoxChild "a card lost the keyboard to a change of width"

X keys ctrl+f
across grid
keyboard GtkText "the filter lost the keyboard to a change of width"
X type g; sleep 1.2
X dump | grep -q 'SearchEntry "merg"' || wrong "what was in the filter was not added to after a change of width"

X keys Return; sleep 4; X pick 0:2; sleep 1.2
keyboard GtkListBoxRow "the keyboard is not on a commit"
across
keyboard GtkListBoxRow "a commit lost the keyboard to a change of width"

# What a picture of the window needs (scripts/shots), with the graph still in
# front: a size past the display's own 1024 by 768, the divider of this page
# put somewhere, a picture at twice the size, and the way back to a size the
# display allows. A PNG says how large it is in its first bytes.
X resize 1300x900; sleep 3.5
X dump "width:" | grep -q "width: 1300 " || wrong "the window did not grow past the display ($(X dump width:))"
X split 300; sleep 1.2
rm -f "$W/shot.png"; X shot "$W/shot.png" 2; sleep 1
size=$(python3 -c 'import struct, sys; print("%dx%d" % struct.unpack(">II", open(sys.argv[1], "rb").read(24)[16:]))' "$W/shot.png" 2>/dev/null)
[ "$size" = 2600x1800 ] || wrong "a picture at twice the size came out as '$size', not 2600x1800"
X resize 900x700; sleep 3.5
X dump "width:" | grep -q "width: 900 " || wrong "the window did not shrink back ($(X dump width:))"

python3 "$HERE/drive.py" alive 2>/dev/null | grep -q "^alive" || wrong "the program stopped: $(grep -m1 -A1 'panicked at' "$W/app.log" | cut -c1-200)"
X stop

# ---- a window with nothing to list
# Not a one-off look this time, which always has its fixtures: an ordinary
# window, whose roots are the configuration's, pointed at a folder that is
# not there. It has a home of its own as well, so that nothing it falls back
# on can be a folder of whoever runs this. The page has to say what it found,
# offer the way to Settings with the keyboard on it, give the keyboard back
# to that button when Settings closes, and to the filter once there is a
# fleet. Here and not in the walk because a page that has never been drawn
# is not on screen there.
ordinary() { OMAREPOS_WALK_ROOT= HOME="$W/home" python3 "$HERE/drive.py" "$@"; }
on_the_way() { X dump "Choose folders" | grep -q "\[focus\]" || wrong "$1 ($(X dump keyboard:))"; }
# A first start in a home with no caches is slow, and frames are few: wait
# for what is expected for as long as fifteen seconds, and say when it did
# not come.
until_says() { local i; for i in $(seq 1 30); do X dump | grep -q "$1" && return 0; sleep 0.5; done; return 1; }
mkdir -p "$W/home"
printf 'roots = ["%s/nowhere"]\nmax_depth = 3\nterminal = "/nonexistent/no-such-terminal"\n' "$W" > "$W/config/omarepos/config.toml"
ordinary start-offscreen || exit 2
until_says 'StatusPage "No repositories found | .*nowhere: does not exist' || wrong "an empty window does not say what it found ($(X dump StatusPage))"
until_says 'Choose folders.*\[focus\]' || wrong "with nothing to list, the keyboard is not on the way to Settings ($(X dump keyboard:))"
X press "Choose folders"
until_says "dialog: true" || wrong "the button on the empty page did not open Settings"
sleep 2; X keys Escape
until_says "dialog: false" || wrong "Settings did not close"
sleep 2
on_the_way "closing Settings did not put the keyboard back on the empty page's button"
X keys ctrl+r; sleep 3.5
on_the_way "a rescan that found nothing did not put the keyboard back on the way to Settings"
# The same, with Settings closed the moment it opens: before it has been
# drawn, which is when the keyboard was left on a button in the header.
X press "Choose folders"; X keys Escape
until_says "dialog: false" || wrong "Settings did not close when closed at once"
sleep 2
on_the_way "Settings closed at once left the keyboard somewhere else"
printf 'roots = ["%s/fleet"]\nmax_depth = 3\nterminal = "/nonexistent/no-such-terminal"\n' "$W" > "$W/config/omarepos/config.toml"
X keys ctrl+r
until_says "om-name" || wrong "the fleet did not appear after the empty page"
sleep 2
keyboard GtkText "leaving the empty page did not put the keyboard in the filter"
# The race runs on frames, which a walk's window is never sent. With
# animations off (as here) it opens at its end; from the start, played, its
# date has to move on.
X keys ctrl+g; until_says "commits  ·  .*om-statusbar" || wrong "Contributions did not open"
X keys r; until_says "om-race-date" || wrong "r on Contributions did not open the race"
X keys Home; started=$(X dump om-race-date)
X keys space; sleep 4
[ "$(X dump om-race-date)" != "$started" ] || wrong "the race did not run when played ($started)"
X keys space Escape Escape; sleep 1
python3 "$HERE/drive.py" alive 2>/dev/null | grep -q "^alive" || wrong "the program stopped: $(grep -m1 -A1 'panicked at' "$W/app.log" | cut -c1-200)"
X stop
echo "wrong: $WRONG"
[ "$WRONG" = 0 ]
