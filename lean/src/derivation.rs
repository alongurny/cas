//! The derivation a prover writes, as tables of rows (spec/lean/derivation.md). Rows refer to other
//! rows by their position, and only to rows before them, so every table is
//! acyclic by construction. Equality of two terms is equality of their ids,
//! which the checker computes after drawing the random point.

/// A position in a table.
pub type Ix = u32;

/// A BLAKE3 key of a cited declaration.
pub type Key = [u8; 32];

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LevelRow {
    Zero,
    Succ(Ix),
    Max(Ix, Ix),
    IMax(Ix, Ix),
    /// A level parameter of the declaration being checked, by position.
    Param(u32),
}

/// A list of levels: `Nil` or a level and the rest.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ListRow {
    Nil,
    Cons(Ix, Ix),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TermRow {
    BVar(u32),
    FVar(u32),
    /// A level row.
    Sort(Ix),
    /// A declaration row and a level list row.
    Const(Ix, Ix),
    App(Ix, Ix),
    Lam(Ix, Ix),
    Pi(Ix, Ix),
    Let(Ix, Ix, Ix),
    /// A natural number, as little-endian bytes without trailing zeros.
    Lit(Vec<u8>),
    Str(Vec<u8>),
    /// Field `i` of `e`, whose type is the structure declared in the row.
    Proj(Ix, u32, Ix),
}

/// A context: empty, or a context with one more entry of this type.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum CtxRow {
    Empty,
    Push(Ix, Ix),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum DeclKind {
    Axiom,
    Definition,
    Theorem,
    Opaque,
    /// An inductive type, a constructor or a recursor: introduced by an
    /// inductive block, whose own proof checks them.
    Inductive,
    Constructor,
    Recursor,
    /// One of the four constants of quotients (`Quot`, `Quot.mk`,
    /// `Quot.lift`, `Quot.ind`), which Lean's kernel adds with fixed types.
    Quot,
}

/// What a constructor or recursor row adds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Info {
    #[default]
    None,
    /// An inductive type: its parameters, indices and constructors,
    /// whether it is recursive, and the types of its block (Lean's `all`).
    Induct { params: u32, indices: u32, ctors: Vec<Key>, recursive: bool, all: Vec<Key> },
    /// A constructor of the inductive type `induct`.
    Ctor { induct: Key, params: u32, fields: u32 },
    /// A recursor: how many parameters, motives, minor premises and indices
    /// precede the major premise, and one reduction rule per constructor.
    Rec { params: u32, motives: u32, minors: u32, indices: u32, rules: Vec<RecRule> },
    /// Which quotient constant.
    Quot(QuotKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum QuotKind {
    Type,
    Mk,
    Lift,
    Ind,
}

/// The right-hand side `rhs` (closed over parameters, motives, minors and
/// then the constructor's fields) when the major premise is built by `ctor`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RecRule {
    pub ctor: Key,
    pub fields: u32,
    pub rhs: Ix,
}

/// A cited declaration: its key, kind, number of level parameters, type,
/// and its value when one is unfolded. Its own proof is what makes its type
/// trustworthy; binding these rows to its blob is the caller's part.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DeclRow {
    pub key: Key,
    pub kind: DeclKind,
    pub levels: u32,
    pub ty: Ix,
    pub value: Option<Ix>,
    pub info: Info,
}

/// What a judgement row claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Fact {
    /// Entry `n` of context `g` has type `a`.
    Has { g: Ix, n: u32, a: Ix },
    /// `e` is well typed in `g`, with type `t`.
    Infer { g: Ix, e: Ix, t: Ix },
    /// `a` and `b` are definitionally equal in `g`.
    Eq { g: Ix, a: Ix, b: Ix },
    /// `r` is `e` with `bvar k` replaced by `a`.
    Inst { e: Ix, k: u32, a: Ix, r: Ix },
    /// `r` is term `e` with level parameters replaced by the list `ls`.
    LInst { e: Ix, ls: Ix, r: Ix },
    /// `r` is level `l` with level parameters replaced by the list `ls`.
    LInstL { l: Ix, ls: Ix, r: Ix },
    /// Levels `l` and `m` are equivalent.
    LEq { l: Ix, m: Ix },
}

/// The rule a judgement row uses. Its premises are other judgement rows,
/// earlier in the table, named in the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Rule {
    HasLast,
    HasUp,
    Sort,
    FVar,
    Const,
    App,
    Lam,
    Pi,
    Let,
    LitNat,
    LitStr,
    Proj,
    Refl,
    Symm,
    Trans,
    Beta,
    Delta,
    Zeta,
    Iota,
    ProjCtor,
    QuotLift,
    NatZero,
    NatSucc,
    NatBin,
    StrLit,
    CongProj,
    EtaStruct,
    UnitLike,
    InstProj,
    LInstProj,
    CongApp,
    CongLam,
    CongPi,
    CongLet,
    CongSort,
    CongConst,
    Eta,
    ProofIrrel,
    InstClosed,
    InstHit,
    InstLower,
    InstApp,
    InstBind,
    InstLet,
    LInstNone,
    LInstSort,
    LInstConst,
    LInstApp,
    LInstBind,
    LInstLet,
    LInstLZero,
    LInstLSucc,
    LInstLMax,
    LInstLIMax,
    LInstLParam,
    LEqNorm,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Row {
    pub fact: Fact,
    pub rule: Rule,
    pub premises: Vec<Ix>,
}

