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

Rerun on 2026-11-09 at 7071b7c: the history stored as a tree of parts
(formats/history-part.md), with packs, the graph index, the stat cache and
pack compaction, both tools over plain TCP. Times in seconds. Earlier runs
are in this file's history.

The two commit rows were measured at 7071b7c, where `commit` still read
the whole history to find the base of a sparse checkout; with that fixed
(the commit after it), a commit of an unchanged git.git checkout takes
0.13 s (git 0.20 s).

### Thesis

4,532 commits, 671 files, git 2.43.0, best of 3.

| test (t/perf script) | git | cas | cas/git |
| --- | --- | --- | --- |
| rev-list --all (p0001) | 0.038 | 0.061 | 1.6 |
| rev-list -- PATH, most-changed file (p0001) | 0.051 | 0.073 | 1.4 |
| count commits (p5600) | 0.038 | 0.041 | 1.1 |
| ls-tree -r of the tip (p3010 ls-files) | 0.003 | 0.006 | 2.0 |
| diff of the last step (p4000) | 0.003 | 0.006 | 2.0 |
| checkout the tip into an empty dir (p0006) | 0.162 | 0.138 | 0.9 |
| commit, nothing changed (p0005 status) | 0.117 | 0.054 | 0.5 |
| commit 500 new files (p0008 odb-fsync) | 0.286 | 0.272 | 1.0 |
| clone over the network (p5601) | 2.618 | 8.783 | 3.4 |
| clone without blobs (p5600) | 0.285 | - | - |
| fetch one file, no clone (cas only) | 0.157 | 0.005 | 0.0 |
| push one new commit | 0.109 | 0.234 | 2.1 |
| list every object (p1006 cat-file --batch-all-objects) | 0.041 | 0.523 | 12.8 |
| merge-base of the tip and a node 100 back (p6010) | 0.003 | 0.053 | 17.7 |
| last-modified of the root's entries (p8020) | - | 0.027 | - |
| fsck (p1450) | 2.833 | 3.306 | 1.2 |
| fast-import (p9300) / import-git | 33.311 | 6.881 | 0.2 |
| gc (p5600) | 0.654 | 6.820 | 10.4 |

### git.git

82,725 commits, git 2.43.0, one run each (REPEAT=1), 2026-11-09 after
the single-thread round (cas at 3e5dd56 plus the bench build). git's
clone uses four threads; with `pack.threads=1` it takes 28.5 s.

| test (t/perf script) | git | cas | cas/git |
| --- | --- | --- | --- |
| rev-list --all (p0001) | 0.688 | 0.939 | 1.4 |
| rev-list -- PATH, most-changed file (p0001) | 0.644 | 1.822 | 2.8 |
| count commits (p5600) | 0.720 | 1.075 | 1.5 |
| ls-tree -r of the tip (p3010 ls-files) | 0.006 | 0.015 | 2.5 |
| diff of the last step (p4000) | 0.004 | 0.008 | 2.0 |
| checkout the tip into an empty dir (p0006) | 0.620 | 0.749 | 1.2 |
| commit, nothing changed (p0005 status) | 0.218 | 0.417 | 1.9 |
| commit 500 new files (p0008 odb-fsync) | 0.389 | 0.606 | 1.6 |
| clone over the network (p5601) | 17.165 | 23.019 | 1.3 |
| clone without blobs (p5600) | 7.044 | - | - |
| fetch one file, no clone (cas only) | 0.176 | 0.009 | 0.1 |
| push one new commit | 0.131 | 0.059 | 0.5 |
| list every object (p1006 cat-file --batch-all-objects) | 0.747 | 0.253 | 0.3 |
| merge-base of the tip and a node 100 back (p6010) | 0.009 | 0.019 | 2.1 |
| last-modified of the root's entries (p8020) | - | 19.260 | - |
| fsck (p1450) | 26.967 | 29.268 | 1.1 |
| fast-import (p9300) / import-git | 80.694 | 89.241 | 1.1 |
| gc (p5600) | 10.707 | 15.304 | 1.4 |

## What the numbers say

- **git.git, 2026-11-09:** every measured command is within 3 times git
  on one thread, and push, listing objects and fetching one file are
  faster. What changed: the ancestry blob (merge-base 1.8 s -> 19 ms),
  path-log reading shared directory blobs once (41 -> 1.8 s), batched git
  reads and per-path deltas in import-git (253 -> 89 s, store 2.0 GB ->
  506 MB), clone that hashes each value once and keeps a batch's delta
  bases (207 -> 23 s), and import-git sealing its pack so the first gc
  reuses its deltas (463 -> 15 s).
