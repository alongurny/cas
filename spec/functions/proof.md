# Function: proof

Input: a `manifest`. Output: `raw` bytes, a proof.

The output is a proof, in the system this document's key stands for, of
what [../formats/manifest.md](../formats/manifest.md) says a proof says:
each blob the manifest lists has the length listed, hashes to the key
listed and is valid in the type listed; each key a new blob holds is found
among the new blobs or in an opened blob or parent; and each opened blob is
found in an opened blob or parent.

The system is Flock (R1CS over GF(2), BLAKE3 as the hash), with one table
for each kind of row of each type, generated from the types in
[../types.md](../types.md), and BLAKE3's compression as one more table. A key is found by the tuple bus
with counts: a table of key rows, one for each blob's own key and each key
a blob holds, asks for (key, type) once where it must be found and gives it
as many times as it is asked for where it may be. The
proof bytes are the commitment and the proof as that system serializes
them.

Unlike other functions, two runs need not give the same bytes; any output
that verifies is a result. Checking a result is `verify(manifest, proof)`,
which rebuilds the circuit from the manifest's records and needs no other
input.
