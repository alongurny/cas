# Function: proof

Input: a `manifest`. Output: `raw` bytes, a proof.

The output is a proof, in the system this document's key stands for, that
each blob the manifest lists has the length listed, hashes to the key
listed and is valid in the type listed.

The system is Flock (R1CS over GF(2), BLAKE3 as the hash), with one table
for each kind of row of each type, generated from the types in
[../types.md](../types.md), and BLAKE3's compression as one more table. The
proof bytes are the commitment and the proof as that system serializes
them.

Unlike other functions, two runs need not give the same bytes; any output
that verifies is a result. Checking a result is `verify(manifest, proof)`,
which rebuilds the circuit from the manifest's records and needs no other
input.