- **Store size since:** delta depth 50 (506 -> 384 MB) and pack frames
  that name a blob by its 32 digest bytes instead of its 71-byte key, in
  the frame and again as a delta's base (384 -> 314 MB), and an index
  whose keys are fixed 40-byte digest records found by interpolation
  search (index 43 -> 20 MB, store 292 MB). Old frames and indexes still
  read. Then import-git's delta bases from the parent commits instead of
  the last version imported at a path (store 292 -> 192 MB, import ~4 s
  slower), and frames without their own name (the index has it) and with
  varint lengths (pack 176 -> 157 MB, git's 155 MB; with the index 177 MB
  against git's 169 MB of pack, idx and rev). The cost: import 89 -> ~100 s, fsck 29 -> 35 s, checkout user CPU
  0.23 -> 0.42 s (longer chains).
- **Allocator:** the CLI uses mimalloc. glibc's malloc spent a quarter
  of fsck finding room for the values deltas decode into: fsck 37.6 ->
  25.3 s (git 28.6), gc 15.9 -> 13.4 s, import 108 -> 102 s.
- **log and path-log:** the log is read straight from the history's
  leaves, numbered in id order, with the same checks as a whole read but
  no map of node ids; a value decoded as a base answers a read without
  a second index lookup; path-log's memo maps take `&str` keys. log 0.72
  -> 0.45 s (git 0.62), path-log 1.67 -> 1.21 s (git 0.71).
- **Offset deltas:** a delta on a base earlier in the same pack names it
  by distance, as git's do: no index lookup per chain link. Pack 157 ->
  143 MB (git's 155 MB).
- **Index lookups:** a key's hex is parsed by table, and a fixed section
  that has served many lookups builds a fanout by leading digest bits:
  835 -> 350 ns a lookup.
- **Changed paths at import:** import-git already reads both git trees of
  each commit, so it lists the changed paths there (a merge-join of raw
  tree entries against the first parent) and records the graph's filters
  with the ancestry. path-log no longer diffs every commit first: 1.16 ->
  0.31 s (git 0.71); import pays about 3.5 s for it.
- **gc mark by digest:** gc's mark and clone's copier keep the keys they
  have seen as 32 digest bytes, and a directory blob's entries are looked
  up by their digest, not spelled out in hex first (the walk reads 20M
  entries, nearly all already seen). gc 16 -> 11 s (git 11.3).
- **Import compresses less:** a write with a likely base compressed the
  value whole and as a delta and kept the smaller. A delta under 1/16 of
  the value is now kept without the whole try. git.git import CPU 125 ->
  97 s, wall 111 -> 101 s; the pack is the same size to the byte.
- **Left:** the index is 40 bytes an entry (git's 28) for 506k entries
  (git's 423k objects); import (101 s) is slower than fast-import (82-93 s)
  but not than fast-export plus fast-import (~100-110 s).

Earlier notes, before that round:

- **Thesis is at git's speed or better** for everyday commands: log,
  path history, ls, diff, merge-base and fsck are within 2 times git;
  checkout, commit and import are faster; fetching one file without a
  clone takes 5 ms.
- **The history tree** removed the cost of one big history object from
  the commands that need a few nodes. On git.git: push 6.1 → 1.1 s, a
  commit 2.8 → 0.13 s, tagging (which read and rewrote the whole history)
  0.04 s, merge-base 3.3 → 1.8 s, fsck
  299 → 236 s. merge-base, rev-list and objects still read every node:
  an ancestry walk that reads leaves on demand (or generation numbers in
  the graph) is next for merge-base.
- **Tree reads are the next cost.** path-log after the graph build and
  last-modified read thousands of 85 KB root directories, each chunked and
  verified. A cache of decoded directories and delta bases (git's p0003)
  and a compact directory encoding are next.
- **Clone** is 11 times git (200 s, 18 s for git): after compaction the
  time is per-object requests and writes, not packs.
- **gc** rewrites every pack each time (git reuses existing deltas); an
  incremental repack is the fix. Listing objects and fsck walk the whole
  store through paged listing, 114 and 11 times git.
