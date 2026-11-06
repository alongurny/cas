# Version format (Stage 4)

A version is a tree of directories and files stored as ordinary core blobs. The core knows nothing
of it: this is tooling (crate `cas-version`).

## 1. References and formats

Blobs carry no headers saying what they are. Whoever points at a blob also says how to read it. A
**reference** is a pair

    <format> <key>

of two blob keys: `<key>` holds the bytes, and `<format>` is the id of the format they are in.

A **format** is an object in the single metaformat ([metaformat.md](metaformat.md)). It holds a
name for people, the media type of its spec, and the blob key of the spec document. A format's id
is the key of its format object, so formats are content addressed like everything else. Changing a
spec gives a new format with a new id. Any store can serve and verify a format's description, and
`formats::publish` writes the metaformat, the specs and the format objects. Names like `dir` are
local aliases in tooling; references always carry ids.

A format may have a header and a body inside its own bytes (`git-blob` does). That is the
format's business and not part of this layer.

Formats nest: a directory format holds references, each naming its child's format. This crate
knows these formats, each specified in [formats/](formats):

| alias      | kind      | bytes                                                       |
|------------|-----------|-------------------------------------------------------------|
| `dir`      | directory | lines of `<format> <key> <name>`, names strictly ascending   |
| `raw`      | file      | the contents                                                |
| `exec`     | file      | the contents of an executable                               |
| `symlink`  | file      | the link target, non-empty, no NUL                          |
| `git-blob` | file      | `blob <n>\0<contents>`, so its sha1 key is git's object id  |

More formats are just more entries. A reference in a format this crate does not know is an
opaque file: it is walked, diffed and stored, but not opened or checked out.

## 2. Versions

A **version** is a reference to its root directory, written as `<format> <key>`, as histories and
the store's names hold it. It maps paths to nodes. A node is a directory or a file (a reference
in a non-directory format). Every parent of a path is a directory, and **empty directories are
kept**.

A **name** is a non-empty UTF-8 string that is not `.` or `..` and contains no `/` and no control
character. A **path** is one or more names joined by `/`. **Canonical order** compares paths name
by name, each name bytewise. So `a/z` sorts before `a-b`, and a directory is followed directly by
what is under it.

Rules every directory format must meet:

- **History independence.** The same tree, written with the same hash algorithm and the same
  directory formats, gives the same root, whatever edits produced it.
- **Canonical decoding.** Each directory has exactly one valid encoding; readers reject any other.
- **Cost follows the change.** Lookup, update, prefix walk and diff touch only the directories on
  the paths involved, never the whole tree.

Directories in one version may use different directory formats, such as a prolly-tree format for
a huge directory. Each operation reads a directory through its own format.

## 3. Operations

- **build(format, map)** writes a version holding `map`, with every directory in `format`.
  Missing parent directories are created.
- **update(root, edits)** applies a batch: all removes, then all sets in canonical order.
  - `remove p` removes `p` and everything under it; an absent path is fine.
  - `set p <file>` replaces whatever is at `p`, including a whole directory.
  - `set p dir` keeps a directory already at `p`, and otherwise puts an empty one there.
  - Missing parents are created as directories. A set below a file is a **path conflict**, and the
    version is left unchanged.
  - Removing a directory's last child leaves the directory. New directories take their parent's
    format. A directory that comes out unchanged is not rewritten.
- **lookup(root, p)** returns the node at `p`, or absent.
- **walk(root, prefix)** yields the nodes at and under `prefix` (all of them when there is no
  prefix) in canonical order. It reads only the directories it returns. This is sparse checkout.
- **diff(a, b, prefix)** yields every path at or under `prefix` whose node differs, as (path, old,
  new) in canonical order. It descends only into directories whose references differ. A path
  that changes between file and directory comes first, followed by everything under the
  directory side.
- **import / checkout** move a version to and from a local directory. Import writes `raw`, `exec`
  and `symlink`, and keeps empty directories. Checkout also reads `git-blob`.

Errors: a referenced blob that is absent is `missing(key)`. Bytes that are invalid in their format
are `malformed(key, reason)`. A root, or a file being checked out, in an unknown format is
`unknown-format`. None of them is skipped silently.

## 4. Not in this format

Order, changelogs, compatibility, tags and docs belong to histories (spec/history.md); versions
have no parents. Ownership, timestamps
and ACLs are left out, and would be formats of their own.

## 5. Conformance

`version/tests/conformance.rs` compares every operation with a plain in-memory map, as property
tests:

- round trips through build, walk and lookup;
- history independence: scrambled insertion order, plus paths added and removed again;
- update semantics, including conflicts, against a model of removes-then-sets;
- prefix walks;
- both diffs (subtree-skipping and merge) against the model.

Counting tests bound the blobs touched when one file changes in a 2,000-file version. Other tests
cover:

- empty directories;
- canonical `dir` decoding;
- format objects matching the metaformat;
- unknown formats acting as opaque files;
- `git-blob` keys equalling git's object ids;
- import, sparse checkout, and a checkout importing back to the same root.
