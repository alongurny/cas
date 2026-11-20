# Lean declarations checked through derivations

A Lean 4 declaration is accepted when a derivation, written by an
untrusted prover, passes the local checks below. The derivation is one in a
declarative type system, stored as tables of rows: it records only steps that hold,
never a failed attempt, and never claims that a term is in normal form. The
same checks are meant to run natively (a small independent checker) and
inside a proof (issue #10). The design is "Extending zkPi efficiently"
(https://claude.ai/artifact/F6rfFCUd6g7bQisYDpZ6NJ).

## Why a derivation is enough

Lean's kernel searches: it tries unfoldings, gives up, caches failures. A
checker needs none of that. If `a` and `b` are definitionally equal, there is
a derivation of `Eq(a, b)` from the rules below, and the prover can write it
down because its own kernel found it. Reducibility hints, normal forms,
failure caches and the order of attempts only steer the search, so they do
not appear.

## Ids and fingerprints

Every term, level, context and list has an id: a random fingerprint of its
fields,

    id(x0, ..., xm) = x0 + x1 r + x2 r^2 + ... + xm r^m

over a field F of size at least 2^192, where the fields are the row's kind,
its numbers and its children's ids. r is drawn after the prover has
committed to the whole derivation (from a hash of the commitment). Two different rows
then get the same id with probability at most m/|F| each, so over n rows at
most n^2 m / |F|, which is negligible. Equal ids are taken as equal terms;
there is no table of distinct terms. Over a 64-bit field such as Goldilocks,
F is an extension of degree 4.

A cited declaration's key (BLAKE3, 32 bytes) enters a fingerprint as four
64-bit limbs. A natural-number literal enters as its limbs, with the count
of limbs first.

## Terms

Terms are locally nameless, as in Lean's kernel: bound variables are
indices (`bvar i`), and variables of the context are de Bruijn levels
(`fvar n`, the n-th entry of the context, from 0).

| Kind | Fields |
| --- | --- |
| `bvar i` | i |
| `fvar n` | n |
| `sort l` | level l |
| `const c ls` | key of c, level list ls |
| `app f a` | f, a |
| `lam A b` | A, b (b under one binder) |
| `pi A B` | A, B (B under one binder) |
| `let A v b` | A, v, b (b under one binder) |
| `lit n` | a natural number |
| `str s` | a string literal |
| `proj S i e` | key of the structure S, i, e |

Binder names and binder information are not part of a term; metadata
(`mdata`) is removed before the derivation is written.

Each term row also carries three numbers, each checked from the row's own
kind and its children's rows:

- `lbr`: one more than the largest loose bound index (0 when closed);
  `bvar i` has i + 1, a binder body counts one less.
- `fv`: one more than the largest `fvar` level (0 when there is none).
- `lp`: whether a level parameter occurs.

## Levels

| Kind | Fields |
| --- | --- |
| `zero` | |
| `succ l` | l |
| `max l m` | l, m |
| `imax l m` | l, m |
| `param i` | i, the position in the declaration's parameter list |

`LEq(l, m)` holds when l and m are equal for every assignment of natural
numbers to the parameters, which is what levels mean in Lean's type theory
(and in Lean4Lean's). The checker decides it by splitting on which
parameters are zero, which removes every `imax`, and comparing the maxima of
`p + k` and constants that remain. Lean's kernel decides only a sufficient
condition (`is_equivalent`: equal normal forms, with parameters ordered by
name), and it can miss an equality such as `max (v+1) (u+1) = imax (v+1)
(u+1)` on a term it never compares; a checker that checks every subterm
meets such terms, so it uses the complete test. Every `param i` has i below the number of
the declaration's level parameters.

## Contexts

A context is a list of types, `ctx(parent, A)` with its length kept in the
row. `Has(G, n, A)` holds when entry n of G is A: directly for the last
entry, otherwise from `Has(parent(G), n, A)`. A context entry's type may
mention only `fvar k` with k below its own position (`fv` < position + 1).

## Substitution

`Inst(e, k, a, e')`: e' is e with `bvar k` replaced by a, and every bound
index above k lowered by one. a has no loose bound variable (`lbr(a) = 0`),
so it is never shifted. One row per distinct (e, k):

- if `lbr(e) <= k`: e' = e;
- `bvar k` gives a; `bvar j` with j > k gives `bvar (j - 1)`;
- `app f x` gives `app f' x'` from `Inst(f, k, a, f')` and `Inst(x, k, a, x')`;
- `lam A b`, `pi A b` give the same kind from `Inst(A, k, a, A')` and
  `Inst(b, k + 1, a, b')`; `let` the same with its value;
- `proj S i x` from `Inst(x, k, a, x')`.

Opening a binder body with a fresh variable is `Inst(b, 0, fvar n, b_o)`
with n the length of the context. Closing it again is the same row read
backwards, with the side condition `fv(B) <= n` on the closed term B, so
that `fvar n` occurs only where `bvar 0` was.

`LInst(e, ps, ls, e')` replaces level parameters by the levels ls, one row
per distinct node with `lp` set, and `LInstL(l, ls, l')` on levels.

## Declarations used

One row per cited constant: its key, kind (axiom, definition, theorem,
opaque, inductive type, constructor, recursor, quotient constant), number of
level parameters, type, and value when one is unfolded. The row is bound to
the cited blob by BLAKE3 over the bytes read, and the cited blob's own proof
says that its type is well formed. A recursor row also carries its
reduction rules, an inductive row its constructors, and a constructor row
its inductive type, parameter count and field count.

## Judgements

`Infer(G, e, T)`: e is well typed in G and has type T. `Eq(G, a, b)`: a and
b are definitionally equal in G. Every premise named below is a row of the
same table, found by lookup.

### Typing

- **sort**: `Infer(G, sort l, sort (succ l))`, the parameters of l in range.
- **fvar**: `Infer(G, fvar n, A)` from `Has(G, n, A)`.
- **const**: `Infer(G, const c ls, T)` from c's row with type T0 and as many
  level parameters as ls has, and `LInst(T0, ls, T)`.
- **app**: `Infer(G, app f a, T)` from `Infer(G, f, F)`, `Eq(G, F, pi A B)`,
  `Infer(G, a, A')`, `Eq(G, A', A)` and `Inst(B, 0, a, T)`.
- **lam**: `Infer(G, lam A b, pi A B)` from `Infer(G, A, S)`,
  `Eq(G, S, sort u)`, `Inst(b, 0, fvar n, b_o)`,
  `Infer(ctx(G, A), b_o, B_o)` and `Inst(B, 0, fvar n, B_o)` with
  `fv(B) <= n`, n the length of G.
- **pi**: `Infer(G, pi A B, sort w)` with w = `mk_imax(u, v)`, Lean's
  simplification of `imax u v` (kernel/level.cpp), as `infer_pi` builds it, from `Infer(G, A, S)`,
  `Eq(G, S, sort u)`, `Inst(B, 0, fvar n, B_o)`,
  `Infer(ctx(G, A), B_o, S')` and `Eq(ctx(G, A), S', sort v)`.
- **let**: `Infer(G, let A v b, T)` from `Infer(G, A, S)`,
  `Eq(G, S, sort u)`, `Infer(G, v, V)`, `Eq(G, V, A)`, `Inst(b, 0, v, b')`
  and `Infer(G, b', T)`.
- **lit**: `lit n` has type `const Nat []`, `str s` has `const String []`;
  the constants Nat and String are found through the list of primitives.
- **proj**: `Infer(G, proj S i e, T)` from `Infer(G, e, E)`, `Eq(G, E, I ps)`
  with I the structure S (an inductive type with one constructor, applied to
  its parameters and indices), `Infer(G, E, SE)` and `Eq(G, SE, sort l)`,
  the constructor's type at the levels (`LInst`), then for each parameter an
  `Eq` to a pi and an `Inst` with the parameter, for fields 0 to i - 1 an
  `Eq` to a pi and an `Inst` with `proj S j e`, and last an `Eq` to a pi
  whose domain is T. When l is equivalent to zero (S is a proposition), T
  and the type of every earlier field that a later one depends on must be
  propositions: `Infer(G, A, S')` and `Eq(G, S', sort zero)`, as Lean's
  kernel requires.

### Equality

- **refl**: `Eq(G, a, a)`. **symm**, **trans** as usual.
- **beta**: `Eq(G, app (lam A b) a, b')` from `Inst(b, 0, a, b')`.
- **delta**: `Eq(G, const c ls, v')` from c's value v and `LInst(v, ls, v')`.
- **zeta**: `Eq(G, let A v b, b')` from `Inst(b, 0, v, b')`.
- **congruence**: `app`, `lam`, `pi`, `let`, `proj` from equal parts (binder
  bodies compared after opening with the same `fvar n`); `sort l = sort m`
  from `LEq(l, m)`; `const c ls = const c ms` from `LEq` of each level.
- **iota**: a recursor applied to its parameters, motives, minors, indices
  and a major premise that is a constructor application (its parameters
  and fields, the fields last) equals the matching rule's right-hand side,
  instantiated with the levels, the parameters, motives and minors, and the
  constructor's fields. The constructor's parameters need not be the
  recursor's: a nested block's recursor `T.rec_1` has a rule for
  `Array.mk`, applied to `T`. Lean too takes only the last fields. Lean's K-like reduction (a proposition with one
  constructor without fields) and its reduction on a structure that is not
  built need no rule of their own: the major premise is first shown equal
  to a constructor application by proof irrelevance or by eta for
  structures, and congruence carries that into the recursor's arguments.
- **proj-ctor**: `proj S i (ctor ps fs)` equals field i.
- **quot**: `Quot.lift f h (Quot.mk r a)` equals `f a`, and
  `Quot.ind h (Quot.mk r a)` equals `h a`, with the levels and arguments of
  the cited quotient constants (rows of kind quot, marked type, mk, lift
  or ind; the arguments after the quotient are kept).
- **eta**: `Eq(G, lam A (app f (bvar 0)), f)` when `lbr(f) = 0`, from
  `Infer(G, f, F)` and `Eq(G, F, pi A' B)` with `Eq(G, A, A')`.
- **eta-struct**: `Eq(G, e, ctor ps fs)` from `Infer(G, e, E)`,
  `Infer(G, ctor ps fs, C)`, `Eq(G, E, C)` and `Eq(G, proj S i e, f_i)` for
  each field, when S is structure-like (one constructor, no indices, not
  recursive, as Lean's kernel requires).
- **proof irrelevance**: `Eq(G, p, q)` from `Infer(G, p, P)`,
  `Infer(G, q, Q)`, `Eq(G, P, Q)`, `Infer(G, P, S)` and `Eq(G, S, sort zero)`.
- **unit-like**: `Eq(G, a, b)` from `Infer(G, a, A)`, `Eq(G, A, I ps)`,
  `Infer(G, b, B)` and `Eq(G, I ps, B)`, when I is structure-like and its
  constructor has no fields.
- **nat literals**: `Eq(G, const Nat.zero [], lit 0)` (rule nat-zero) and
  `Eq(G, app (const Nat.succ []) (lit n), lit (n + 1))` (nat-succ); with
  symm these also write a literal as a constructor, which is how a
  recursor reduces on a literal. `Nat.add`, `sub`, `mul`, `div`, `mod`,
  `gcd`, `beq`, `ble`, `land`, `lor`, `xor`, `shiftLeft`, `shiftRight` and
  `pow` applied to two literals equal the result (nat-bin): a literal, or
  `Bool.true`/`Bool.false` for `beq` and `ble`. These are exactly the
  operations of Lean's `reduce_nat`, with its semantics: truncated
  subtraction, `x / 0 = 0`, `x % 0 = x`, and `pow` only for exponents up
  to 2^24. The native checker computes the result; a circuit checks
  products by evaluating the limb polynomials at a random point with the
  carries as extra columns, and a quotient q and remainder m of a by b by
  `a = q b + m` and `m < b`.
- **string literals**: `Eq(G, str s, String.mk l)` (str-lit) where l is
  `List.cons.{0} Char (Char.ofNat (lit c)) ...` ending in
  `List.nil.{0} Char`, one cons for each character of s (s is UTF-8).
- The constants these rules name (`Nat`, `Nat.zero`, `Nat.add`, `Bool.true`,
  `String.mk`, `List.cons` and the others) are found through the derivation's
  list of primitives, which pairs each with a cited declaration row. That
  these rows are the real constants, by key, is the caller's part, like
  the kinds and inductive information of all cited rows.

## The declaration's own check

- **axiom, opaque**: the type is a type: `Infer([], T, S)`, `Eq([], S, sort u)`.
- **definition, theorem**: also `Infer([], v, V)` and `Eq([], V, T)`; a
  theorem's type is a proposition (u = zero).
- **mutual definitions**: each one, with the others cited by key.
- **inductive block**: a block goal names the types (rows of kind
  inductive, each listing its constructors), the recursors, the number of
  parameters and level parameters, and a tape of judgement rows. The
  checker runs Lean's `add_inductive` (kernel/inductive.cpp) step by step
  and answers each `whnf`, `infer`, `instantiate` and `is_def_eq` it makes
  with the next row of the tape, which must be about the term it asks
  for (each row's own check is that of any row). So it checks, as Lean
  does: the types' telescopes after whnf, shared parameters, one universe
  for all types; each constructor's parameters, that each field's universe
  is at most the block's (`is_geq`) unless the block is a proposition,
  strict positivity, and that it returns its type applied to the
  parameters and to indices free of the block; whether the block is
  recursive; the elimination universe (Prop only, or a new level
  parameter placed first, which moves the block's parameters up by one);
  and the recursive fields and their arguments. It then builds each
  recursor's type and rules as `declare_recursors` does, computing ids of
  terms with variables bound directly (no rows needed), and compares them
  with the recursor rows given. K-like reduction needs no flag, since it
  is derived from proof irrelevance. An inductive row also lists the
  types of its block (Lean's `all`), which the check compares.
  As Lean's `mk_local_decl_for` does, every binder type of the block
  (parameters, indices and fields) is taken with its annotations consumed
  (`optParam T d`, `autoParam T s`, `outParam T` and `semiOutParam T`
  become T), so a class `Coe (α : semiOutParam (Sort u))` has the
  recursor `Coe.rec {α : Sort u}`. Each time a type changes, the tape
  proves the two equal, so nothing trusts those names.
- **nested inductive block**: a block whose constructors use one of its
  types inside another inductive type's parameters, as in
  `Tree | node : List Tree -> Tree`. As Lean's kernel does, the checker
  rewrites it into a mutual block (`elim_nested_inductive`): each nested
  occurrence `I Ds`, met outermost first, becomes an auxiliary type
  applied to the block's parameters, with copies of the constructors of
  `I` (and of every type of `I`'s block) at Ds, themselves rewritten in
  turn. The rewriting is syntactic, so the checker does it on terms with
  local constants and compares the ids of the results with the rows of
  the mutual block given; a nested parameter that mentions a field is
  refused. In the mutual block the block's own types and constructors
  appear under keys derived from theirs (`blake3` derive-key
  "cas-lean nested copy"), and the auxiliary types under keys derived
  from the first type's key and their position, since only their
  constructors' types differ. The mutual block is checked as any block.
  Then the block's own constructors, information and recursors must be
  the mutual block's restored (`restore_nested`): auxiliary types and
  constructors replaced by the nested occurrences and their constructors,
  copies by the originals, and the recursors renamed (the block's own,
  then `rec_1`, `rec_2`, ... for the auxiliary types, whose rules name
  the constructors of the nested type, such as `List.cons`).
- **quotient**: the four quotient constants with their fixed types.

The claim of a declaration is that its own check passes given the rows of
the declarations it cites, and its axioms are the union of the cited
declarations' axioms, plus itself when it is an axiom.

## Soundness

What acceptance means, rule by rule, against two known references: Lean's
kernel (`src/kernel/type_checker.cpp` and `level.cpp` at Lean v4.25.0) and
the typing judgement of Lean4Lean (`Lean4Lean/Theory/Typing/Basic.lean`,
commit 67f63b0, `VEnv.IsDefEq`), which formalizes Lean's type theory.

**Reading.** `Infer(G, e, T)` stands for `G ⊢ e : T` and `Eq(G, a, b)` for
`G ⊢ a ≡ b : A` for some `A` (Lean4Lean's `IsDefEqU`). Lean4Lean's
judgement is typed; `Eq` here is not, like the kernel's `is_def_eq`, which
also compares two terms without a type. So the claim is the kernel's own:
every rule is a step the kernel takes, and an accepted derivation is a run
of those steps with the search left out. What the kernel needs to be sound
(that `is_def_eq` on two well-typed terms only relates terms equal in the
theory, which rests on unique typing and subject reduction), this checker
needs too, and no more. Lean4Lean proves unique typing up to one `sorry`
(`Theory/Typing/UniqueTyping.lean`) and does not yet prove the kernel's
`is_def_eq` sound, so neither this checker nor Lean's kernel has a finished
proof; both rest on the same open step.

**Typing rules**, each the kernel function it follows and the Lean4Lean
rule it gives:

| rule | kernel | Lean4Lean |
|---|---|---|
| sort | `infer_type_core`, Sort case (l. 289), with `check_level` (l. 76) | `sortDF` |
| fvar | `infer_fvar` (l. 84) | `bvar` (locally nameless: an fvar is a context entry) |
| const | `infer_constant` (l. 92): level count, `check_level`, `instantiate_type_lparams` | `constDF` |
| app | `infer_app` (l. 163): function type made a pi, argument type checked | `appDF` with `defeqDF` |
| lam | `infer_lambda` (l. 116) | `lamDF` |
| pi | `infer_pi` (l. 134), universe `mk_imax` (`level.cpp` l. 112) | `forallEDF` (the `mk_imax` form is `≈` to `imax u v`) |
| let | `infer_let` (l. 198) | none: translated away |
| lit | `infer_type_core`, Lit case (`lit_type`) | none: translated away (see below) |
| proj | `infer_proj` (l. 221), including its proposition checks | none: translated away |

**Equality rules**:

| rule | kernel | Lean4Lean |
|---|---|---|
| refl, symm, trans | `is_def_eq_core` (l. 1047) is an equivalence | `symm`, `trans`, typing as refl |
| beta, zeta | `whnf_core` (l. 401), App and Let cases | `beta` |
| delta | `lazy_delta_reduction_step` (l. 875) | `extra` (a definition's unfolding is an environment defeq) |
| congruence | `is_def_eq_app` (l. 806), `is_def_eq_binding` (l. 681), `lazy_delta_proj_reduction` (l. 999), `is_def_eq` on levels (l. 710) | `appDF`, `lamDF`, `forallEDF`, `constDF`, `sortDF` |
| iota, quot | `reduce_recursor` (l. 333): `quot_reduce_rec`, `inductive_reduce_rec` | `extra` |
| proj-ctor | `reduce_proj` (l. 377) | `extra` (iota), after translation |
| eta | `try_eta_expansion_core` (l. 769) | `eta` |
| eta-struct | `try_eta_struct_core` (l. 784) | `extra` |
| proof irrelevance | `is_def_eq_proof_irrel` (l. 827) | `proofIrrel` |
| unit-like | `is_def_eq_unit_like` (l. 1035) | `extra` |
| nat literals | `reduce_nat` (l. 600), `nat_lit_to_constructor` (`inductive.cpp` l. 1191) | `extra` (iota), after translation |
| string literals | `string_lit_to_constructor` (`inductive.cpp` l. 1200) | after translation |

Lean4Lean's terms (`VExpr`) have no `let`, literals or projections: a
`let` is its body with the value put in, a literal is the constructor
application it stands for, and a projection is the structure's recursor
applied to it; Lean4Lean's `extra` is the environment's list of
definitional equalities, among them each recursor's rules and the
quotient's. The rules for these three kinds of term are the kernel's, and
that they agree with the translated terms is the translation's part.

**Where this checker accepts more than the kernel.** Two places, both
sound in the theory:

- Level equivalence is decided by value (equal under every assignment of
  the parameters), which is Lean4Lean's `≈` (`VLevel.Equiv`). The kernel's
  `is_equivalent` (`level.cpp` l. 503) normalizes and can miss an
  equivalence, so the kernel accepts less.
- A derivation's steps need not be the ones the kernel's search would take,
  and may pass through terms the kernel never builds. Each step is still
  one of the rules above.

**Level parameters in range.** The kernel checks them in `check_level`
(sort and constant levels); the native checker does too
(`params_in_range`, `lean/src/check.rs`). The circuit does not, and need
not: the declarations themselves are read from their text, which refuses
an undeclared parameter (`Env::level`), so the statement's types and
values have every parameter in range. A parameter out of range can then
occur only inside the derivation, and Lean4Lean's `IsDefEq.instL`
(`Theory/Typing/Lemmas.lean`, proved) turns a derivation into one at any
levels in range by replacing each such parameter by `zero`, which leaves
the statement's terms unchanged.

**What the circuit adds.** The circuit checks the same rules as the native
checker (the rule census of `lean_tower rules` lists none assumed on the
shuffle paper's cone). That the two agree rule by rule rests on the code
and on refusal tests (`flock-prover/tests-data`), not on a proof.

## Not covered

`native_decide` rests on `Lean.ofReduceBool`, an axiom saying compiled code
computed the right Boolean. It stays an axiom, visible in the claim.
`unsafe` and `partial` declarations are never kernel-checked in Lean and
are not covered.
