# Format: tree

A directory, as `dir` ([dir.md](dir.md)) describes it: named views of
other blobs, names strictly ascending, each name non-empty, not `.` or
`..`, without `/` or control characters. A `tree` stores the entries in
binary and cuts a big directory into several blobs, so that versions
that differ in a few entries share the rest.

## Blobs

A directory is one `tree` blob (its top), which may name more `tree`
blobs below it. The empty blob is the empty directory. Any other blob
starts with a varint L, its level.

- **A leaf** (L = 0) holds entries: the algorithm table, then the format
  table (a varint count, then that many keys), then a varint count of
  entries and each entry: its name, a varint index into the format table
  (the entry's format), and its key.
- **An inner blob** (L >= 1) holds the blobs of level L - 1: the algorithm
  table, then a varint count of parts and each part: a name (the greatest
  name under that part) and the key of a `tree` blob of level L - 1.

A varint is unsigned LEB128 in the fewest bytes. The algorithm table is a
varint count and, for each algorithm in bytewise order of its name, a
varint length and the name; it lists exactly the algorithms of the keys
in the blob. A key is a varint index into the algorithm table and the
digest, as many bytes as the algorithm's digest has (blake3: 32). The
format table lists exactly the formats of the leaf's entries, in
bytewise order of the key text. A name is a varint count of the bytes it
shares with the previous name of the blob (0 for the first), a varint
count of the bytes that follow, and those bytes (UTF-8 once joined). A
blob has at least one entry or part, and every count is exact: there are
no trailing bytes. So a directory has exactly one encoding.

## Cutting

The **height** of a name is the number of trailing zero bits of the
first 8 bytes of BLAKE3(name), read as a little-endian unsigned integer,
divided by 6 and rounded down.

The entries, in order, are cut into leaves after each name whose height
is greater than 0, the last leaf ending with the greatest name. The
leaves are cut into inner blobs of level 1 after each part whose name
has height greater than 1, and so on, level L cutting after names of
height greater than L, until a level has a single blob: the top. A leaf
holds 64 entries on average and an inner blob 64 parts, and the cut
depends on the names alone, so a directory has one set of blobs however
it was built, and a change to one entry changes one leaf and one blob
on each level above it.

A directory is valid when each blob is (above), each part names a blob
of the level below whose greatest name is the part's name, the names
under each blob are greater than those under the blob before it, within
each blob every name but the last has height at most the blob's level
and the last has a greater height unless the blob is the last of its
level, and a top inner blob has at least two parts.

## Cost

A git.git root directory (about 400 entries) is 85 KB as `dir` text and
about 17 KB as a `tree`, in about seven blobs; a commit that changes one
file rewrites one leaf of it and the top, not the whole directory.
