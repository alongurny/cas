# Format: history-part

A part of the node tree of a history ([history.md](history.md)): either
a leaf holding nodes, or an inner part holding the parts one level down.
The blob is a `uni-bin` value, a map with exactly one of these shapes:

- A leaf: `{'nodes': N}`, where N is a non-empty map from node id to node,
  as history.md describes nodes, each with one more key:
  `'generation'`, a decimal string without leading zeros: 1 for a node
  with no predecessors, else one more than the greatest generation of its
  predecessors (git's commit-graph generation number). A node always has
  a greater generation than every node it follows, so an ancestry walk
  that takes nodes by generation, greatest first, can stop as soon as
  what is left cannot matter. The history's content does not include it:
  it is fixed by the nodes.
- An inner part: `{'level': L, 'parts': P}`, where L is a decimal string
  from `1` without leading zeros, and P is a non-empty map from node id to
  a view (a string `<format> <key>`) of a `history-part` at level L - 1.
  A leaf is at level 0. Each id in P is the greatest node id under that
  part.

Node ids are ordered as byte strings.

## Chunking

The tree is determined by the set of node ids, so a history has one tree
whatever order its nodes were added in. The **height** of a node id is
the number of trailing zero bits of the first 8 bytes of BLAKE3(id),
read as a little-endian unsigned integer, divided by 6 and rounded down.

At level 0 the history's nodes, in id order, are cut into leaves: a leaf
ends right after each id of height greater than 0, and the last leaf ends
with the greatest id. At level L >= 1 the parts of level L - 1, in order,
are cut the same way by the greatest id under each, ending a part after
each id of height greater than L. Levels are built until one level has a
single part, which is the top: the root's `'nodes'` view names it. So a
leaf holds 64 nodes on average, an inner part 64 parts, and a top inner
part always has at least two.

Equivalently, a tree is valid when: every generation is as above; every
leaf and inner part is at the level its parent says; the ids under each part are greater than those
under the part before it on the same level; within each part every id
but the greatest has height at most its level; the greatest has height
greater than its level unless the part is the last of its level; each id
in an inner part is the greatest id under that child; and the top, if
inner, has at least two parts.

## Cost

Adding or changing a node rewrites the leaf it falls in (sometimes two,
when a chunk end is removed) and one part on each level above it: about
log64 of the number of nodes. A reader of one node reads the inner
parts (one 64th of the leaves' count, all small) and one leaf. Two trees
are compared by the views they hold: equal views hold equal nodes, so
push and extension checks read only the leaves that differ.
