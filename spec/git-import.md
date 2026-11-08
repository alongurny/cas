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
| commit message | the changelog of each step into the node; for a root commit, the node's doc |
| tag (lightweight or annotated) | a history tag on the commit's node |

Author, committer and dates are not imported: a history records changes, not who made them or
when. A commit whose tree repeats an earlier version (an empty commit, a revert) becomes a node
with a `#n` id. Every step is marked breaking, the default.

Empty directories do not exist in git, so none are created.

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

Findings:

1. Fixed: the disk backend's `list` scanned every key, so `has` (one `list` per blob) made sync
   quadratic. A span whose bounds share `<alg>:<two hex digits>` now reads only that fan-out
   directory.
2. A history is one blob, so each new node rewrites all of it: cost follows the history's
   length, not the change. Chunking the history (and very large directories) internally, with
   the identity still over the whole content, is the next scale step.
3. Blobs are stored uncompressed and without deltas: about 8 times git's packed size here.
   Compression in the backend is an implementation detail and would not change any key.
4. Submodules have no CAS form yet. A natural one is a view of the submodule's own history (or
   version), which needs a format for it.
