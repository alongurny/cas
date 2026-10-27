# Version format (Stage 4)

A version is a canonical Merkle encoding of a map from paths to entries, stored as ordinary core
blobs whose bytes contain other blob keys. The core knows nothing of it: to the core these are
opaque blobs, and this format is tooling (crate `cas-version`).

## 1. Paths and entries

A **name** is a non-empty UTF-8 string that is not `.` or `..` and contains no `/` and no control
character. A **path** is one or more names joined by `/`.

**Canonical order** compares paths name by name, each name bytewise. So `a/z` sorts before `a-b`,
even though `-` is below `/` bytewise. A directory's contents follow right after the directory's
own path and come before its next sibling, so any prefix selects one contiguous run.

An **entry** is a pair (content key, mode). The content key is a blob key of a registered algorithm
with a digest of the right length. Its blob holds the file's bytes. The mode is one of:

| mode   | meaning                                    |
|--------|--------------------------------------------|
| `file` | regular file                               |
| `exec` | executable file                            |
| `link` | symbolic link; the blob holds the target   |

A version maps paths to entries. No path in it lies under another path in it (nothing is both a
file and a directory). Directories exist only as prefixes, so a version cannot hold an empty one.

## 2. Shapes

How the map is split into blobs is the version's **shape**. Every node blob of every shape begins
with the header line

```
cas-tree <shape>\n
```

where `<shape>` names the shape and its format revision, such as `path/1`. A reader picks the
shape from the root's header and rejects a shape it does not know. A version's identity is its root
key, so the same map has different roots in different shapes. Comparing versions across shapes
means walking them (section 5).

Every shape must satisfy these rules:

- **History independence.** The same map, written with the same hash algorithm, gives the same root
  key, whatever sequence of edits produced it.
- **Canonical decoding.** Each node has exactly one valid encoding. Readers reject every other
  encoding as malformed, so an invalid node can never alias a valid one.
- **Cost follows the change.** Looking up a path, updating a few paths, walking a prefix, and
  diffing versions that differ in a few paths each read and write blobs in proportion to the
  paths involved (times depth), not the size of the version.

The writer chooses the hash algorithm for the nodes it writes. Child references are full keys, so
nodes written with different algorithms mix freely. History independence holds per algorithm.

## 3. Shape `path/1`

One node per directory, like git's trees. A node is UTF-8 text:

```
cas-tree path/1\n
<type> <key> <name>\n        (zero or more lines)
```

- `<type>` is a mode (`file`, `exec`, `link`), or `tree` for a subdirectory, whose `<key>` is the
  key of that directory's `path/1` node.
- Exactly one space separates the fields. `<name>` is the rest of the line. It may contain spaces,
  but newlines are excluded because names have no control characters.
- Lines are in strictly ascending bytewise order of `<name>`, which makes them unique too.
- Every line, including the last, ends in `\n`. Nothing follows the last line.
- Only the root may have no lines. A `tree` line pointing to an empty node is malformed.

Example: a root holding `a/x` and the executable `b c`:

```
cas-tree path/1
tree blake3:5d1c… a
exec blake3:8e4c… b c
```

(Keys are shortened here.)

## 4. Operations

- **build(map)** writes a version holding exactly `map` and returns its root.
- **update(root, edits)** applies a batch of edits. `set p e` maps `p` to `e`, replacing whatever
  was at `p`, including a whole directory. `remove p` removes `p` and everything under it; an
  absent path is fine. A batch acts as all its removes followed by all its sets in canonical
  order. A set below a path that is a file at that point, or below another set in the same batch,
  is a **path conflict**, and the version is left unchanged. Directories left empty disappear.
  Only nodes on the paths of the edits are read or written. A node that would come out unchanged
  is not rewritten.
- **lookup(root, p)** returns the entry at `p`, or absent (directories are not entries).
- **walk(root, prefix)** yields the entries at or under `prefix` (all of them when there is no
  prefix) in canonical order. It reads only the nodes on the way to the prefix and below it. This
  is what a sparse checkout uses.
- **diff(a, b, prefix)** yields every path at or under `prefix` whose entry differs, as (path, old,
  new) in canonical order. Within one shape, it descends only into subtrees whose keys differ.
  Across shapes, it merges the two walks.

Errors: a referenced blob that is absent is `missing(key)`. A node that is not a canonical
encoding is `malformed(key, reason)`. Neither is ever silently skipped.

## 5. Not in this format

Parents, authors, messages and times belong to history records (Stage 5). Ownership, timestamps
and other metadata are left out on purpose, and would be a separate format (principle 4).

## 6. Conformance

`version/tests/conformance.rs` runs against every shape in `SHAPES`, comparing each against a
plain in-memory map. Property tests cover round trips, history independence (scrambled insertion
order, plus paths added and removed again), update semantics including conflicts, prefix walks,
and diffs: the shape's diff, the generic merge diff and the model must all agree. Counting tests
bound the blobs read and written when one file changes in a 2,000-file version. A filesystem test
imports a directory with a nested executable and a symlink, checks out a prefix, and checks that a
full checkout imports back to the same root. Shape-specific unit tests reject each kind of
non-canonical `path/1` node.