/// What is being checked: a declaration of this kind, with this many level
/// parameters, this type and (for a definition or theorem) this value. The
/// numbers name the judgement rows that establish it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Goal {
    pub kind: DeclKind,
    pub levels: u32,
    pub ty: Ix,
    pub value: Option<Ix>,
    /// `Infer([], ty, s)` and `Eq([], s, sort u)`.
    pub ty_is_type: (Ix, Ix),
    /// For a value: `Infer([], value, t)` and `Eq([], t, ty)`.
    pub value_has_ty: Option<(Ix, Ix)>,
    /// For a theorem: `LEq(u, zero)`.
    pub is_prop: Option<Ix>,
}

/// The derivation of one declaration's check: all its tables.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Derivation {
    pub levels: Vec<LevelRow>,
    pub lists: Vec<ListRow>,
    pub terms: Vec<TermRow>,
    pub ctxs: Vec<CtxRow>,
    pub decls: Vec<DeclRow>,
    pub rows: Vec<Row>,
    /// The declaration rows of the constants Lean's kernel knows by name.
    /// Like a row's kind and info, that these are the real `Nat.add` and the
    /// others is the caller's part to check.
    pub prims: Vec<(Prim, Ix)>,
}

impl Derivation {
    /// A commitment to the derivation, from which the random point is drawn.
    pub fn commitment(&self) -> [u8; 32] {
        // Any injective encoding will do natively; Debug output is one.
        *blake3::hash(format!("{self:?}").as_bytes()).as_bytes()
    }
}

/// The constants Lean's kernel computes with: what literals mean, and the
/// operations on two literals that it evaluates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Prim {
    Nat,
    NatZero,
    NatSucc,
    NatAdd,
    NatSub,
    NatMul,
    NatPow,
    NatGcd,
    NatMod,
    NatDiv,
    NatBeq,
    NatBle,
    NatLand,
    NatLor,
    NatXor,
    NatShiftLeft,
    NatShiftRight,
    BoolTrue,
    BoolFalse,
    String,
    StringMk,
    Char,
    CharOfNat,
    ListNil,
    ListCons,
}

/// What an inductive block's check establishes: its inductive types (each
/// with its constructors, named in its info) and its recursors are what
/// Lean's kernel adds for the block (inductive.cpp, `add_inductive`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct BlockGoal {
    pub levels: u32,
    pub params: u32,
    /// The declaration rows of the types, in the block's order.
    pub types: Vec<Ix>,
    /// The declaration rows of their recursors, in the same order.
    pub recursors: Vec<Ix>,
    /// The judgement rows the check consults, in the order it consults
    /// them (each `whnf`, `infer`, `instantiate` and `is_def_eq` of the
    /// kernel's procedure).
    pub tape: Vec<Ix>,
}

/// What a nested inductive block's check establishes (spec/lean/derivation.md,
/// "nested inductive blocks"): `aux` is the mutual block Lean's kernel
/// makes of it, checked as any block, and the types, constructors and
/// recursors given are that block's rewritten back.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct NestedGoal {
    pub levels: u32,
    pub params: u32,
    /// The declaration rows of the block's own types.
    pub types: Vec<Ix>,
    /// The declaration rows of the recursors: one per type of `aux`, in its
    /// order (the block's own types, then Lean's `rec_1`, `rec_2`, ...).
    pub recursors: Vec<Ix>,
    pub aux: BlockGoal,
}

/// The key of the Lean constant with this dotted name of string
/// components, as the importer derives it (`import::Env::key`).
pub fn lean_key(name: &str) -> Key {
    let mut h = blake3::Hasher::new_derive_key("cas-lean lean constant name");
    for s in name.split('.') {
        h.update(&[0]);
        h.update(&(s.len() as u64).to_le_bytes());
        h.update(s.as_bytes());
    }
    *h.finalize().as_bytes()
}

impl Derivation {
    /// Lean's `consumeTypeAnnotations`, which its kernel applies to every
    /// binder type of an inductive block: `optParam T d`, `autoParam T s`,
    /// `outParam T` and `semiOutParam T` become T, repeatedly. Whoever uses
    /// the result also proves it equal to e, so these names are trusted for
    /// nothing.
    pub fn consume_annotations(&self, mut e: Ix) -> Ix {
        thread_local! {
            static KEYS: [(Key, usize); 4] = [
                (lean_key("optParam"), 2),
                (lean_key("autoParam"), 2),
                (lean_key("outParam"), 1),
                (lean_key("semiOutParam"), 1),
            ];
        }
        loop {
            let mut args = vec![];
            let mut h = e;
            while let TermRow::App(f, x) = self.terms[h as usize] {
                args.push(x);
                h = f;
            }
            let TermRow::Const(d, _) = self.terms[h as usize] else { return e };
            let key = self.decls[d as usize].key;
            if !KEYS.with(|k| k.iter().any(|(k, n)| *k == key && *n == args.len())) {
                return e;
            }
            e = *args.last().unwrap();
        }
    }
}

impl Prim {
    /// The name of the Lean constant this primitive is.
    pub fn lean_name(self) -> &'static str {
        use Prim::*;
        match self {
            Nat => "Nat",
            NatZero => "Nat.zero",
            NatSucc => "Nat.succ",
            NatAdd => "Nat.add",
            NatSub => "Nat.sub",
            NatMul => "Nat.mul",
            NatPow => "Nat.pow",
            NatGcd => "Nat.gcd",
            NatMod => "Nat.mod",
            NatDiv => "Nat.div",
            NatBeq => "Nat.beq",
            NatBle => "Nat.ble",
            NatLand => "Nat.land",
            NatLor => "Nat.lor",
            NatXor => "Nat.xor",
            NatShiftLeft => "Nat.shiftLeft",
            NatShiftRight => "Nat.shiftRight",
            BoolTrue => "Bool.true",
            BoolFalse => "Bool.false",
            String => "String",
            StringMk => "String.mk",
            Char => "Char",
            CharOfNat => "Char.ofNat",
            ListNil => "List.nil",
            ListCons => "List.cons",
        }
    }
}
