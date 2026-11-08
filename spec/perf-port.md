# git's t/perf, ported

`bench/perf/run.sh REPO` runs the git `t/perf` tests that have a cas
counterpart, on both tools, on one corpus. It uses t/perf's method: each time
is the best of several runs (`REPEAT`, default 3), and setup is not timed.
The script list is from git 2.56.0. A one-to-one port is impossible, since
each tool has features the other lacks (Alon, 2026-11-08). So a test is
ported when both tools have the operation, and otherwise skipped with the
reason below. [performance.md](performance.md) has the plan these numbers
measure.

## Ported

| t/perf | test | cas counterpart |
| --- | --- | --- |
| p0001-rev-list | `rev-list --all` | `log` |
| p0001-rev-list | `rev-list HEAD -- PATH` | `path-log PATH` (the most-changed file) |
| p5600-partial-clone | count commits | `log`, counted |
| p3010-ls-files | list the tip's paths (`ls-tree -r`, since the corpus is bare) | `ls` |
| p4000-diff-algorithms | a diff (`diff --stat HEAD^ HEAD`) | `diff` |
| p0006-read-tree-checkout | checkout of the tip (into an empty directory) | `checkout DEST` |
| p0005-status | find no change: `commit --allow-empty` on a clean tree | `commit` of an unchanged checkout |
| p0008-odb-fsync | add and commit 500 new files | `commit` with 500 new files |
| p5601-clone-reference | clone (plain, over the network) | `pull` into an empty store over HTTPS |
| p5600-partial-clone | clone without blobs | none yet (cas fetches lazily by key) |
| (cas) | fetch one file without a clone | `checkout DEST PATH` against the server |
| (cas) | push one new commit | `push` |
| p9300-fast-import-export | `fast-import` of `fast-export --all` | `import-git` |
| p5600-partial-clone | `gc` | `gc` |

Both tools' network rows go over plain TCP on localhost, so neither pays for
TLS: git through `git daemon` (`git://`), cas through a benchmark build that
also speaks plain HTTP (the `bench-insecure` feature; normal builds speak
only HTTPS). git's fsync defaults differ: it
does not fsync loose objects unless configured to. cas makes every write
durable.

## Skipped, and why

- **No such feature in cas:**
  - p0002, p0004, p0007, p0090 (the index and cache-tree): cas has no index
    yet; performance.md section 4 adds a stat cache.
  - p2000-sparse-operations, p7519 and p7527 (fsmonitor), p9210-scalar.
  - p4205, p4209 and p4211 (pretty formats, pickaxe, line-log), p4220 to
    p4221, and p7810 to p7822 (grep): there is no content search over
    history.
  - p6100-describe, p6300-for-each-ref, p1400 and p1401 (refs), p5550 and
    p5551 (tags and refs in fetch): cas tags live inside histories.
  - p3404-rebase-interactive, p3904-stash-patch, p7102-reset, p7300-clean,
    p7000-filter-branch.
- **Git internals with no observable counterpart:**
  - p0003-delta-base-cache, p5302 to p5333 (pack index, many packs, bitmaps,
    name-hash, multi-pack reuse), p5304-prune, p0071-sort, p0100-globbing.
  - These come back as cas internal benchmarks when the pack backend lands
    (step 2).
