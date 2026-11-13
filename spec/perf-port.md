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

Measured on 2026-11-13 at 3601a14, on histories of commits; 4 cloud Xeon
cores at 2.3 GHz, git 2.43.0, best of 3, both tools over plain TCP on
localhost (cas built with `bench-insecure`). Times in seconds.

### Thesis

4,925 commits (all branches), 1,292 files at the tip.

| test (t/perf script) | git | cas | cas/git |
| --- | --- | --- | --- |
| rev-list --all (p0001) | 0.045 | 0.091 | 2.0 |
| rev-list -- PATH, most-changed file (p0001) | 0.054 | 0.212 | 3.9 |
| count commits (p5600) | 0.043 | 0.091 | 2.1 |
| ls-tree -r of the tip (p3010 ls-files) | 0.004 | 0.006 | 1.5 |
| diff of the last step (p4000) | 0.003 | 0.004 | 1.3 |
| checkout the tip into an empty dir (p0006) | 0.278 | 0.189 | 0.7 |
| commit, nothing changed (p0005 status) | 0.123 | 0.018 | 0.1 |
| commit 500 new files (p0008 odb-fsync) | 0.265 | 0.216 | 0.8 |
| clone over the network (p5601) | 2.812 | 4.049 | 1.4 |
| clone without blobs (p5600) | 0.379 | - | - |
| fetch one file, no clone (cas only) | 0.201 | 0.008 | 0.0 |
| push one new commit | 0.123 | 0.087 | 0.7 |

### git.git

Not measured: git.git has paths the name rule refuses (`Märchen`, and
names with `=`, `^`, `,`, `~`, `@`, `%` in its tests), so it cannot be
imported into a history of commits (spec/types.md). Its numbers on the earlier history format (2026-11-10:
every command faster than git but ls, diff and merge-base) are in this
file's history before 3601a14.

## What the numbers say

- cas is faster than git at committing (7 times with nothing changed),
  checking out, pushing one commit and fetching one file without a clone.
- It is 2 to 4 times slower at reading history (log, path-log): every
  commit blob is read and its hash checked, a choice kept on purpose
  (2026-11-10). path-log is the largest gap.
- Clone is 1.4 times git.
- These are measurements only: Alon asked to stop optimizing on 2026-11-10.
