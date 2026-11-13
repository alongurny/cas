# Lean declarations checked as tables

A Lean 4 declaration is accepted when a set of tables, written by an
untrusted prover, passes the local checks below. The tables are a
derivation in a declarative type system: they record only steps that hold,
never a failed attempt, and never claim that a term is in normal form. The
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
committed to all tables (from a hash of the commitment). Two different rows
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
(`mdata`) is removed before the tables are written.

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

`LEq(l, m)` holds when both have the same normal form, given by `LNorm(l,
n)` rows that follow Lean's level normalization (kernel/level.cpp): a
normal form is a sorted list of distinct `succ^k` of `zero`, a parameter or
an `imax` that cannot be resolved, with subsumed entries removed. `imax l
m` resolves to `zero` when m normalizes to `zero`, to `max l m` when m is a
successor, and stays otherwise. Every `param i` has i below the number of
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
- **pi**: `Infer(G, pi A B, sort (imax u v))` from `Infer(G, A, S)`,
  `Eq(G, S, sort u)`, `Inst(B, 0, fvar n, B_o)`,
  `Infer(ctx(G, A), B_o, S')` and `Eq(ctx(G, A), S', sort v)`.
- **let**: `Infer(G, let A v b, T)` from `Infer(G, A, S)`,
  `Eq(G, S, sort u)`, `Infer(G, v, V)`, `Eq(G, V, A)`, `Inst(b, 0, v, b')`
  and `Infer(G, b', T)`.
- **lit**: `lit n` has type `const Nat []`, `str s` has `const String []`;
  the keys of Nat and String are those of the cited prelude.
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
  and a major premise that is a constructor application (with the right
  number of parameters) equals the matching rule's right-hand side,
  instantiated with the levels, the parameters, motives and minors, and the
  constructor's fields. Lean's K-like reduction (a proposition with one
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
- **nat literals**: `lit 0 = const Nat.zero []`; `lit (n + 1) = app
  (const Nat.succ []) (lit n)`; `Nat.add`, `sub`, `mul`, `div`, `mod`,
  `gcd`, `beq`, `ble`, `land`, `lor`, `xor`, `shiftLeft`, `shiftRight`,
  `pow` and `log2` applied to two (one) literals equal the literal result.
  Products are checked by evaluating the limb polynomials at a random point
  with the carries as extra columns; a quotient q and remainder m of a by b
  by `a = q b + m` and `m < b`.
- **string literals**: `str s` equals `String.mk` of the list of
  `Char.ofNat (lit c)` for its characters.

## The declaration's own check

- **axiom, opaque**: the type is a type: `Infer([], T, S)`, `Eq([], S, sort u)`.
- **definition, theorem**: also `Infer([], v, V)` and `Eq([], V, T)`; a
  theorem's type is a proposition (u = zero).
- **mutual definitions**: each one, with the others cited by key.
- **inductive block**: the types are well formed with the declared
  parameters and indices; each constructor's type ends in its inductive type
  applied to the parameters; every recursive occurrence is strictly
  positive; the universe of each constructor argument fits the inductive's
  universe (or the type is a proposition); and the recursors and reduction
  rules stored in the blob are exactly the ones Lean generates, checked term
  by term against their construction. Nested types are first translated to a
  mutual block, as the kernel does, and the translation is stored and
  checked too.
- **quotient**: the four quotient constants with their fixed types.

The claim of a declaration is that its own check passes given the rows of
the declarations it cites, and its axioms are the union of the cited
declarations' axioms, plus itself when it is an axiom.

## Not covered

`native_decide` rests on `Lean.ofReduceBool`, an axiom saying compiled code
computed the right Boolean. It stays an axiom, visible in the claim.
`unsafe` and `partial` declarations are never kernel-checked in Lean and
are not covered.
