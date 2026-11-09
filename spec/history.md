# Histories

A version (`dir` and the other formats) is pure content. Everything about
versioning lives in a separate object, a **history** (format `history`,
[formats/history.md](formats/history.md)): which versions, in what order,
what changed at each step and whether that step is a compatible upgrade,
which versions carry which tags, and their documentation. There is no
commit object and no parent link: a node of a history is a version.

Histories are stored as a small root over a tree of parts, each in
`uni-bin` ([formats/uni-bin.md](formats/uni-bin.md)),
a general data format modeled on Uniform's primitive values. A value is a
string or a map from values to values, and the blob is its canonical
binary serialization. The same values written as canonical text are a
different format, `uni-txt` ([formats/uni-txt.md](formats/uni-txt.md)).
Views are strings `<format> <key>`; the format says which fields hold
them.

## Naming a history

A history object never changes. The store's **variables** (mutable values,
compare-and-set by name; the core spec calls them tags) give histories names: the value is
the text of a view of the current history object. Recording a version
writes a new history object and moves the name from the old view.
A lost race is a store `conflict`, and the caller retries from the new
value.

By convention the shared history is `main`, readable by the public (what
public means depends on where the store runs, spec/access.md). This is only
a convention: nothing in the format treats `main` specially.

## Workflow

Branches are not first-class and are discouraged. Each person keeps their
own private histories instead:

1. Accumulate small changes in a private history. Each change is a node;
   their keys never need to be shown to anyone.
2. Generate the public history from it: add one node (or a few) holding
   the final version after the public tip, with a changelog for the whole
   change. The public history does not reveal the private nodes, and the
   split into nodes is chosen anew.
3. When the public history moved meanwhile, rebase: replay the private
   steps onto its new tip (three-way at path level, spec/merge.md), then
   publish.

A merge (a node with two predecessors) is supported but not preferred; a
public history usually stays linear. Pushing a history moves the
destination's name only when the new history extends the old one
(fast-forward only); anything else is a new history the destination's
owner has to accept explicitly.

## Operations (module `history`)

- `load(store, name)` and `save(store, alg, name, old, history)`, the
  compare-and-set pair; `record(store, alg, name, f)` loads, applies `f`
  and saves. These read and write the whole history.
- `Stored::open(store, view)` reads a history's root and the parts above
  its leaves (formats/history-part.md), and then single nodes on demand;
  `edit(store, alg, name, f)` changes the root and a few nodes and
  rewrites only the leaves they fall in. `commit` and tagging use it, so
  their cost grows with the logarithm of the history, not its size, and
  push compares two histories by the parts they share.
- `History::add(version, after, step)` adds a node after the given
  predecessors (by default the single tip), choosing a `#n` suffix if the
  version already occurs.
- `tips`, `log` (newest first: a node before its predecessors, ties by the
  smallest id), `follows`, `merge_bases`, `compatible(a, b)`,
  `extends(old, new)`.
- `rebase` and `publish` in module `merge`.

## Conformance

`version/tests/history.rs` checks `uni-bin` (round trips, canonical form,
rejections), the history format (round trips, every validity rule),
recording and its conflict, `#n` ids, log order, merge bases, compatible
upgrades and extension.
