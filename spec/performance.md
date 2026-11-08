# Performance design

Alon asked for this design on 2026-11-08. The starting point is
[bench-git.md](bench-git.md), on Thesis (4,532 commits, 671 files):

| | git | cas |
| --- | --- | --- |
| clone | 2.5 s | 48 s |
| push one commit | 0.02 s | 0.67 s |
| history of one file | 0.05 s | 0.63 s |
| store size | 96 MB | 246 MB |
| import from git | | 27.5 s |

Checkout, commit and log are already about even with git.

The goal is to match or beat git on each operation it shares with cas, on
corpora of growing size (Thesis, git.git, Linux). That takes git's internals:
negotiation, packs, deltas, commit-graph indexes and the index file. Where
cas's model allows it, it should go further. Two constraints shape every item
below:

- **The core stays format-free.** The core server knows keys, values and
  variables, not formats. Git's server walks commits and trees itself. A cas
  server should not, so the walking stays in the client. The server gets
  generic batch operations instead, and the cost of a walk is counted in
  round trips, not in requests.
- **Identity covers the whole content.** A history's key hashes its whole
  serialized form (Alon, 2026-11-06). Splitting a value into pieces happens
  only in storage and transfer, never in identity.

## Where the time goes now

1. **Clone:** each object costs a `has` request (a one-key `list`), a read
   and a local write with two fsyncs. Thesis has about 32,000 objects, so
   that is about 64,000 sequential round trips.
2. **Push one commit:** `push` reads the remote history whole (5 MB) to check
   the fast-forward. Then `copy` visits every one of the 4,532 node versions
   and asks the remote about each one. Only one of them is new.
3. **History of one file:** each node resolves the path from the root, one
   directory read per level. Every read decompresses again, nothing is
   cached, and nothing remembers which paths a step changed.
4. **Size:** zstd plus content-defined chunks, but no deltas between
   versions of a file. A one-line edit stores the whole file again.
5. **Import:** two fsyncs per blob and per chunk file.

## 1. Transfer: batched, level-wise, negotiated locally

Git negotiates: the client sends "want" and "have" commits, and the server
sends one pack of everything reachable from the wants and not from the haves,
with deltas against objects the client has (a thin pack). Protocol v2 also
skips ahead through long histories while negotiating.

cas gets the same effect without moving format knowledge into the server.

**Generic batch operations in the wire.** Each of these is gated key by key
like the single operation it batches:

| Request | Body | Answer |
| --- | --- | --- |
| `POST /batch/has` | keys | the keys absent |
| `POST /batch/read` | keys | frames `(key, status, value)`, streamed |
| `POST /batch/write` | frames `(key, expect, value)` in order | per-frame outcomes |

Frames are length-prefixed, and the stream is zstd-compressed as a whole.
Writes go to storage in the order given, so the closure invariant holds if
the writer sends children before parents. Writing a batch costs one fsync,
not two per blob (see section 2). A variable write can close the batch, and
it then applies only if every blob before it was written.

**The client computes what the other side lacks.** Histories make
negotiation free:

- **Have-set:** the nodes of the other side's history. A push already reads
  that history for the fast-forward check. A pull uses the client's own last
  copy of it.
- **New nodes:** the nodes of the new history that are not in the old one.
  Because of the closure invariant, everything reachable from an old node is
  already on the other side.
- **Objects to send:** for each new node, walk its version beside its
  predecessor's, which is already on the other side. Descend only where the
  views differ. This is git's tree-diff pruning when it builds a pack, and it
  runs locally on the side that has both trees. The cost is the size of the
  change, not the size of the history.

A push of one commit becomes: read the remote head, do the local diff walk,
then send one `batch/write` with the few new blobs and the variable.

**Clone and pull without a local base:** the client walks the remote breadth
first. Each round is one `batch/has` against the local store, then one
`batch/read` of everything at that depth across all nodes. The number of
rounds is the depth of the deepest tree, about 10, not the number of
objects. Single-object fetch (`GET /k/{key}`) stays as it is.

**Parallelism:**

- A big round is split across several connections. The server already has a
  pool of workers.
- The client hashes and checks frames on a thread pool while the next
  request is in flight (pipelining).
- With TLS, each connection is kept alive for the whole sync, so the
  handshake is paid once.

**Big values in transfer:** reading by chunks is the read-side twin of the
chunked write that exists already. `GET /chunks-of/{key}` returns a blob's
chunk ids, and the client fetches only the chunks it lacks. The client can
cut its own old copy of the value (for example the previous history) with
the same FastCDC parameters, so it knows its chunk ids without a chunk store
of its own. This is rsync's idea applied to content-defined chunks. A pull
after one new commit then moves a few chunks of the history, not 5 MB.

