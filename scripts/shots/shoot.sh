#!/bin/bash
# Take the pictures the README shows, of a debug build on a display of its
# own (see scripts/walk/drive.py), looking at the fleet scripts/shots/fleet.sh
# cloned. The window has a home, a configuration and a state directory of its
# own, so nothing else of whoever runs this is in a picture: none of their
# other repositories, their tags or their accounts, and not their theme.
#
#   OMAREPOS_SHOTS_DIR=/some/scratch/dir scripts/shots/fleet.sh
#   OMAREPOS_SHOTS_DIR=/some/scratch/dir scripts/shots/shoot.sh
#
# Writes docs/img/*.png (or to OMAREPOS_SHOTS_OUT, to look before replacing
# them) at twice the size they are shown at. Needs what
# frames.sh needs (python3, gdbus, busctl, gtk4-broadwayd), ImageMagick, and
# Omarchy's stock themes. Two minutes, nothing on screen, no network: git is
# sent nowhere (fleet.sh).
HERE=$(dirname "$(readlink -f "$0")")
REPO=$(dirname "$(dirname "$HERE")")
W=${OMAREPOS_SHOTS_DIR:-$REPO/target/shots}
CODE=$W/home/Work
RAW=$W/raw
OUT=${OMAREPOS_SHOTS_OUT:-$REPO/docs/img}
THEMES=${OMARCHY_PATH:-/usr/share/omarchy}/themes
# The four themes shown side by side, and the three cards shown close up.
TILES=${SHOTS_THEMES:-catppuccin-latte gruvbox ethereal rose-pine}
CARDS=${SHOTS_CARDS:-omarepos omacosy awesome-omarchy}
[ -e "$W/.omarepos-shots" ] && [ -d "$CODE/omarchy/.git" ] \
  || { echo "no fleet in $W: run scripts/shots/fleet.sh first"; exit 2; }
( cd "$REPO" && cargo build --offline 2>&1 | tail -1 ) || exit 2
mkdir -p "$RAW" "$OUT" "$W/config/omarepos" "$W/config/gtk-4.0" "$W/state/omarepos" "$W/state/omarchy/current"

# ---- what the window is told
# An ordinary window on ~/Work, with the timer a person would have switched
# on, last run four minutes ago: so no fetch is due while the pictures are
# taken, and the status bar says when the last one was.
cat > "$W/config/omarepos/config.toml" <<'EOF'
roots = ["~/Work"]
max_depth = 3

[refresh]
every_minutes = 30
fetch = true
EOF
echo $(( $(date +%s) - 240 )) > "$W/state/omarepos/last-refresh"
# The desktop's own font, icons and header buttons, as Omarchy sets them, and
# no animation: a page that slides in is not there yet when the picture is taken.
cat > "$W/config/gtk-4.0/settings.ini" <<'EOF'
[Settings]
gtk-enable-animations=false
gtk-font-name=Adwaita Sans 11
gtk-icon-theme-name=Yaru-blue
gtk-decoration-layout=:close
EOF
# A tiled window, which is what Hyprland makes of this one, has square
# corners and no shadow. A display that tiles nothing draws both.
cat > "$W/config/gtk-4.0/gtk.css" <<'EOF'
window, window.csd { border-radius: 0; box-shadow: none; outline: none; }
EOF
tag() { local t=$1 r; shift; for r in "$@"; do TAGS[$r]="${TAGS[$r]:+${TAGS[$r]}, }\"$t\""; done; }
declare -A TAGS
# Five tags, so that each has a button in the header, with names that get
# five different colours of the eight a tag's name picks from.
tag base omarchy omakub try-omarchy aether
tag tools aether disktree omarchist flea rawmakase wayscriber hyprwhspr omarepos
tag ports omarchy-nix omarchy-on-cachyos omacosy omarchy-apple-dev
tag wiki awesome-omarchy
tag mine omarepos
for r in "${!TAGS[@]}"; do printf '[repos."%s/%s"]\ntags = [%s]\n\n' "$CODE" "$r" "${TAGS[$r]}"; done \
  > "$W/config/omarepos/repos.toml"

# ---- the window
export OMAREPOS_WALK_DIR=$W OMAREPOS_WALK_ROOT= HOME=$W/home
unset XDG_DATA_HOME XDG_CACHE_HOME
X() { python3 "$REPO/scripts/walk/drive.py" "$@" 2>&1 | grep -v "^$"; }
# A theme arrives the way `omarchy theme set` delivers one: built beside the
# place it goes, and moved in.
C=$W/state/omarchy/current
theme() { rm -rf "$C/next-theme"; mkdir -p "$C/next-theme"; cp "$THEMES/$1/colors.toml" "$C/next-theme/"
  rm -rf "$C/theme"; mv "$C/next-theme" "$C/theme"; echo "$1" > "$C/theme.name"; }
