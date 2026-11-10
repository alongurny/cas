# Importing git

`cas import-git <repo> [<rev>] [-H <name>]` (module `git`) turns the commits reachable from `<rev>` into a
new history named `<name>` (default `main`). It runs `git` and only reads the repository.

| git | CAS |
| --- | --- |
| tree | `dir` (entries re-sorted by name; each git tree converted once) |
| file, mode 100644 | `raw` |
| file, mode 100755 | `exec` |
| symlink, mode 120000 | `symlink` |
| submodule, mode 160000 | left out, counted as skipped |
| commit | a node whose version is its tree, after the nodes of its parents |
| commit message | the notes of the step from the first parent (see below); cas has no commit messages, so no changelog or doc. Re-encoded to UTF-8 by git when the commit declares an encoding; any other non-UTF-8 byte becomes U+FFFD (old git.git commits) |
| tag (lightweight or annotated) | a history tag on the commit's node |

Author, committer and dates are not imported: a history records changes, not who made them or
when. A commit whose tree repeats an earlier version (an empty commit, a revert) becomes a node
with a `#n` id. Every step is marked breaking, the default.

Empty directories do not exist in git, so none are created.

## Notes

A commit message is what cas keeps as notes ([release-history.md](release-history.md)), from the
same diffs (module `git_notes`):

- **Steps.** The step from a commit's first parent gets [step notes](formats/step-notes.md): the
  whole message, its non-blank lines joined by ` / `, as the step's note; the subject (the
  message's first line) on every line the step removed or added; and the subject as the note of
  a path that has no such lines (binary, a directory, an empty file). Context lines have no note.
  A merge's other steps get no notes: they would repeat everything the first parent's
  line did, which git's own `log -p` does not show either. A root commit has no step.
- **Tips.** Each tip gets an annotated file for every text file: per line, the node that last
  changed it and its subject (`<12 hex digits of its version> <subject>`). This is a forward
  blame over the steps' diffs: a line a merge changed relative to its first parent keeps the note
  of another parent that has it unchanged. Line matching is cas's diff (Myers), not git's, so
  about one line in twenty can name a different commit than `git blame` (Thesis: 182 of 191 lines
  of `src/01-intro.tex` agree).

On git.git (82,725 commits, 4 cores): the import takes about 275 s instead of 110 s, and the store
holds 515 MB after it and 243 MB after a full gc, instead of 177 MB. Notes kept on a stored diff
(history format 3) held 556 MB after a gc: a diff's text is new bytes, which deltas against
versions do not shrink, so step notes keep runs over the files' lines instead. Diffs are computed
a window of 4,096 nodes at a time, a node per free thread (a few translation merges take seconds
each).

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
5. import-git records the derived graph too: it diffs each commit's raw git trees against its
   first parent while it reads them, so the graph's changed-path filters cost no second walk
   (path-log on git.git 1.16 -> 0.31 s).