**Expected:** clone about 10 round trips plus the bytes, so it is close to
git when the bytes are equal (section 2 makes them so). Push of one commit
is two round trips.

## 2. Storage: packs, deltas, and one fsync per batch

Git keeps recent objects loose and packs the rest. A pack is one file of
objects, many of them deltas against similar objects. Git picks delta bases
with a sliding window over objects sorted by type, a hash of the path name,
and size, and it bounds the delta chains. A `.idx` file next to it holds a
fanout table by first byte, sorted hashes and offsets. Several packs are tied
together by a multi-pack index, and reachability bitmaps speed up gc and
pack building.

The plan for a pack backend (internal to the disk store, same keys, same
`Backend` trait):

- **Pack files:**
  - Append-only files of frames.
  - A batch write (section 1) or an import appends one pack and fsyncs it
    once. Small single writes stay loose files, as now.
  - `cas repack` merges packs and loose files.
- **Index:**
  - An open-addressing hash table from key to (pack, offset) per pack. Blake3
    keys are uniformly random, so a slot is the key's leading bits. A lookup
    is one probe, against about 17 compares for git's binary search.
  - A small in-memory Bloom or xor filter per pack answers `has` for absent
    keys without touching the disk.
  - A multi-pack index merges the per-pack tables once packs pile up.
