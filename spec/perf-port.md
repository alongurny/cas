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

git's network rows use its pack protocol over a local pipe (`file://`). cas
uses HTTPS on localhost, its only transport. git's fsync defaults differ: it
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

### Thesis

4,532 commits, 671 files, git 2.43.0, best of 3, 2026-11-08 after the batch
work (45b0692). Times in seconds.

| test (t/perf script) | git | cas | cas/git |
| --- | --- | --- | --- |
| rev-list --all (p0001) | 0.037 | 0.083 | 2.2 |
| rev-list -- PATH, most-changed file (p0001) | 0.050 | 0.531 | 10.6 |
| count commits (p5600) | 0.036 | 0.107 | 3.0 |
| ls-tree -r of the tip (p3010 ls-files) | 0.003 | 0.076 | 25.3 |
| diff of the last step (p4000) | 0.003 | 0.075 | 25.0 |
| checkout the tip into an empty dir (p0006) | 0.137 | 0.181 | 1.3 |
| commit, nothing changed (p0005 status) | 0.117 | 0.034 | 0.3 |
| commit 500 new files (p0008 odb-fsync) | 0.212 | 0.261 | 1.2 |
| clone over the network (p5601) | 2.422 | 10.779 | 4.5 |
| clone without blobs (p5600) | 0.272 | - | - |
| fetch one file, no clone (cas only) | 0.053 | 0.119 | 2.2 |
| push one new commit | 0.036 | 0.981 | 27.2 |
| fast-import (p9300) / import-git | 33.306 | 27.875 | 0.8 |
| gc (p5600) | 0.637 | 2.663 | 4.2 |

### git.git

82,725 commits, git 2.43.0, one run each (REPEAT=1), same day.

| test (t/perf script) | git | cas | cas/git |
| --- | --- | --- | --- |
| rev-list --all (p0001) | 0.621 | 2.397 | 3.9 |
| rev-list -- PATH, most-changed file (p0001) | 0.561 | 64.386 | 114.8 |
| count commits (p5600) | 0.585 | 3.065 | 5.2 |
| ls-tree -r of the tip (p3010 ls-files) | 0.006 | 2.125 | 354.2 |
| diff of the last step (p4000) | 0.004 | 2.119 | 529.8 |
| checkout the tip into an empty dir (p0006) | 0.286 | 2.397 | 8.4 |
| commit, nothing changed (p0005 status) | 0.215 | 0.102 | 0.5 |
| commit 500 new files (p0008 odb-fsync) | 0.445 | 0.276 | 0.6 |
| clone over the network (p5601) | 15.560 | 316.4 | 20.3 |
| clone without blobs (p5600) | 7.812 | - | - |
| fetch one file, no clone (cas only) | 0.051 | 2.593 | 50.8 |
| push one new commit | 0.072 | 16.968 | 235.7 |
| fast-import (p9300) / import-git | 78.320 | 499.644 | 6.4 |
| gc (p5600) | 9.665 | 274.344 | 28.4 |

The clone failed in this run: the client ran out of memory holding every
directory until the end. The copy now writes each directory as soon as its
subtree is in and takes a history's nodes 256 at a time. The clone was then
rerun alone: 5 min 16 s at a 1.4 GB peak (server and client together).

## What the numbers say

- **cas is ahead or even** where the work is local and small: commit (no
  index, yet it rehashes a clean tree faster than git finds no change),
  checkout on Thesis, and import on Thesis.
- **The history object does not scale.** On git.git every command that
  reads the history parses one object holding all 82,725 nodes, about 2 s
  each. That is the whole cost of `ls`, `diff` and checkout there (2.1 to
  2.4 s against git's milliseconds). Git reads one commit.
- **The store does not scale either:** import 6.4 times git's time, gc 28
  times, store 2.6 GB against 161 MB. These are loose files without deltas,
  step 2 of the plan.
- **Path history** (115 times git) is git's changed-path Bloom filters
  against a full walk, plus the history parse; step 3.
- **Push of one commit** (235 times git) reads and checks the whole remote
  history; the local diff walk and history indexes are the fix.
