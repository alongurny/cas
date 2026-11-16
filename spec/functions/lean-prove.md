# Function: lean-prove

The proof of a Lean unit: `lean-prove(X)` for a [lean-decl](../formats/lean-decl.md) blob X that is
a unit (an axiom, definition, theorem or opaque declaration, or the first type of an inductive
block) is a [lean-proof](../formats/lean-proof.md) blob.

Unlike other derived functions, the result depends on more than X: a declaration names other
constants, and which declarations those are is given by the environment
([lean-env](../formats/lean-env.md)) the prover reads X in. The result records the declarations it
was made against. The variable `derived/F/X` holds the newest result; a reader that works in an
environment where one of the recorded declarations differs treats the result as absent, and
`cas lean prove` replaces it. A failure is recorded too (a result with no proof), and is tried
again by the next `cas lean prove`.

A result is not trusted for being stored: `cas lean check` checks it (spec/lean/cli.md).
