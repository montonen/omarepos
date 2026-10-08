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
# screen. It takes about a minute.
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
# three frames to be over, and they come one a second here.
across() { for size in 900x700 1020x700; do X resize $size; sleep 3.5; done; }

python3 "$HERE/drive.py" start-offscreen || exit 2
keyboard GtkText "the keyboard is not in the filter at launch"

# The first change of width since launch, with the keyboard somewhere other
# than where it started: it must come back to where it was, not to there.
X type mer; sleep 1.2
X keys Down
keyboard GtkFlowBoxChild "Down from the filter did not reach a card"
across
keyboard GtkFlowBoxChild "a card lost the keyboard to a change of width"

X keys ctrl+f
across
keyboard GtkText "the filter lost the keyboard to a change of width"
X type g; sleep 1.2
X dump | grep -q 'SearchEntry "merg"' || wrong "what was in the filter was not added to after a change of width"

X keys Return; sleep 4; X pick 0:2; sleep 1.2
keyboard GtkListBoxRow "the keyboard is not on a commit"
across
keyboard GtkListBoxRow "a commit lost the keyboard to a change of width"

python3 "$HERE/drive.py" alive 2>/dev/null | grep -q "^alive" || wrong "the program stopped: $(grep -m1 -A1 'panicked at' "$W/app.log" | cut -c1-200)"
X stop
echo "wrong: $WRONG"
[ "$WRONG" = 0 ]