# Frames come about once a second here, and a change of width takes several
# to be over: the header puts its tag buttons back on a later one than the
# cards move on. Nothing is drawn while nothing changes, so the window has
# come to rest when the count of frames has stood still for a few seconds.
still() { local last= now same=0 i
  for i in $(seq 1 80); do sleep 0.5; now=$(X dump frames:)
    if [ "$now" = "$last" ]; then same=$((same+1)); [ $same -ge 5 ] && return 0; else same=0; last=$now; fi
  done; echo "the window did not come to rest"; return 1; }
size() { X resize "$1"; still
  X dump width: | grep -q "width: ${1%x*} " || { echo "the window did not come to $1"; return 1; }; }
shot() { X shot "$RAW/$1.png" "${2:-2}"; sleep 0.4; [ -s "$RAW/$1.png" ] || { echo "no picture of $1"; exit 1; }; }
grid() { X keys Escape Escape Escape Escape; }
open_repo() { grid; X keys ctrl+f; X type "$1"; sleep 0.6; X keys Return; sleep 2; }

theme tokyo-night
python3 "$REPO/scripts/walk/drive.py" start-offscreen || exit 2
trap 'python3 "$REPO/scripts/walk/drive.py" stop' EXIT

# ---- the fleet: every repository, as the window opens
size 1360x1010 || exit 1
shot fleet
ORDER=$(X dump .om-name | sed -n 's/.*Label "\(.*\)" \.om-name.*/\1/p')

# ---- the filter: a tag typed, and Enter about to open the top hit
X type "tag:ports"; size 1360x400
shot filter
grid

# ---- one repository: the graph, and what a commit changed
size 1280x800
open_repo quattro; X split 440; still
shot graph
# ---- work in progress: staged, not staged and untracked, with a diff
open_repo disktree; X split 250; still
shot changes
# ---- every repository's history, searched at once
grid; X keys ctrl+shift+f; sleep 0.8; X type "theme switch"; sleep 3; still
shot search
# ---- who did what this week
# The page goes up before its lines are counted, saying "counting lines…" in
# the status bar until they are: wait for the figures and for that to go.
until_read() { local i said; for i in $(seq 1 240); do said=$(X dump "om-statusbar")
    case "$said" in *"counting lines"*) ;; *"commits  ·"*) return 0;; esac; sleep 0.5; done
  echo "Contributions did not finish reading"; exit 1; }
grid; X keys ctrl+g; until_read; still
shot contributions

# ---- the four cards with something to do, in other themes
grid; X press "Show only repos needing attention"; size 690x550
for t in $TILES; do
  theme "$t"; sleep 1.5; still; shot "theme-$t"
done

# ---- cut, join and pack
# A card is 324 by 218 with 12 between, the first at 14, 65: four to a row at
# this width. Three of them, with a margin of the page around each.
card() { local i=0 n; for n in $ORDER; do [ "$n" = "$1" ] && break; i=$((i+1)); done
  echo "$(( (14 + (i % 4) * 336 - 8) * 2 )) $(( (65 + (i / 4) * 230 - 8) * 2 ))"; }
strip=()
for n in $CARDS; do
  read -r x y <<< "$(card "$n")"
  magick "$RAW/fleet.png" -crop "680x468+$x+$y" +repage "$RAW/card-$n.png"; strip+=("$RAW/card-$n.png")
done
magick "${strip[@]}" +append "$RAW/cards.png"
tiles=(); for t in $TILES; do tiles+=("$RAW/theme-$t.png"); done
magick montage "${tiles[@]}" -tile 2x -geometry +0+0 "$RAW/themes.png"
# A theme is few colours, and 256 of them hold a picture of this window at a
# third of the size with nothing to see for it, four themes side by side
# included. (Not scaled: a picture made smaller gains colours and weight.)
for p in fleet filter cards graph changes search contributions themes; do
  magick "$RAW/$p.png" -strip -alpha off -dither None -colors 256 -define png:compression-level=9 "PNG8:$OUT/$p.png"
done
ls -l "$OUT" | awk 'NR > 1 { printf "%8d  %s\n", $5, $9 }'
