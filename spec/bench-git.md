# cas against git

`bench/git-vs-cas.sh REPO` runs the same operations with git and with the
`cas` command on one repository. Measured on 2026-11-08 on the Thesis
repository (4,532 commits, 671 files at the tip), release build, one
machine, servers on localhost. git's transport is its pack protocol over a
local pipe (`file://`), which is if anything faster than its HTTP.
Times are seconds of one run each.

| operation | git | cas |
| --- | --- | --- |
| import from git (cas only) | | 27.1 |
| store size, MB | 96 | 246 |
| log, every node | 0.05 | 0.09 |
| list paths of the tip | 0.003 | 0.07 |
| history of one file | 0.06 | 0.64 |
| check out the tip | 0.50 | 0.35 |
| commit one edit | 0.12 | 0.14 |
| store growth from that commit, KB | 15 | 36 |
| clone everything over the network | 2.4 | 51 |
| fetch one file of the tip over the network | 0.03 | 0.09 |
| push one new commit | 0.02 | 0.94 |

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
2. **Push one commit: the whole history object.** The remote's history is
   read and the new one written whole (5 MB here); chunking in the store
   does not help over the wire on reads. The chunked write sends only the
   new chunks, so the cost is the read. Reading by chunks (a request for a
   blob's chunk ids) would fix it the same way.
3. **History of one file** walks every node and compares the path's view at
   each step; git compares tree ids in one process with its object cache.
   A derived, cached result (like `summary`) would make repeats free.
4. **Size: no deltas.** zstd and content-defined chunks reach 2.6 times
   git's pack. Edits inside a small file still store the whole file again.

Where cas wins or ties: checkout, commit, log, and fetching one object
without any clone.
