# Lean bundles: a theorem with all its evidence in one file

A **bundle** says what some theorems state and gives evidence for everything they rest on, so that
anyone can check them with nothing but the file. Code: `lean/src/bundle.rs`; the command is
`cas-lean` (`lean-cli/`).

## What it holds

    Bundle {
        theorems: Vec<String>,              // Lean names
        decls:    Vec<String>,              // the closure, one declaration's text each
        units:    Vec<(String, Evidence)>,  // by unit name, one entry per unit
        short:    Vec<u8>,                  // a flock Lean bundle (format 1), or empty
    }
    Evidence = Long(Vec<u8>) | Short | Assumed

**The statement is the declarations.** `decls` holds every declaration the theorems reach through
their types, values, inductive blocks and recursors (and Lean's primitives when a term has a
literal), each as the canonical text `Env::decl_text` writes ([lean-decl](../formats/lean-decl.md)).
Constants are named by their Lean names, so the text says exactly what is proved, including what
every definition used in it means. The **statement digest** is `blake3:` of the bincode encoding of
`decls`; it is what a paper's PDF prints to say which statement it checks.

**Every unit has exactly one piece of evidence.** A unit is what is proved at once (an axiom,
definition, theorem, opaque declaration, or inductive block; [cli.md](cli.md)):

- `Long`: its proof, a [lean-proof](../formats/lean-proof.md) `Proof` in the `unit::encode`
  encoding (the derivation of [derivation.md](derivation.md) and its goal). The verifier runs the
  derivation checker on it, checks the goal is the unit and every cited declaration row against
  `decls` (`unit::check`). This is the long proof: the size of the derivation, checked natively.
- `Short`: the unit is among those the succinct proof in `short` checks (flock
  `docs/lean-bundle.md`). The verifier checks that proof and that it covers the unit.
- `Assumed`: nothing; the verifier lists it. Lean's axioms are units too, so an axiom is either
  checked (its type is a well-formed type) or assumed, and the verifier lists the closure's axioms
  separately in any case.

A bundle can mix the three: a short proof for the bulk of a cone, long proofs for what the circuit
does not cover yet, and an explicit list of what is assumed.

## What verification refuses

- a declaration text that does not parse, or a declaration twice;
- a declaration naming a constant that `decls` does not declare (the closure is not closed);
- an unsafe declaration;
- a theorem of `theorems` that `decls` does not declare;
- a unit with no evidence, two pieces of evidence, or evidence for a name that is not a unit;
- a long proof the checker refuses;
- a succinct proof that does not verify or does not cover a `Short` unit.

## Encoding

The file is the 16 bytes `cas-lean-bundle2` followed by the zstd compression of the bincode 1.3
encoding of `Bundle` (variable-length integers, as `unit::encode`). Format 1 (`cas-lean-bundle1`)
is the flock file of a succinct proof alone.

## How short evidence is tied to the text

The succinct proof names the declarations it checks by keys, hashes of their names. Its statement
also carries facts (flock `docs/lean-bundle.md`, "What the declarations are"): every declaration
the cone uses, with its type, value and recursor rules as terms. The verifier turns `decls` into
derivation rows (`bundle::decl_rows`) and refuses unless every declaration of the facts is one of
the bundle's, the same by structure. A succinct proof in the older format 1 has no facts; the
verifier says that its short units are then bound by name only.
