#!/bin/bash
# Record the race (Contributions, `r`) as a video, of the fleet
# scripts/shots/fleet.sh cloned: the authors of fifteen public Omarchy
# repositories and of this one, over the last 12 months, in Tokyo Night.
#
#   OMAREPOS_SHOTS_DIR=/some/scratch/dir scripts/shots/fleet.sh
#   OMAREPOS_SHOTS_DIR=/some/scratch/dir scripts/shots/race.sh
#
# A debug build on a display of its own, as shoot.sh takes its pictures, and
# the race moved on a frame at a time (`race.frame`, which only a debug build
# has): a display sends a window a frame when it likes, and a video wants
# thirty to the second, each as long as the next. Writes race.mp4, race.webm
# and race-poster.png (the last frame) to OMAREPOS_SHOTS_OUT, by default
# $OMAREPOS_SHOTS_DIR/out. Needs what shoot.sh needs and ffmpeg. About five
# minutes; the frames are a gigabyte or so while it runs, and are left
# behind with RACE_KEEP=1.
HERE=$(dirname "$(readlink -f "$0")")
REPO=$(dirname "$(dirname "$HERE")")
W=${OMAREPOS_SHOTS_DIR:-$REPO/target/shots}
CODE=$W/home/Work
FRAMES=$W/race-frames
OUT=${OMAREPOS_SHOTS_OUT:-$W/out}
THEMES=${OMARCHY_PATH:-/usr/share/omarchy}/themes
SIZE=${RACE_SIZE:-1280x720}
FPS=30
[ -e "$W/.omarepos-shots" ] && [ -d "$CODE/omarchy/.git" ] \
  || { echo "no fleet in $W: run scripts/shots/fleet.sh first"; exit 2; }
command -v ffmpeg >/dev/null || { echo "needs ffmpeg"; exit 2; }
( cd "$REPO" && cargo build --offline 2>&1 | tail -1 ) || exit 2
mkdir -p "$OUT" "$W/config/omarepos" "$W/config/gtk-4.0" "$W/state/omarepos" "$W/state/omarchy/current"
rm -rf -- "${FRAMES:?}"; mkdir -p "$FRAMES"

# The window shoot.sh takes its pictures of, without the timer: nothing is
# fetched while it records.
printf 'roots = ["~/Work"]\nmax_depth = 3\n' > "$W/config/omarepos/config.toml"
cat > "$W/config/gtk-4.0/settings.ini" <<'EOF'
[Settings]
gtk-enable-animations=false
gtk-font-name=Adwaita Sans 11
gtk-icon-theme-name=Yaru-blue
gtk-decoration-layout=:close
EOF
cat > "$W/config/gtk-4.0/gtk.css" <<'EOF'
window, window.csd { border-radius: 0; box-shadow: none; outline: none; }
EOF
rm -f "$W/state/omarepos/set-aside"
C=$W/state/omarchy/current
rm -rf -- "${C:?}/theme"; mkdir -p "$C/theme"; cp "$THEMES/tokyo-night/colors.toml" "$C/theme/"
echo tokyo-night > "$C/theme.name"

export OMAREPOS_WALK_DIR=$W OMAREPOS_WALK_ROOT= HOME=$W/home
unset XDG_DATA_HOME XDG_CACHE_HOME
X() { python3 "$REPO/scripts/walk/drive.py" "$@" 2>&1 | grep -v "^$"; }
until_says() { local i; for i in $(seq 1 120); do X dump | grep -q "$1" && return 0; sleep 0.5; done; return 1; }
# How many frames the display has drawn of the window, which stands still
# once the window has come to rest after a change of size or of page.
drawn() { X dump frames: | tr -dc 0-9; }
still() { local last= now same=0 i
  for i in $(seq 1 80); do sleep 0.5; now=$(drawn)
    if [ "$now" = "$last" ]; then same=$((same+1)); [ $same -ge 5 ] && return 0; else same=0; last=$now; fi
  done; return 1; }
python3 "$REPO/scripts/walk/drive.py" start-offscreen || exit 2
trap 'python3 "$REPO/scripts/walk/drive.py" stop' EXIT
X resize "$SIZE"; still

# Contributions over the last 12 months, and its race.
X keys ctrl+g; until_says "commits  ·  .*om-statusbar" || { echo "Contributions did not open"; exit 1; }
X choose 'Time window=5'; sleep 4
X keys r; until_says "om-race-date" || { echo "the race did not open"; exit 1; }
until_says "om-race-total" || { echo "the race has nobody in it"; exit 1; }
still

# Twenty seconds at 1×, a frame every thirtieth of one, and the last.
X act page-act race.frame=start
frames=$(( ${RACE_SECONDS:-20} * FPS ))
# A thirtieth of a second exactly: in whole milliseconds the race fell a
# fifth of a second short of its last day in twenty.
STEP=$(awk "BEGIN { printf \"%.6f\", 1000 / $FPS }")
# Each picture is drawn there and then (`render`, not `shot`, which is of
# the last frame the display asked for), so each is of the race as the step
# before it left it, whenever the display draws.
for i in $(seq 0 "$frames"); do
  f=$FRAMES/$(printf %04d "$i").png
  X act render "2:$f" >/dev/null; [ -s "$f" ] || { echo "no frame $i"; exit 1; }
  X act page-act "race.frame=$STEP"
done
last=$FRAMES/$(printf %04d "$frames").png

# Shot at twice the size and made smaller, which smooths the type; the last
# frame held for three seconds.
scale="scale=${SIZE%x*}:${SIZE#*x}:flags=lanczos,tpad=stop_mode=clone:stop_duration=3"
ffmpeg -loglevel error -y -framerate $FPS -i "$FRAMES/%04d.png" -vf "$scale,format=yuv420p" \
  -c:v libx264 -preset slow -crf 22 -movflags +faststart -an "$OUT/race.mp4" || exit 1
ffmpeg -loglevel error -y -framerate $FPS -i "$FRAMES/%04d.png" -vf "$scale,format=yuv420p" \
  -c:v libvpx-vp9 -crf 36 -b:v 0 -row-mt 1 -an "$OUT/race.webm" || exit 1
magick "$last" -resize "$SIZE" -strip -dither None -colors 256 "PNG8:$OUT/race-poster.png"
[ -n "$RACE_KEEP" ] || rm -rf -- "${FRAMES:?}"
ls -l "$OUT"/race* | awk '{ printf "%9d  %s\n", $5, $9 }'
