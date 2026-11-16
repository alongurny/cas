# Format: lean-proof

What `cas lean prove` stores for one unit: a declaration (axiom, definition, theorem or opaque
declaration), or an inductive block (its types, constructors and recursors), named by its first
type. The proof is a derivation (spec/lean/derivation.md) and its goal.

The bytes are the bincode 1.3 encoding, with its default options (integers as variable-length
little-endian, sequences with a length first, enumerations by variant number), of the `Record` of
`lean/src/unit.rs`, field by field in order:

- `summary`: the unit's name, its status (`ok`, or the stage that failed: `import`, `cite`, `prover`,
  `budget`, `panic`), the numbers of judgement rows and of term rows, and the milliseconds the prover
  took;
- `against`: the declarations the proof was made against, as pairs of a name and the key of its
  [lean-decl](lean-decl.md) blob, in name order: the unit's own declarations and every constant the
  derivation cites. These blobs are the proof's children;
- `outcome`: the proof (the goal, then the derivation's tables), or the failure's message.

A blob is valid when decoding it and encoding it back gives the same bytes, the names are in order,
the keys are blob keys, and the status is `ok` exactly when there is a proof. Validity says nothing
about the proof: `cas lean check` checks it (spec/lean/cli.md).
