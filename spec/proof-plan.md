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

## A limit of flat blobs

BLAKE3 hashes each chunk with its position (the chunk counter). An edit
that keeps the length, such as a changed key in a directory entry, changes
one chunk and the parents above it, so the new blob's proof can reuse the
old tree's other chunk values. An insertion or a removal shifts every later
byte, so every later chunk value changes and nothing after it is reused.
Most directory changes in a history are of that kind (a file added or
removed). For them the gain of #8 is in reading the counterpart (point 2
above), not in reusing its tree. Records may also cross a chunk boundary,
and the sort order is checked between neighbours, so a chunk row checks
the record it starts in and the first key of the next one.

## Order of work

1. Chunk-with-path rows in Flock (#8): a row that opens one 1024-byte chunk
   of a blob whose key is an input, with its parent nodes; and the edited
   blob's proof reusing the old blob's tree (only the changed chunks and
   their parents are hashed).
2. The match rows and the manifest lookup (#1).
3. `cas verify <commit> --manifest-only`: needs the commit key, the
   manifest, the proof and the parents' verification, and reads no blob.

Step 1 is built (2026-11-13): `cas_chunks` in alon/flock, branch `cas`
(ed50983). It opens chunks of a blob against its key, the other subtrees
entering as private chaining values, and proves an edit of the same length
by sharing those values between the old and the new key. Its cost is the
opened chunks' blocks and one parent per level for each side, against
every block of the blob. Steps 2 and 3 (#1) are not started.

## #1 without places (2026-11-13)

Matching "the same place" needs names and merges handled in the circuit.
A weaker condition is enough and is simpler:

- Let V be the blobs the proof opens from the parents: each parent commit
  (its key is in the commit, which is listed), and any blob whose key an
  opened blob of V holds.
- Each key a listed blob holds (not a file, not a parent commit, not none)
  is, with its type, either a record of the manifest or held by a blob of
  V with the same type.

Sound: every blob of V is reachable from a verified parent, so by
induction every key it holds of a proven type is valid. Complete: the
manifest walk leaves a key out only when a parent's counterpart in the
same field holds it, and the prover opens that counterpart chain as V.

In the circuit this is one lookup: requests `(key, type)` from the rows of
the listed blobs, supplies from the manifest's public records and from the
rows of V. A supply answers any number of requests, so it needs the bus
with multiplicities (alon/flock docs/tuple-bus-review-cas.md, finding 2),
which Flock does not have yet. V's blobs are hashed whole (an insertion
shifts every later entry), which is about the cost of the parents' changed
directories again.

Next: multiplicities on Flock's bus (a bit-decomposed multiplicity column,
`∏ (β + φ)^(2^i m_i)` on the supply side), then the request and supply
ports on the typed rows, then `cas verify` from the manifest alone.

## #1 built (2026-11-13)

Flock's bus has multiplicities (alon/flock `cas`, b756764: each count bit
is a product group, proven with a zero check of `m² + m` in one sumcheck).
The manifest now names, besides the new blobs, the parents and the blobs
opened from them (spec/formats/manifest.md, roles `new | opened | parent`).
The circuit adds a table of key rows: one for each blob's own key and each
key a row holds, with the code of its type (the row circuits output it,
from the field's target or its tag byte). A key row asks for its
(key, code) once when it must be found (a key a new blob holds, or an
opened blob's own key) and gives it as often as it is asked for when it
may (a new blob's own key, a key an opened blob or a parent holds). `cas
verify` follows the parents' proofs back to the first commit.
