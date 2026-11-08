#!/usr/bin/env bash
# cas against git on one repository (spec/bench-git.md). Prints "operation<TAB>git<TAB>cas",
# times in seconds. F names a file of the repository for the one-file rows. Uses ports 18080.
set -euo pipefail
CAS=$(realpath "${CAS:-$(dirname "$0")/../target/release/cas}")
SRC=$(realpath "${1:?usage: git-vs-cas.sh GIT-REPO [EMPTY-WORKDIR]}")
B=${2:-$(mktemp -d)}
F=${F:-src/02-preliminaries.tex}
mkdir -p "$B"; [ -z "$(ls -A "$B")" ] || { echo "$B is not empty" >&2; exit 1; }; cd "$B"
TIMEFORMAT=%R
t() { { time "$@" >/dev/null 2>&1; } 2>&1 | tail -1; }
row() { printf '%s\t%s\t%s\n' "$1" "$2" "$3"; }
size() { du -s --apparent-size --block-size=1M "$1" | cut -f1; }
# Repositories under test: a bare git copy, and a cas store imported from it.
git clone -q --bare --no-local $SRC git.git
git -C git.git config uploadpack.allowFilter true
row "import (cas only)" "-" "$(t $CAS --store cas import-git $SRC)"
row "store size, MB (apparent)" "$(size git.git)" "$(size cas)"
row "log, every node" "$(t git -C git.git log --oneline)" "$(t $CAS --store cas log)"
row "list paths of the tip" "$(t git -C git.git ls-tree -r --name-only HEAD)" "$(t $CAS --store cas ls)"
row "history of one file" "$(t git -C git.git log --oneline -- $F)" "$(t $CAS --store cas path-log $F)"
row "check out the tip" "$(t git --git-dir=git.git --work-tree=$(mkdir -p wg && echo wg) checkout -f HEAD -- .)" "$(t $CAS --store cas checkout wc)"
# Commit one small edit to a 671-file tree.
git clone -q git.git wgit
echo "% bench edit" >> wgit/$F; echo "% bench edit" >> wc/$F
kb() { du -s --apparent-size --block-size=1K "$1" | cut -f1; }
g0=$(kb wgit/.git/objects); c0=$(kb cas)
row "commit one edit" "$(t git -C wgit commit -qam bench)" "$(t $CAS --store cas commit -m bench wc)"
row "store growth from that commit, KB" "$(( $(kb wgit/.git/objects) - g0 ))" "$(( $(kb cas) - c0 ))"
# Serve both over localhost. git: smart HTTP via git http-backend is not at hand, so git uses its
# pack protocol over a local pipe (file://), which is if anything faster than HTTP.
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 1 -subj /CN=127.0.0.1 -addext subjectAltName=IP:127.0.0.1 -addext basicConstraints=critical,CA:FALSE -keyout key.pem -out cert.pem 2>/dev/null
$CAS --store cas serve 127.0.0.1:18080 --open --cert cert.pem --key key.pem >/dev/null 2>&1 & SRV=$!
sleep 1
U=https://127.0.0.1:18080
row "clone everything" "$(t git clone -q --bare file://$B/git.git g2.git)" "$(t $CAS --store c2 --ca cert.pem pull $U)"
row "fetch one file of the tip" "$(t sh -c "git clone -q --filter=blob:none --depth 1 --no-checkout file://$B/git.git g3 && git -C g3 show HEAD:$F")" "$(t $CAS --store $U --ca cert.pem checkout one $F)"
# Push one new node / commit to the served copies.
git clone -q file://$B/g2.git wg2
echo "% push edit" >> wg2/$F; git -C wg2 commit -qam push
$CAS --store c2 checkout wc2 >/dev/null; echo "% push edit" >> wc2/$F; $CAS --store c2 commit -m push wc2 >/dev/null
sleep 1
row "push one new commit" "$(t git -C wg2 push -q origin HEAD)" "$(t $CAS --store c2 --ca cert.pem push $U)"
kill $SRV || true
