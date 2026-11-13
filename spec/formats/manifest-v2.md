# Format: manifest

What one proof of validity covers. It is given as a type
([types.md](../types.md)):

    manifest = sorted<{ blob: key<directory | commit | annotation | lines | runs | release | note | author | prose | tag>,
                        length: integer }> by blob

So a blob is one block for each blob covered, in the order of their keys:
the key, one tag byte that says which type the blob is, its length in bytes
in 8 bytes, and zeros. There is at least one record.

A proof for a manifest says: each blob listed has that length, hashes to
that key and is valid in that type. The proof's public inputs are exactly
the records, so a verifier that holds the manifest needs nothing else, and
a proof is named by the key of its manifest.

Files are not listed: any bytes are a valid file.

The length is a relation between this blob and the blob listed, and so is
the claim of validity: both are what the proof is for. A manifest is valid
on its own when it has this layout.
