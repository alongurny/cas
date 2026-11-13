# Proofs: what is left, and the plan

Issues #1 (a manifest is the one its commit gives) and #8 (a changed chunk
proven against the old blob) need the same two pieces, so they are planned
together. #9's review of Flock's tuple bus (alon/flock,
docs/tuple-bus-review-cas.md) says which of them the bus can carry today.

## What a proof says today

`proof(m)` says: each blob `m` lists has its length, its key, and is valid
in its type. `cas verify` of a commit works `m` out again from the commit,
so it reads the blobs; a verifier that only holds a commit key, `m` and the
proof must trust that `m` is `manifest(commit)`.

## What is enough

A verifier does not need `m` to be exactly `manifest(c)`. It needs
*closure*: every key that a listed blob holds (of a proven type; not a
file, not a parent commit) is either listed in `m`, or is the key in the
same field of the same place in a parent. With closure, and the parents
verified the same way, every non-file blob the history reaches is valid, by
induction from the first commit. And `m` must list `c` itself as a
`commit`, which the verifier checks by reading `m`.

## The circuit

Closure is a relation between a listed blob and its counterpart in a parent
(the blob the manifest walk compared it with). The proof gets, privately,
for each listed blob that has a counterpart:

1. **Which counterpart.** The path of names from the parent commit down to
   it. Its key is the key at that path in the parent; the parent commit's
   key is public (it is in `c`, which is listed).
2. **Only the chunks that are read.** A directory that changed in one entry
   shares all other entries with its counterpart. The circuit binds the
   counterpart by BLAKE3 chunk paths (#8): the 1024-byte chunk that holds
   the entry, and the parent nodes up to the counterpart's key. That is
   the lazy-input design measured on 2026-11-11 (about 15 times fewer
   compressions on directories than hashing whole blobs).
3. **The match.** For each key in a listed blob: either a row that says
   "listed" (a lookup into the manifest's records, which are public), or a
   row that says "same as the counterpart's entry of the same name" (a
   lookup into the counterpart's opened chunk).

Both lookups are one-to-one or of a count known from the manifest (each
key of a listed blob is matched once), so the bus works as it is, with
table ids and blob indices in coordinates of their own (review, finding 1).

## Order of work

1. Chunk-with-path rows in Flock (#8): a row that opens one 1024-byte chunk
   of a blob whose key is an input, with its parent nodes; and the edited
   blob's proof reusing the old blob's tree (only the changed chunks and
   their parents are hashed).
2. The match rows and the manifest lookup (#1).
3. `cas verify <commit> --manifest-only`: needs the commit key, the
   manifest, the proof and the parents' verification, and reads no blob.

Not started as of 2026-11-13. Both are Flock circuit work (alon/flock,
branch `cas`).
