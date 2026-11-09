# Importing git

`cas import-git REPO [REV] [-H NAME]` (module `git`) turns the commits reachable from `REV` into a
new history named `NAME` (default `main`). It runs `git` and only reads the repository.

| git | CAS |
| --- | --- |
| tree | `dir` (entries re-sorted by name; each git tree converted once) |
| file, mode 100644 | `raw` |
| file, mode 100755 | `exec` |
| symlink, mode 120000 | `symlink` |
| submodule, mode 160000 | left out, counted as skipped |
| commit | a node whose version is its tree, after the nodes of its parents |
| commit message | the changelog of each step into the node; for a root commit, the node's doc. Re-encoded to UTF-8 by git when the commit declares an encoding; any other non-UTF-8 byte becomes U+FFFD (old git.git commits) |
| tag (lightweight or annotated) | a history tag on the commit's node |

Author, committer and dates are not imported: a history records changes, not who made them or
when. A commit whose tree repeats an earlier version (an empty commit, a revert) becomes a node
with a `#n` id. Every step is marked breaking, the default.

Empty directories do not exist in git, so none are created.

Into a local disk store, each file is written as a delta against its version at the same path in
a parent commit, and each directory blob against the parent's blob that covered the same names. A
merge takes the parent whose directory there shares the most entries (its tree is often closer to
the branch merged in than to its first parent). The last version imported at the path is only the
fallback: `--topo-order` imports a topic branch after newer commits of the branch it forked from,
and deltas against those carried every change in between (git.git: directory deltas 96 -> 14 MB,
store 292 -> 192 MB, import about 4 s slower).
These are the deltas a gc would pick, so the import ends by merging its packs into one sealed pack
(as a repack leaves it): the first gc reuses its frames instead of trying each delta again
(git.git: first gc 53 s -> 15 s).

## Dogfooding (2026-11-08)

Measured with the release build on the Thesis repository (4,532 commits; 105 MB packed in git,
796 MB of distinct blob content):

| operation | result |
| --- | --- |
| import | 18 s; 15,749 dirs, 16,385 files, 2,419 submodule entries skipped |
| store size | 844 MB on disk, about git's uncompressed content |
| history object | 5.1 MB (about 1.1 KB per node) |
| log / ls / summary / path-log | under 0.2 s each |
| checkout (670 files) | 0.1 s |
| commit one change | 0.1 s, but the store grows by 5.1 MB (a new history object) |
| first push to an empty disk store | 30 s (was over 10 min before the fix below) |
| push one new node | 0.5 s |
| private history, rebase, publish | 0.1 to 0.2 s each |
| gc run | 1.5 s |

With the encoded disk store (zstd, and chunking of blobs over 1 MiB; smaller ones whole, so packs keep them as deltas):

| operation | result |
| --- | --- |
| store size | 246 MB of files (364 MB on disk with 4 KiB blocks), was 844 MB |
| commit one change | the store grows by 26 KB, was 5.1 MB |
| import | 34 s (more, smaller files to fsync) |

Findings:

1. Fixed: the disk backend's `list` scanned every key, so `has` (one `list` per blob) made sync
   quadratic. A span whose bounds share `<alg>:<two hex digits>` now reads only that fan-out
   directory.
2. Fixed: a history is one blob, so each new node rewrote all of it. The disk backend now
   stores big blobs as content-defined chunks, so a new history object costs the chunks that
   changed; its key still hashes the whole content.
3. Fixed in part: blobs are compressed now (2.3 times git's packed size, was 8). Git also
   stores deltas between similar files, which content-defined chunks only partly match.
4. Submodules have no CAS form yet. A natural one is a view of the submodule's own history (or
   version), which needs a format for it.
