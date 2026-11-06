# Format: history

A history: versions in an order, with everything that concerns versioning
(changelog, compatibility, tags, documentation). Versions never refer to
histories, and nothing in a version refers to another version: there are
no parents. A history is an interface over versions, not part of them, so
replacing a bad step makes a new history and leaves every version's key
unchanged.

The blob is a `uni-bin` value: a map with these keys, each optional (an
absent key means empty).

- `'doc'`: a string describing the history.
- `'nodes'`: a map from node id to node.
- `'tags'`: a map from tag name (any non-empty string) to a node id.

## Nodes

A node id is a string: a view `<format> <key>` of a version, optionally
followed by `#` and a decimal number from 2 up without leading zeros, for a
version that occurs more than once (`#2` is its second occurrence). The
version of a node is its id without the suffix. Since a version's key
depends only on its content, two histories that hold the same version
agree on its node id.

A node is a map with these keys, each optional:

- `'after'`: a map from the id of each node this one follows (its
  predecessors, all in `'nodes'`) to a step.
- `'doc'`: a string, documentation of this version.

A node with no `'after'` is a root. A node with two or more predecessors is
a merge. The `'after'` relation must be acyclic.

## Steps

A step is the change from a predecessor to a node, a map with optional
keys:

- `'changelog'`: a string describing the change.
- `'compatible'`: present (value `{}`) when the step is declared a
  compatible upgrade; absent, the step breaks compatibility. The default
  is deliberately conservative: compatibility is an explicit, per-step
  promise, not inferred from version numbers.

## Order and derived notions

- Tips: nodes no other node follows. A history with one tip is linear at
  its end; its tip is "the latest".
- `B` follows `A` (transitively) when a chain of steps leads from `A` to
  `B`.
- Upgrading from `A` to `B` is compatible when some chain from `A` to `B`
  consists only of compatible steps.
- Extension: history `H'` extends `H` when every node of `H` is in `H'`
  with the same steps. Tags and docs are not part of this relation. A
  fast-forward-only update requires an extension.

## Validity

Every key is one of those above; node ids parse; every predecessor and
every tag target is a node; the order is acyclic; and every value is of the
type given. The key of the blob is over all of it: storage may be chunked
by an implementation, but the history's identity is its whole content.
