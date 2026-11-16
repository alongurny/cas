# Format: lean-env

A Lean environment: which declaration each name stands for. A `uni-bin` map with exactly three
entries:

- `decls`: a map from each declaration's name, as Lean prints it, to the key of its
  [lean-decl](lean-decl.md) blob;
- `lean`: the Lean version the declarations were exported with, or the empty string;
- `modules`: the modules exported, separated by spaces, or the empty string.

The declarations named are the blob's children: a store that holds an environment holds them.

Within an environment a constant is its name, so a declaration's meaning is fixed by the environment
it is read in: the same `lean-decl` blob can sit in two environments that give the constants it names
different declarations. Proofs therefore record the declarations they were made against
([lean-proof](lean-proof.md)).
