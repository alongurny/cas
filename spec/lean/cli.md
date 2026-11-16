# `cas lean`: Lean declarations and their proofs in the store

`cas lean` keeps Lean declarations in a cas store, proves them (writes the derivation of
[derivation.md](derivation.md) for each), stores the proofs, and checks them. Nothing is kept
outside the store, so declarations and proofs are synced, served and collected like everything else.

## What is stored

- Each declaration is a [lean-decl](../formats/lean-decl.md) blob: its own canonical text, naming
  other constants by name.
- An environment is a [lean-env](../formats/lean-env.md) blob: the key of each declaration by name.
  The variable `lean/<env>` (default `lean/default`) holds a view of it.
- The proof of a unit is a [lean-proof](../formats/lean-proof.md) blob, the result of the function
  [lean-prove](../functions/lean-prove.md) over the unit's declaration: the variable `derived/F/X`
  holds a view of it, F being the function's id and X the declaration's key.

A **unit** is what is proved at once: an axiom, definition, theorem or opaque declaration; or an
inductive block (its types, constructors and recursors), kept under its first type. The four
quotient constants are Lean's builtins and have no proof. Unsafe declarations have none either, and
a proof that cites one is refused.

## Commands

    cas lean import <export-file>
    cas lean import --root <lake-dir> <module>...
    cas lean prove <name>... [--all] [--closure] [--budget <rows>]
    cas lean check <name>... [--all] [--closure] [--quiet]
    cas lean show <name>
    cas lean axioms <name>...
    cas lean report [--failed] [--slowest <n>]

Every command takes `--env <name>`. A declaration is named by its Lean name or by the key of its
blob. `-j` sets the number of threads.

**import** reads an export of `lean/measure/Export.lean`, or with `--root` runs that exporter in
the Lake project (`lake env lean --run`), so with the project's own Lean. It writes each
declaration's blob and the environment, and moves `lean/<env>` to it. A declaration that did not
change has the same text, so the same key, and is not written again.

**prove** proves the units of the named declarations; with `--closure`, also every unit they rest
on; with `--all`, every unit of the environment. It reads the named declarations and whatever they
reach: the constants their terms name, a block's constructors and recursors, and Lean's primitive
constants when a term has a literal. A unit whose stored proof was made against the declarations
the environment has now is kept. So after an edit and a new import, only the units that changed, and
those whose proofs cite something that changed, are proved again. A failure (for instance the row
budget) is stored as a result with no proof and tried again next time. One line per unit: name,
declaration key, status, rows, milliseconds.

**check** is the verifier. For each unit it

1. reads the stored proof, and refuses if there is none or it was made against other declarations;
2. runs the derivation checker on it (every row against its rule);
3. checks that the goal is this unit: the declaration's kind, level count, type and value, or the
   block's types and recursors;
4. checks every declaration row of the derivation against the environment's declaration of that
   name: kind, level count, type, value where the proof unfolds it, and information (for a recursor,
   its rules). This is the citation check: the derivation checker takes cited rows as given;
5. checks that each row given as a primitive (`Nat.add` and the others) is the constant of that name.

Terms are compared by a BLAKE3 digest of their structure, computed the same way from a
derivation's rows and from a declaration's text.

Without `--closure`, the cited declarations are compared but their own proofs are not checked.
With `--closure` or `--all`, every unit reached is checked, so "accepted" means accepted all the way
down to axioms and the quotient builtins. It exits with a failure when anything is refused, and
prints the reason for each.

**show** prints one declaration: key, kind, level count, type, the unit that proves it, its stored
proof, and the constants it names with their keys. **axioms** lists the axioms the named
declarations rest on, like Lean's `#print axioms`. **report** sums up the stored results of the
environment: counts by status, rows (median, 99th percentile, largest, total), prover time, and on
request the slowest units and the failures.

## What a check establishes

If `cas lean check --closure <name>` accepts, then Lean's kernel accepts each unit reached, read in
this environment, as far as the derivation rules are sound (derivation.md) and up to the collision
resistance of BLAKE3 and the checker's random fingerprints. What it rests on unproved is exactly
what `cas lean axioms` lists, plus the quotient builtins.

It does not establish that the environment is what Lean elaborated from the sources: the exporter
runs outside the check.