- **Deltas:**
  - A frame can be stored as a zstd delta against a base frame in the same
    pack (zstd's `--patch-from`, a dictionary made of the base), with a
    bounded chain depth.
  - The backend is format-free, so the writer passes a hint with each frame:
    "similar to key K". The diff walk in section 1 knows it at no cost,
    because K is the same path in the predecessor. This replaces git's
    path-name hash heuristic with the real predecessor.
  - A backend may ignore the hint. Identity is unchanged, since keys hash the
    whole value.
- **Big blobs** keep the content-defined chunks. Deltas cover the small,
  often-edited files that chunks miss, which is most of a source tree.
- **Decoded-object cache:** an LRU of decompressed values, like git's delta
  base cache. Walks reread the same directories again and again.

**Expected:** size close to git's pack, against 2.6 times now. Import drops
from 27 s to a few seconds, since there are a few fsyncs instead of about
60,000. The diff walk also gives each blob its delta base.

## 3. History indexes: cas's commit-graph

Git's commit-graph file stores, for each commit, its parents as positions, its
root tree and a generation number. Generation numbers cut ancestry queries
short. Since 2.27 it can also store a changed-path Bloom filter per commit:
`git log -- path` skips any commit whose filter says the path did not change.
That is why git's history of one file takes 0.05 s.

cas already has the right mechanism: derived data cached under `derived/F/X`
for a function F (spec/derived.md). The plan is a function `graph`, whose
result for a history is one uni-bin index:

- **Topological numbering:** node to position, predecessor positions, and a
  generation number for each node.
- **Ancestry by intervals:** pre/post-order intervals on a spanning tree,
  with generation numbers as a cut-off for the non-tree edges. For
  linear-ish histories that makes "does A precede B" constant time.
  `extends`, `merge_bases` and `compatible` stop walking the whole node map.
- **Changed paths:** for each step, a Bloom filter (git's parameters: 10 bits
  per entry, 7 hashes) of the paths that changed, directories included.
  `path_log` tests the filter and resolves only the nodes that may have
  changed the path, with the cache from section 2.
- **Incremental update:** a history that extends another reuses the index
  of the old one and adds only the new nodes. Derived results are keyed by
  input, so the old index is still there.

**Expected:** history of one file below git's 0.05 s on Thesis. The ancestry
checks behind push, rebase and publish stop growing with the length of the
history.

## 4. Working tree: an index file and parallel I/O

Git's index caches stat data (mtime, ctime, size, inode) for each tracked
file. `git status` and `git commit -a` rehash only the files whose stat data
changed. Fsmonitor goes further and asks the OS what changed. Git can also
check out on several workers (`checkout.workers`).

- **Stat cache:** `cas commit DIR` keeps a local stat cache (outside the
  store, like `.git/index`) from path to stat data and view. It rehashes only
  the files whose stat changed, and reuses a directory's view when nothing
  under it changed (git's cache-tree). Racy-git rule: an entry whose mtime is
  not older than the cache itself is rehashed.
- **Parallel I/O:** hashing on commit and writing on checkout run on a
  thread pool. Blake3 also uses several cores for big files.

**Expected:** commit on Linux (about 80,000 files) costs the files that
changed, not all 80,000. Checkout gains from parallel writes on multi-core
machines.

## 5. Data structures beyond git

These are places where cas's model allows something git cannot do easily:

- **Hash-table pack index with filters** (section 2): constant-time lookup
  and no disk access for absent keys. Git's `.idx` is a binary search.
- **Real delta bases:** each delta base is the file's actual predecessor, not
  a guess from a window of similar names and sizes.
- **Prolly trees for huge directories:**
  - A `dir` format variant whose node is a content-defined, balanced tree of
    entries.
  - Formats nest by subtree (spec/version.md), so only the directories that
    need it pay for it.
  - Editing one entry of a 100,000-entry directory then rewrites
    O(log n) small nodes, not the whole listing. Git rewrites the whole tree
    object.
- **No parents in versions:** cas versions name no parents, so two histories
  that contain the same trees share all their objects. Git rewrites every
  commit after an amend or a rebase. The `graph` index keeps ancestry fast
  despite the missing pointers.
- **Batch operations as generic core operations:** caches, mirrors and gc
  use them too, not only sync.

## 6. Benchmarks: a port of git's `t/perf`

A one-to-one port is not possible, because each tool has features the other
lacks. The rule is:

- **Port** a script when cas has the same operation: rev-list and log,
  log of a path, ls-tree, checkout (read-tree), commit, diff, clone and fetch,
  push, fsck, gc and repack, and fast-import (cas's `import-git`).
- **Skip**, with the reason recorded, scripts about features cas does not
  have: the index format details, sparse checkout, pack bitmaps as such,
  pickaxe and grep engines, describe, filter-branch, fsmonitor until
  section 4 has one, and submodules.
- **Add** scripts for what git lacks or makes awkward: single-object fetch
  without a clone, private-history rebase and publish, the `compatible`
  check, and derived summaries.

The script names will be checked against git's `t/perf` directory when it
is ported. The port keeps t/perf's method: each test runs several times and
the best time counts. It runs on the same repository and the same machine
for both tools. Results are recorded per corpus:

- Thesis
- git.git
- the Linux kernel
- a synthetic large-directory repository, for the prolly-tree case

## Progress

- 2026-11-08, step 1 in part: the batches and the breadth-first copy. Thesis
  clone over HTTPS 48 s → 9.5 s, push of one commit 0.67 s → 0.37 s. Samples
  of the clone show the client's time in creating, writing and renaming
  about 60,000 loose files (blobs and chunks): that is step 2. The local diff
  walk for push is still to do.

- 2026-11-08, git.git (82,725 commits) through the t/perf port
  ([perf-port.md](perf-port.md)): every command that reads the history
  spends about 2 s parsing it whole. Step 3 therefore also has to make
  reading the tip, tags and recent nodes cheap without parsing every node.
  The history's identity still covers its whole content, so this is a
  derived index (or a cached parse keyed by the history's key), not a
  change of format.

- 2026-11-08, step 2: packs (`src/pack.rs`). A batch of more than one blob
  is written as one pack file plus its index, with one durable flush, instead
  of one loose file each. `cas gc` repacks: it orders every blob by history,
  takes delta hints from `gc::delta_hints` (the same path in the previous
  version, and for a chunked file the chunk at the same position), and keeps
  a zstd-with-dictionary delta when it is smaller, with chains at most 10
  deep. Thesis store 246 MB → 140 MB after gc (git 96 MB, 90 MB of it PDFs);
  `import-git` now batches its writes, 27.9 s → 5.4 s.

## Order of work

Each step comes with its row in the benchmark, measured before and after.

1. **Batch operations, level-wise clone and local diff walk** (section 1).
   This is the biggest gap: clone 48 s and push 0.67 s. It also produces
   the "similar to" hints that step 2 needs.
2. **Pack backend:** one fsync per batch, hash index, filters, deltas and
   the decoded-object cache (section 2). Targets: size and import.
3. **The `graph` derived index** with changed-path filters (section 3).
   Targets: path history and ancestry checks.
4. **Chunk-wise reads for big values** (section 1). Target: pull after one
   new commit.
5. **Stat cache and parallel working-tree I/O** (section 4). Target: commit
   and checkout on Linux.
6. **Prolly-tree directories** (section 5), driven by the synthetic corpus.

The `t/perf` port runs alongside step 1, so every later step is measured on
all corpora.
