#!/usr/bin/env bash
# The git t/perf scripts that have a cas counterpart, run on both tools
# (spec/perf-port.md). Usage: run.sh GIT-REPO [EMPTY-WORKDIR]
# Prints a Markdown table: test, git seconds, cas seconds, cas/git. Each
# time is the best of $REPEAT runs (t/perf's min_time). Uses ports 18180
# and 18181. Both tools talk over plain TCP on localhost, so neither pays for
# TLS: git through `git daemon` (git://), cas through a benchmark build with
# the bench-insecure feature (plain HTTP). Normal cas builds speak only HTTPS.
set -euo pipefail
REPEAT=${REPEAT:-3}
ROOT=$(realpath "$(dirname "$0")/../..")
if [ -z "${CAS:-}" ]; then
  cargo build -q --release -p cas-cli --features bench-insecure --target-dir "$ROOT/target/bench" --manifest-path "$ROOT/Cargo.toml"
  CAS=$ROOT/target/bench/release/cas
fi
CAS=$(realpath "$CAS")
SRC=$(realpath "${1:?usage: run.sh GIT-REPO [EMPTY-WORKDIR]}")
W=${2:-$(mktemp -d)}
mkdir -p "$W"; [ -z "$(ls -A "$W")" ] || { echo "$W is not empty" >&2; exit 1; }
W=$(realpath "$W"); cd "$W"

# Best wall time of $REPEAT runs of "setup; command", timing only the command.
best() {
  local setup=$1 cmd=$2 b=""
  for _ in $(seq "$REPEAT"); do
    eval "$setup" >/dev/null 2>&1
    local s e
    s=$(date +%s.%N); eval "$cmd" >/dev/null 2>&1 || { echo FAIL; return; }; e=$(date +%s.%N)
    b=$(echo "$s $e ${b:-1e9}" | awk '{d=$2-$1; print (d<$3)?d:$3}')
  done
  printf '%.3f' "$b"
}
row() { # name, git setup, git cmd, cas setup, cas cmd
  local g c
  g=$( [ "$3" = - ] && echo - || best "$2" "$3")
  c=$( [ "$5" = - ] && echo - || best "$4" "$5")
  local r=-; [[ $g =~ ^[0-9.]+$ && $c =~ ^[0-9.]+$ ]] && r=$(awk "BEGIN{printf \"%.1f\", $c/($g>0?$g:0.001)}")
  printf '| %s | %s | %s | %s |\n' "$1" "$g" "$c" "$r"
}

# The corpus: a bare git copy and a cas store imported from it.
git clone -q --bare --no-local "$SRC" g.git
git -C g.git config uploadpack.allowFilter true
"$CAS" --store c import-git "$SRC" >/dev/null
F=$(git -C g.git log --format= --name-only | sort | uniq -c | sort -rn | awk 'NR==1{print $2}')
"$CAS" --store c serve 127.0.0.1:18180 --open --insecure-http >/dev/null 2>&1 & SRV=$!
git daemon --reuseaddr --listen=127.0.0.1 --port=18181 --base-path="$W" --export-all --enable=receive-pack "$W" \
  >/dev/null 2>&1 & GD=$!
trap 'kill $SRV $GD 2>/dev/null' EXIT
U=http://127.0.0.1:18180
G=git://127.0.0.1:18181
sleep 1

