# Formats given as types

Some formats are not described byte by byte. Each is a type, built from
the parts below, and its parser, its printer, its validity check and the
views it holds all follow from the type. The formats are `directory`,
`commit`, `annotation`, `lines`, `runs` and `release`, with the text formats
`note`, `author` and `tag` (see each in [formats/](formats/)).

They are designed so that a proof of validity is cheap: a blob is a whole
number of 64-byte blocks, the size of one BLAKE3 block; every field sits at
a fixed place in its record; and each value has exactly one encoding.

## Parts

- **key<T>** is 32 bytes: the BLAKE3 digest of a blob that is a valid T.
  The store key of that blob is `blake3:` and the digest in hex.
  **key<T>?** may also be 32 zero bytes, for none.
- **key<A | B | C>** is a key together with a tag that says which of the
  types the blob is. The tag is one byte, 1 for the first type.
- **A | B | C** on its own is a choice among kinds with no content: one tag
  byte, 1 for the first kind. A yes or no is such a choice; there is no
  boolean.
- **integer** is a whole number from 0 up to 2^64, in 8 bytes, most
  significant byte first. **time** is an integer of seconds since
  1970-01-01 00:00 UTC. There is no other number type.
- **name** is 1 to 255 bytes, each an ASCII letter, a digit, `.`, `-`, `_`
  or a space; it is not `.` alone and has no two dots in a row. The list of bytes is short on purpose,
  for now: it is what is needed. It is written as one byte holding its length and then
  its bytes.
- **text** is UTF-8 with no control character. **semver** is a text that is
  a version as Semantic Versioning 2.0.0 writes it. A text is a blob of its
  own, never a field.
- **bytes** is anything.

## Records

A record `{ ... }` is written as

1. its keys, 32 bytes each, in the order the type names them;
2. one tag byte for each key that has a choice of types, in the same order,
   then one tag byte for each choice of kinds;
3. its integers, 8 bytes each;
4. its name, if it has one, which is then its last field;
5. zero bytes up to the next multiple of 64.

A record may end in one list. The list has no count: it is what remains of
the blob.

- **sorted<R> by f** is records of type R one after another, each strictly
  greater in f than the one before. f is the record's name, compared
  bytewise, or its first key, compared bytewise.
- **runs<R> by f** is records of type R one after another, where no record
  has the same f as the one before, and there is at least one record.

## Validity

A blob is valid when it parses as its type and the value prints back to
exactly the same bytes. So unused bytes are zero, there are no trailing
bytes, and there is one blob for each value.

A view of the blob named by each present key, read as the type the key
states, is a child of the blob. A history, a release or a directory is
complete in a store when all of these, followed all the way down, are
there and valid.

## Parameters

A type may take parameters, as `commit<T, A>` does: T is what is being
versioned and A is the type of its notes. The layout does not depend on the
parameters, only the types that the keys must name. The formats here are
the instances for directories: `commit` is `commit<directory, annotation>`
and `release` is `release<directory>`.
