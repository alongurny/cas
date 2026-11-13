# Format: manifest

What one proof of validity covers. It is given as a type
([types.md](../types.md)):

    manifest = sorted<{ blob: key<directory | commit | annotation | lines | runs | release | note | author | prose | tag>,
                        role: new | opened | parent,
                        length: integer }> by blob

So a blob is one block for each blob covered, in the order of their keys:
the key, one tag byte that says which type the blob is, one tag byte for
its role, its length in bytes in 8 bytes, and zeros. At least one record is
new, and a parent is a commit or a release.

The roles:

- **new**: a blob the proof brings.
- **opened**: a blob read from a parent's side, so that the keys the new
  blobs share with the parents can be found in it.
- **parent**: a commit or a release the proven node follows. It has a proof
  of its own; this proof does not show it valid, it relies on it.

A proof for a manifest says: each blob listed has that length, hashes to
that key and is valid in that type; and each key that a new blob holds and
that names a blob of a type a proof covers (not a file, not a parent node,
not none) is, with that type, the key of a new blob or a key that an opened
blob or a parent holds in a field of that type; and each opened blob's key
is, with its type, a key that an opened blob or a parent holds. So if the
parents are valid, every blob the new blobs reach is valid: an opened blob
is reached from a parent, and a key a valid blob holds names a valid blob.

The proof's public inputs are exactly the records, so a verifier that holds
the manifest needs nothing else, and a proof is named by the key of its
manifest. A verifier that relies on the proof also checks the parents'
proofs.

Files are not listed: any bytes are a valid file.

The length is a relation between this blob and the blob listed, and so is
the claim of validity: both are what the proof is for. A manifest is valid
on its own when it has this layout.
