# Function: graph

Input: a history (format `history`). Output: a `raw` blob, the history's
commit graph in the sense of git's commit-graph file: every node in log
order, with its predecessors by position, its changelog notes, and a
changed-path Bloom filter.

All integers are big-endian.

    "CASGRAPH" 1          magic and version (9 bytes)
    n u32                 number of nodes
    n records, in the history's log order (newest first):
      id len u16, id      the node id
      k u8                number of predecessors
      k × u32             each predecessor's record position; the top bit
                          set means the step is declared compatible
      notes len u32, notes  the steps' changelogs joined by " / "
      filter              see below

A node's predecessors are its `after` entries in their order in the
history. The filter covers the step from the first of them.

    0                     no filter: the node has no predecessor
    1, m u32, m bytes     a Bloom filter of m×8 bits
    2                     the step changed more than 8192 paths: any path
                          may have changed

A filter holds every path the step changed (any path whose object differs
between the first predecessor's version and the node's) together with all
of its parent directories. Its size is m = ⌈10·e/8⌉ bytes for e entries,
at least 8. A path p sets the bits h₀ … h₆, where hᵢ is the `u32`
little-endian at bytes 4i … 4i+3 of BLAKE3(p) modulo m×8. A bit b is bit
b mod 8 of byte ⌊b/8⌋.

A path whose bits are not all set did not change in the step from the
first predecessor. Since a node counts in a path's log only when the path
differs from every predecessor, such a node is not in the log, and its
trees need not be read: this is what git's changed-path filters do for
`git log -- PATH`.

A graph of a history that extends another can reuse the other's records by
node id; only new nodes need a diff.