echo "corpus: $(basename "$SRC"), $(git -C g.git rev-list --count HEAD) commits, $(git -C g.git ls-tree -r HEAD | wc -l) files; $(git --version); repeat $REPEAT"
echo
echo "| test (t/perf script) | git | cas | cas/git |"
echo "| --- | --- | --- | --- |"
row "rev-list --all (p0001)" : "git -C g.git rev-list --all" : "$CAS --store c log"
row "rev-list -- PATH, most-changed file (p0001)" : "git -C g.git rev-list HEAD -- $F" : "$CAS --store c path-log $F"
row "count commits (p5600)" : "git -C g.git rev-list --all --count" : "$CAS --store c log | grep -c ."
row "ls-tree -r of the tip (p3010 ls-files)" : "git -C g.git ls-tree -r --name-only HEAD" : "$CAS --store c ls"
row "diff of the last step (p4000)" : "git -C g.git diff --stat HEAD^ HEAD" : "$CAS --store c diff"
row "checkout the tip into an empty dir (p0006)" \
  "rm -rf wg; mkdir wg" "git --git-dir=g.git --work-tree=wg checkout -f HEAD -- ." \
  "rm -rf wc" "$CAS --store c checkout wc"
row "commit, nothing changed (p0005 status)" \
  "rm -rf wg2; git clone -q g.git wg2" "git -C wg2 commit -q --allow-empty -m same" \
  "rm -rf wc2; $CAS --store c checkout wc2" "$CAS --store c -H scratch commit wc2"
row "commit 500 new files (p0008 odb-fsync)" \
  "rm -rf wg3; git clone -q g.git wg3; mkdir -p wg3/new; for i in \$(seq 500); do echo \$i\$RANDOM > wg3/new/\$i; done" \
  "git -C wg3 add -f new && git -C wg3 commit -qm new" \
  "rm -rf wc3; $CAS --store c checkout wc3; mkdir -p wc3/new; for i in \$(seq 500); do echo \$i\$RANDOM > wc3/new/\$i; done" \
  "$CAS --store c -H scratch3 commit wc3"
row "clone over the network (p5601)" \
  "rm -rf g2.git" "git clone -q --bare $G/g.git g2.git" \
  "rm -rf c2" "$CAS --store c2 pull $U"
rm -rf c2 g2.git
row "clone without blobs (p5600)" \
  "rm -rf g3.git" "git clone -q --bare --filter=blob:none $G/g.git g3.git" \
  : -
row "fetch one file, no clone (cas only)" \
  "rm -rf g4" "git clone -q --filter=blob:none --depth 1 --no-checkout $G/g.git g4 && git -C g4 show HEAD:$F" \
  "rm -rf one" "$CAS --store $U checkout one $F"
row "push one new commit" \
  "rm -rf gp.git wgp; git clone -q --bare g.git gp.git; git clone -q $G/gp.git wgp; echo x >> wgp/$F; git -C wgp commit -qam x" \
  "git -C wgp push -q origin HEAD" \
  "rm -rf cp wcp; cp -r c cp; $CAS --store cp checkout wcp; echo x >> wcp/$F; $CAS --store cp commit wcp" \
  "$CAS --store cp push $U"
rm -rf cp wcp gp.git wgp g4 one g3.git
N=$("$CAS" --store c log | grep -oE '^[0-9a-f]{12}(  | \()' | cut -c1-12 | sed -n 101p)
row "list every object (p1006 cat-file --batch-all-objects)" \
  : "git -C g.git cat-file --batch-all-objects --batch-check" : "$CAS --store c objects"
row "merge-base of the tip and a node 100 back (p6010)" \
  : "git -C g.git merge-base HEAD HEAD~100" : "$CAS --store c merge-base latest $N"
row "last-modified of the root's entries (p8020)" \
  : - : "$CAS --store c last-modified"
row "fsck (p1450)" : "git -C g.git fsck --no-dangling" : "$CAS --store c fsck"
row "fast-import (p9300) / import-git" \
  "rm -rf gi.git; git init -q --bare gi.git; git -C g.git fast-export --all > export" "git -C gi.git fast-import --quiet < export" \
  "rm -rf ci" "$CAS --store ci import-git $SRC"
rm -rf ci gi.git export
row "gc (p5600)" "rm -rf gc.git; cp -r g.git gc.git" "git -C gc.git gc -q" "rm -rf cg; cp -r c cg" "$CAS --store cg gc"
rm -rf cg gc.git
