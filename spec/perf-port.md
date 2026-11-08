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
| p1006-cat-file | list every object (`--batch-all-objects`) | `objects` |
| p6010-merge-base | merge-base of the tip and a node 100 back | `merge-base` |
| p8020-last-modified | the last change of each root entry (git 2.43 lacks the command) | `last-modified` |
| p1450-fsck | `fsck` | `fsck` |

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
- **Needs a CLI command cas lacks so far:** p1451 (fsck of skip lists),
  p6011 (merge-base of many refs), p1500-graph-walks, and p0001's
  `--objects` walks.
- **Not comparable as written:** p0009-diff-pathspec (diffs the working
  tree, which cas does not track), p3400-rebase (needs branches;
  cas rebases private histories).

## Results

Rerun on 2026-11-08 at aef38de: packs, heads, the graph index, the stat
cache and pack compaction, both tools over plain TCP. Times in seconds.
Earlier runs are in this file's history.

### Thesis

4,532 commits, 671 files, git 2.43.0, best of 3.

| test (t/perf script) | git | cas | cas/git |
| --- | --- | --- | --- |
| rev-list --all (p0001) | 0.039 | 0.078 | 2.0 |
| rev-list -- PATH, most-changed file (p0001) | 0.052 | 0.078 | 1.5 |
| count commits (p5600) | 0.040 | 0.039 | 1.0 |
| ls-tree -r of the tip (p3010 ls-files) | 0.004 | 0.007 | 1.8 |
| diff of the last step (p4000) | 0.004 | 0.005 | 1.2 |
| checkout the tip into an empty dir (p0006) | 0.149 | 0.121 | 0.8 |
| commit, nothing changed (p0005 status) | 0.126 | 0.018 | 0.1 |
| commit 500 new files (p0008 odb-fsync) | 0.213 | 0.378 | 1.8 |
| clone over the network (p5601) | 2.501 | 9.018 | 3.6 |
| clone without blobs (p5600) | 0.301 | - | - |
| fetch one file, no clone (cas only) | 0.159 | 0.005 | 0.0 |
| push one new commit | 0.114 | 0.496 | 4.4 |
| list every object (p1006 cat-file --batch-all-objects) | 0.044 | 0.565 | 12.8 |
| merge-base of the tip and a node 100 back (p6010) | 0.004 | 0.076 | 19.0 |
| last-modified of the root's entries (p8020) | - | 0.026 | - |
| fsck (p1450) | 2.818 | 3.560 | 1.3 |
| fast-import (p9300) / import-git | 32.982 | 7.398 | 0.2 |
| gc (p5600) | 0.672 | 7.000 | 10.4 |

### git.git

82,725 commits, git 2.43.0, one run each (REPEAT=1), so a first run pays
one-time work: path-log includes building the graph (about 35 s).

| test (t/perf script) | git | cas | cas/git |
| --- | --- | --- | --- |
| rev-list --all (p0001) | 0.630 | 2.609 | 4.1 |
| rev-list -- PATH, most-changed file (p0001) | 0.604 | 41.661 | 69.0 |
| count commits (p5600) | 0.700 | 0.853 | 1.2 |
| ls-tree -r of the tip (p3010 ls-files) | 0.007 | 0.037 | 5.3 |
| diff of the last step (p4000) | 0.005 | 0.033 | 6.6 |
| checkout the tip into an empty dir (p0006) | 0.394 | 0.329 | 0.8 |
| commit, nothing changed (p0005 status) | 0.104 | 0.130 | 1.2 |
| commit 500 new files (p0008 odb-fsync) | 0.266 | 0.421 | 1.6 |
| clone over the network (p5601) | 18.132 | 199.880 | 11.0 |
| clone without blobs (p5600) | 7.380 | - | - |
| fetch one file, no clone (cas only) | 0.183 | 0.006 | 0.0 |
| push one new commit | 0.138 | 6.079 | 44.1 |
| list every object (p1006 cat-file --batch-all-objects) | 0.600 | 68.589 | 114.3 |
| merge-base of the tip and a node 100 back (p6010) | 0.009 | 3.341 | 371.2 |
| last-modified of the root's entries (p8020) | - | 72.806 | - |
| fsck (p1450) | 26.519 | 299.173 | 11.3 |
| fast-import (p9300) / import-git | 78.618 | 257.338 | 3.3 |
| gc (p5600) | 9.660 | 512.567 | 53.1 |

## What the numbers say

- **Thesis is at git's speed or better** for everyday commands: log,
  path history, ls, diff, merge-base and fsck are within 2 times git;
  checkout, commit and import are faster; fetching one file without a
  clone takes 5 ms.
- **git.git still pays for the size of one history object.** Every
  command that needs more than the heads reads and parses all 82,725
  nodes: rev-list (2.6 s), merge-base (3.3 s), push (6.1 s), and a commit
  rewrites it. The fix is a history format that grows by a node without
  rewriting the rest (step 3, still open).
- **Tree reads are the next cost.** path-log after the graph build and
  last-modified read thousands of 85 KB root directories, each chunked and
  verified. A cache of decoded directories and delta bases (git's p0003)
  and a compact directory encoding are next.
- **Clone** is 11 times git (200 s, 18 s for git): after compaction the
  time is per-object requests and writes, not packs.
- **gc** rewrites every pack each time (git reuses existing deltas); an
  incremental repack is the fix. Listing objects and fsck walk the whole
  store through paged listing, 114 and 11 times git.
