# cas against git

> These measurements were made on the earlier history format, which was
> removed on 2026-11-11 in favour of histories of commits. They are kept as
> a record; some commands and functions they name no longer exist.

`bench/git-vs-cas.sh REPO` runs the same operations with git and with the
`cas` command on one repository. Measured on 2026-11-08 on the Thesis
repository (4,532 commits, 671 files at the tip), release build, one
machine, servers on localhost. git's transport is its pack protocol over a
local pipe (`file://`), which is if anything faster than its HTTP.
Times are seconds of one run each. cas's network rows run over HTTPS
(its only transport since 659d1b8), git's over its plain pack protocol.

| operation | git | cas |
| --- | --- | --- |
| import from git (cas only) | | 27.5 |
| store size, MB | 96 | 246 |
| log, every node | 0.04 | 0.08 |
| list paths of the tip | 0.002 | 0.07 |
| history of one file | 0.05 | 0.63 |
| check out the tip | 0.13 | 0.17 |
| commit one edit | 0.13 | 0.11 |
| store growth from that commit, KB | 14 | 36 |
| clone everything over the network | 2.5 | 48 |
| fetch one file of the tip over the network | 0.03 | 0.09 |
| push one new commit | 0.02 | 0.67 |

Fetching one file: git needs a partial, shallow clone (`--filter=blob:none
--depth 1`, which the server must allow) and then `git show`; cas reads the
history, then the directories on the path, then the file, one request each.

## Where cas loses, and why

1. **Clone: one request per object.** A pull walks the history and, for each
   object, asks whether the destination has it and then fetches it: about
   32,000 sequential round trips. git sends one pack. The fix is in the
   wire, not the store: a request that names many keys (or many in flight
   at once). Before the fix in this round (TCP_NODELAY on the server) each
   request also waited about 40 ms for a delayed ACK, and the same clone
   took over 10 minutes.
2. **Push one commit: the whole history object, and every node.** The
   remote's history is read whole (5 MB here) for the fast-forward check,
   and the copy then asks the remote about each of the 4,532 node versions
   although one is new. See [performance.md](performance.md).
3. **History of one file** walks every node and compares the path's view at
   each step; git compares tree ids in one process with its object cache.
   A derived, cached result (like `summary`) would make repeats free.
4. **Size: no deltas.** zstd and content-defined chunks reach 2.6 times
   git's pack. Edits inside a small file still store the whole file again.

Where cas ties: checkout, commit, log, and fetching one object without
any clone. The plan to close the rest is [performance.md](performance.md).
