#!/bin/bash
# The repositories the README's pictures are taken of: the fifteen public ones
# that had the most stars on GitHub under a search for "omarchy" when the
# pictures were last taken, cloned without signing in, and a copy of this
# one. They go into a home directory of their own, so that nothing else of
# whoever runs this is in a picture: no other repository of theirs, no tag,
# no account.
#
#   OMAREPOS_SHOTS_DIR=/some/scratch/dir scripts/shots/fleet.sh
#
# About 1.2 GB and a few minutes. Clones that are already there are kept, so
# it can be run again; delete the directory to start over.
set -e
HERE=$(dirname "$(readlink -f "$0")")
REPO=$(dirname "$(dirname "$HERE")")
W=${OMAREPOS_SHOTS_DIR:-$REPO/target/shots}
CODE=$W/home/code

FLEET="
omacom/omarchy
omacom/omakub
omacom/try-omarchy
tobi/disktree
joshuaswarren/omarchy-apple-dev
goodroot/hyprwhspr
henrysipp/omarchy-nix
devmobasa/wayscriber
tahayvr/omarchist
omacom/aether
thisisgm/flea
pch/rawmakase
paulsp94/omacosy
mroboff/omarchy-on-cachyos
aorumbayev/awesome-omarchy
"

# What follows winds clones back and edits files in them. It does that only
# under a home directory this script made itself.
if [ -e "$W/home" ] && [ ! -e "$W/.omarepos-shots" ]; then
  echo "$W/home was not made by this script: not touching it"; exit 2
fi
mkdir -p "$CODE" "$W/config/git"
touch "$W/.omarepos-shots"

# git is given no configuration but this one: no credential helper, no
# rewritten addresses, and nobody's identity but that of whoever wrote the
# last commit here, which is who the pictures are taken as.
ME_NAME=$(git -C "$REPO" log -1 --format=%an)
ME_MAIL=$(git -C "$REPO" log -1 --format=%ae)
cat > "$W/gitconfig" <<INI
[user]
	name = $ME_NAME
	email = $ME_MAIL
[advice]
	detachedHead = false
INI
# The same again where libgit2 looks for it (see scripts/walk/fixtures.sh).
cp "$W/gitconfig" "$W/config/git/config"
export GIT_CONFIG_GLOBAL="$W/gitconfig" GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE

clone() {
  local name=${1##*/}
  [ -d "$CODE/$name/.git" ] && return 0
  git clone -q "https://github.com/$1.git" "$CODE/$name" && echo "cloned $1"
}
export -f clone; export CODE
echo $FLEET | tr ' ' '\n' | xargs -P 5 -I{} bash -c 'clone {}'

# This repository, as it stands here: the same commits on the same branch,
# and as far ahead of its remote as the checkout it is copied from.
[ -d "$CODE/omarepos/.git" ] || git clone -q "$REPO" "$CODE/omarepos"
branch=$(git -C "$REPO" symbolic-ref --short HEAD)
git -C "$CODE/omarepos" fetch -q "$REPO" "$branch"
git -C "$CODE/omarepos" checkout -q -B "$branch" FETCH_HEAD
if url=$(git -C "$REPO" remote get-url origin 2>/dev/null); then
  git -C "$CODE/omarepos" remote set-url origin "$url"
  git -C "$CODE/omarepos" branch -q --set-upstream-to="origin/$branch" "$branch" 2>/dev/null || true
  if pushed=$(git -C "$REPO" rev-parse -q --verify "refs/remotes/origin/$branch"); then
    git -C "$CODE/omarepos" update-ref "refs/remotes/origin/$branch" "$pushed"
  fi
fi

# Nothing is ever pushed from here, whatever is pressed, and nothing fetched:
# git, as the window runs it, is sent somewhere that is not there. (Only git.
# The window reads a remote's address itself, from the other copy of this
# configuration, and shows the real one.)
for repo in "$CODE"/*/; do git -C "$repo" config remote.origin.pushurl "no-push://pictures-only"; done
cat >> "$W/gitconfig" <<'INI'
[url "file:///dev/null/pictures-only/"]
	insteadOf = https://github.com/
INI

# Every copy starts level with its remote and clean, whatever an earlier run
# left it as.
main() { git -C "$CODE/$1" symbolic-ref --short refs/remotes/origin/HEAD | sed 's|^origin/||'; }
for repo in $FLEET; do
  name=${repo##*/}; branch=$(main "$name")
  git -C "$CODE/$name" checkout -q -f "$branch"
  git -C "$CODE/$name" reset -q --hard "origin/$branch"
  git -C "$CODE/$name" clean -q -f -d
done

# A fleet where everything is level and clean has nothing to show. So, on
# these copies only: one as it was two days ago with an edit in it, which is
# what "held back" is said of (the pictures are taken with the refresh set to
# fast-forward, so a clean copy that was merely behind would be a picture of
# something that does not happen); uncommitted work in two more; one on a
# branch of its own and one on a release.
git -C "$CODE/omacosy" reset -q --hard "$(git -C "$CODE/omacosy" rev-list -1 --before="2 days ago" "origin/$(main omacosy)")"
echo >> "$CODE/omacosy/README.md"
echo >> "$CODE/aether/README.md"; echo >> "$CODE/aether/go.mod"; echo "ideas" > "$CODE/aether/NOTES.md"
# The one whose changes a picture opens: an install prefix and a line width,
# the kind of thing somebody keeps to themselves, one staged and one not, and
# a file of notes.
sed -i 's|^PREFIX ?= .*|PREFIX ?= /usr/local|' "$CODE/disktree/Makefile"
git -C "$CODE/disktree" add Makefile
sed -i 's|^max_width = .*|max_width = 100|' "$CODE/disktree/rustfmt.toml"
echo "Scan /var as well, once the cache has been cleared." > "$CODE/disktree/notes.md"

git -C "$CODE/omarchy-apple-dev" switch -q swiftui-min 2>/dev/null \
  || git -C "$CODE/omarchy-apple-dev" switch -q -c swiftui-min --track origin/swiftui-min
git -C "$CODE/wayscriber" checkout -q "$(git -C "$CODE/wayscriber" tag --sort=-creatordate | head -1)"

echo "fleet in $CODE: $(ls "$CODE" | tr '\n' ' ')"
du -sh "$CODE" | cut -f1
