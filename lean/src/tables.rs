//! The tables a prover writes (spec/lean/tables.md). Rows refer to other
//! rows by their position, and only to rows before them, so every table is
//! acyclic by construction. Equality of two terms is equality of their ids,
//! which the checker computes after drawing the random point.

/// A position in a table.
pub type Ix = u32;

/// A BLAKE3 key of a cited declaration.
pub type Key = [u8; 32];

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LevelRow {
    Zero,
    Succ(Ix),
    Max(Ix, Ix),
    IMax(Ix, Ix),
    /// A level parameter of the declaration being checked, by position.
    Param(u32),
}

/// A list of levels: `Nil` or a level and the rest.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ListRow {
    Nil,
    Cons(Ix, Ix),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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
}

/// A context: empty, or a context with one more entry of this type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CtxRow {
    Empty,
    Push(Ix, Ix),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeclKind {
    Axiom,
    Definition,
    Theorem,
    Opaque,
}

/// A cited declaration: its key, kind, number of level parameters, type,
/// and its value when one is unfolded. Its own proof is what makes its type
/// trustworthy; binding these rows to its blob is the caller's part.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeclRow {
    pub key: Key,
    pub kind: DeclKind,
    pub levels: u32,
    pub ty: Ix,
    pub value: Option<Ix>,
}

/// What a judgement row claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    Refl,
    Symm,
    Trans,
    Beta,
    Delta,
    Zeta,
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Row {
    pub fact: Fact,
    pub rule: Rule,
    pub premises: Vec<Ix>,
}

/// What is being checked: a declaration of this kind, with this many level
/// parameters, this type and (for a definition or theorem) this value. The
/// numbers name the judgement rows that establish it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

/// All tables of one declaration's check.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Tables {
    pub levels: Vec<LevelRow>,
    pub lists: Vec<ListRow>,
    pub terms: Vec<TermRow>,
    pub ctxs: Vec<CtxRow>,
    pub decls: Vec<DeclRow>,
    pub rows: Vec<Row>,
    /// The declaration rows of `Nat` and `String`, when literals occur.
    pub nat: Option<Ix>,
    pub string: Option<Ix>,
}

impl Tables {
    /// A commitment to the tables, from which the random point is drawn.
    pub fn commitment(&self) -> [u8; 32] {
        // Any injective encoding will do natively; Debug output is one.
        *blake3::hash(format!("{self:?}").as_bytes()).as_bytes()
    }
}