- **Needs a CLI command cas lacks so far:**
  - p1006-cat-file (list every object), p1450 and p1451 (fsck),
    p6010-merge-base and p6011, p1500-graph-walks.
  - p0001's `--objects` walks.
  - p8020-last-modified.
  - Each has a library counterpart (gc's mark, `merge_bases`, path_log).
- **Not comparable as written:** p0009-diff-pathspec (diffs the working
  tree, which cas does not track), p3400-rebase (needs branches;
  cas rebases private histories).

## Results

Both rerun on 2026-11-08 after packs (c4864d4) and with both tools over plain
TCP (d4e00b1). Times in seconds. The earlier run (45b0692, cas over HTTPS
against git over a pipe, loose files) is in this file's history.

### Thesis

4,532 commits, 671 files, git 2.43.0, best of 3.

| test (t/perf script) | git | cas | cas/git |
| --- | --- | --- | --- |
| rev-list --all (p0001) | 0.038 | 0.097 | 2.6 |
| rev-list -- PATH, most-changed file (p0001) | 0.050 | 0.598 | 12.0 |
| count commits (p5600) | 0.036 | 0.121 | 3.4 |
| ls-tree -r of the tip (p3010 ls-files) | 0.003 | 0.088 | 29.3 |
| diff of the last step (p4000) | 0.003 | 0.108 | 36.0 |
| checkout the tip into an empty dir (p0006) | 0.141 | 0.205 | 1.5 |
| commit, nothing changed (p0005 status) | 0.124 | 0.056 | 0.5 |
| commit 500 new files (p0008 odb-fsync) | 0.179 | 0.299 | 1.7 |
| clone over the network (p5601) | 2.581 | 6.289 | 2.4 |
| clone without blobs (p5600) | 0.295 | - | - |
| fetch one file, no clone (cas only) | 0.165 | 0.091 | 0.6 |
| push one new commit | 0.112 | 0.596 | 5.3 |
| fast-import (p9300) / import-git | 32.876 | 5.390 | 0.2 |
| gc (p5600) | 0.645 | 6.957 | 10.8 |

Against the earlier run: clone 10.8 → 6.3 s, push 0.98 → 0.60 s, import
27.9 → 5.4 s (import now writes packs), gc 2.7 → 7.0 s (gc now repacks with
deltas: store 246 → 140 MB, git's is 96 MB).

### git.git

82,725 commits, git 2.43.0, one run each (REPEAT=1).

| test (t/perf script) | git | cas | cas/git |
| --- | --- | --- | --- |
| rev-list --all (p0001) | 0.648 | 3.035 | 4.7 |
| rev-list -- PATH, most-changed file (p0001) | 0.611 | 64.130 | 105.0 |
| count commits (p5600) | 0.581 | 2.985 | 5.1 |
| ls-tree -r of the tip (p3010 ls-files) | 0.006 | 1.996 | 332.7 |
| diff of the last step (p4000) | 0.004 | 1.928 | 482.0 |
| checkout the tip into an empty dir (p0006) | 0.280 | 2.238 | 8.0 |
| commit, nothing changed (p0005 status) | 0.198 | 0.406 | 2.1 |
| commit 500 new files (p0008 odb-fsync) | 0.379 | 0.674 | 1.8 |
| clone over the network (p5601) | 17.830 | 165.414 | 9.3 |
| clone without blobs (p5600) | 6.880 | - | - |
| fetch one file, no clone (cas only) | 0.193 | 1.939 | 10.0 |
| push one new commit | 0.126 | 9.852 | 78.2 |
| fast-import (p9300) / import-git | 77.515 | 206.613 | 2.7 |
| gc (p5600) | 11.528 | 493.454 | 42.8 |

Against the earlier run: clone 316 → 165 s (it completes now), push 17.0 →
9.9 s, import 500 → 207 s, gc 274 → 493 s (repack added). Commit got slower
(0.10 → 0.41 s): lookups now also probe the pack indexes.

## What the numbers say

- **cas is ahead or even** where the work is local and small: import on
  both corpora's scale relative to git is 0.2 (Thesis) and 2.7 (git.git),
  commit and checkout on Thesis, and fetching one file on Thesis.
- **The history object does not scale.** On git.git every command that
  reads the history parses one object holding all 82,725 nodes, about 2 s
  each. That is the whole cost of `ls`, `diff` and checkout there (2 s
  against git's milliseconds). Git reads one commit. The history index
  (step 3) is next.
- **Path history** (105 times git) is git's changed-path Bloom filters
  against a full walk, plus the history parse; step 3.
- **Push of one commit** (78 times git on git.git) still read and checked
  the remote's whole history in this run; the local diff walk (marking the
  remote history's versions present without asking) comes in the next
  commit, and the history index removes the parses.
- **gc** repacks everything each time; git's gc reuses existing deltas. An
  incremental repack (only loose objects and small packs) is the fix.
