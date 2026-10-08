#!/bin/bash
# Build the repositories a walk runs on, from nothing, under target/walk.
# One of each kind the application has to cope with. Nothing here is a real
# repository of anybody's, and git is given a home of its own.
set -e
HERE=$(dirname "$(readlink -f "$0")")
W=${OMAREPOS_WALK_DIR:-$(dirname "$(dirname "$HERE")")/target/walk}
chmod -R u+rwx "$W" 2>/dev/null || true
rm -rf "$W"
mkdir -p "$W/fleet" "$W/remotes" "$W/config/omarepos" "$W/config/gtk-4.0" "$W/state"
cat > "$W/gitconfig" <<'INI'
[user]
	name = Fixture
	email = fixture@example.invalid
[init]
	defaultBranch = main
[protocol "file"]
	allow = always
[advice]
	detachedHead = false
INI
# A window nobody is looking at is sent no frames, and a page transition that
# is animated never ends there.
printf '[Settings]\ngtk-enable-animations=false\n' > "$W/config/gtk-4.0/settings.ini"
export GIT_CONFIG_GLOBAL="$W/gitconfig" GIT_CONFIG_NOSYSTEM=1
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE
cd "$W/fleet"

start() { git init -q -b main "$1" && ( cd "$1" && echo "# $1" > README.md && git add . && git commit -q -m "Start $1" ); }
remote() { git init -q --bare "$W/remotes/$1.git" && git clone -q "$W/remotes/$1.git" "$1" 2>/dev/null \
  && ( cd "$1" && echo "# $1" > README.md && git add . && git commit -q -m "Start $1" && git push -q -u origin main ); }

# With a remote: one ahead and dirty, one behind, one level.
remote alpha; ( cd alpha && echo one > one.txt && git add . && git commit -q -m "Add one" && echo scratch > notes.txt )
remote beta
git clone -q "$W/remotes/beta.git" "$W/seed-beta" && ( cd "$W/seed-beta" && echo two > two.txt && git add . && git commit -q -m "Add two upstream" && git push -q )
remote gamma
# A remote that is on the web as far as its URL says, at a host that is not
# there: .invalid never resolves, so a fetch fails at once.
start widgets; ( cd widgets && git remote add origin https://git.example.invalid/acme/widgets.git )
# No commits yet, with a file waiting.
git init -q -b main delta && echo hello > delta/first.txt
# Branches and merges, long ref names, odd file names, a binary, a file with
# no final newline, a rename with an edit, and something uncommitted.
start merges
( cd merges
  for b in one two three four five; do
    git switch -q -c "feature/$b-with-a-rather-long-name-to-stretch-the-row" main
    echo $b > $b.txt; git add .; git commit -q -m "Add $b on its own branch"
  done
  git switch -q main
  for b in one two three; do git merge -q --no-ff "feature/$b-with-a-rather-long-name-to-stretch-the-row" -m "Merge $b"; done
  git tag v1
  mkdir -p "src/Åäö dir"; echo x > "src/Åäö dir/fil med mellanslag – ünïcödé.txt"
  printf 'no newline at end' > nonl.txt; head -c 2000 /dev/urandom > blob.bin
  { for i in $(seq 1 500); do echo "a line of ordinary text"; done; printf 'before\0after\n'; } > late-nul.txt
  git add .; git commit -q -m "Odd names, a binary, no trailing newline"
  git mv one.txt renamed-one.txt; echo more >> renamed-one.txt; git commit -q -am "Rename with an edit"
  echo dirty >> README.md; echo new > untracked.txt )
start detached; ( cd detached && echo 2 > two && git add . && git commit -q -m second && git checkout -q --detach HEAD~1 )
start conflicted
( cd conflicted && git switch -q -c other && echo other > README.md && git commit -q -am other \
  && git switch -q main && echo mine > README.md && git commit -q -am mine && { git merge -q other >/dev/null 2>&1 || true; } )
# An index nothing can read.
start broken; printf 'garbage' > broken/.git/index
# Enough history and enough untracked files to fold and to scroll.
start lots
( cd lots && for i in $(seq 1 14); do echo $i > "untracked-$i.txt"; done
  for i in $(seq 1 30); do echo $i >> log.txt; git add log.txt
    git commit -q -m "Commit number $i with a subject that goes on for quite a while to test the ellipsis"; done )
echo "fixtures in $W/fleet: $(ls "$W/fleet" | tr '\n' ' ')"
