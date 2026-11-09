# Function: ancestry

Input: a history (format `history`). Output: a `raw` blob, the history's
ancestry in compact form: every node with its generation and its
predecessors, in fixed-size records sorted for binary search. It answers
ancestry walks such as merge-base without reading the history's leaves,
as git's commit-graph file does. A tool may keep it for a history it
replaced, and use it for the nodes the two histories share as they are.

All integers are big-endian.

    "CASANCES" 1          magic and version (9 bytes)
    k u32                 number of kinds
    k kinds, sorted:      a node id's format key and its version's algorithm
      len u16, format key
      len u16, algorithm name
    d u8                  the version digest length (the same for every kind)
    n u32                 number of nodes
    the n nodes, sorted by (kind, digest, occurrence), no two equal, as
    columns (the digests, which do not compress, apart from the rest):
      n × kind u16        index into the kinds
      n × d bytes         the version key's digest
      n × occurrence u32  the id's `#N` number, 0 for none
      n × generation u32  as in history-part.md
      n × first u32       position of the node's first predecessor below
    e u32                 number of predecessor entries
    e × u32               node positions: node i's predecessors are
                          entries first(i) up to first(i + 1) (or e)

A node id `<format> <alg>:<hex>[#N]` is the record with the kind
(format, alg), the digest the hex spells and the occurrence N. Each
node's predecessors are its `after` entries. Each node's generation is 1
with no predecessors, else one more than the greatest of theirs. A
history whose ids have no such form (digests of different lengths, more
than 65536 kinds, an occurrence past u32) has no ancestry blob.
