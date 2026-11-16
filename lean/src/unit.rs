//! What `cas lean` proves and checks, one unit at a time: a declaration
//! (axiom, definition, theorem, opaque), or an inductive block with its
//! constructors and recursors. Quotients are Lean's builtins, with types
//! fixed by its kernel; they are compared like any cited constant.
//!
//! A proof is a derivation and its goal. Checking it is the checker's run
//! plus the part the checker leaves to its caller: every declaration row
//! the derivation cites is the declaration of that name in the environment
//! (its kind, levels, type, value when unfolded, and information), the
//! goal states the unit's own declarations, and each primitive row is the
//! constant Lean's kernel knows by that name.

use crate::check::Checker;
use crate::derivation::*;
use crate::derive::{Builder, Cited, Source};
use crate::expr::{E, X, level_of};
use crate::import::{Decl, Env, Kind};
use crate::level::Level;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// What a derivation establishes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnitGoal {
    Decl(Goal),
    Block(BlockGoal),
    Nested(NestedGoal),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
    pub goal: UnitGoal,
    pub derivation: Derivation,
}

/// Why a unit was not proved: the stage (`import`, `cite`, `prover`,
/// `budget`, `panic`) and a message.
#[derive(Clone, Debug)]
pub struct Failure {
    pub stage: String,
    pub message: String,
}

fn fail(stage: &str) -> impl Fn(String) -> Failure + '_ {
    move |message| Failure { stage: stage.into(), message }
}

/// Whether declaration i is a unit of its own: an axiom, definition,
/// theorem or opaque declaration, or the first type of an inductive block.
/// Unsafe declarations are not: Lean's kernel does not fully check them.
pub fn is_unit(env: &Env, i: usize) -> bool {
    let d = &env.decls[i];
    !d.unsafe_
        && match &d.kind {
            Kind::Axiom | Kind::Def { .. } | Kind::Thm { .. } | Kind::Opaque { .. } => true,
            Kind::Induct { all, .. } => all.first() == Some(&d.name),
            _ => false,
        }
}

/// The unit that establishes declaration i: itself, or for a type,
/// constructor or recursor of an inductive block, the block's first type.
/// None for quotients (builtins) and unsafe declarations.
pub fn unit_of(env: &Env, i: usize) -> Option<usize> {
    let d = &env.decls[i];
    if d.unsafe_ {
        return None;
    }
    let first = match &d.kind {
        Kind::Induct { all, .. } | Kind::Rec { all, .. } => *all.first()?,
        Kind::Ctor { induct, .. } => match &env.by_name(*induct)?.kind {
            Kind::Induct { all, .. } => *all.first()?,
            _ => return None,
        },
        Kind::Quot(_) => return None,
        _ => return Some(i),
    };
    env.index_of_name(first)
}

struct Shared(Arc<Env>);
impl Source for Shared {
    fn cite(&self, key: &Key) -> Option<Cited> {
        self.0.cite(key)
    }
    fn name(&self, key: &Key) -> Option<String> {
        self.0.by_key(key).map(|d| self.0.name(d.name))
    }
}

/// The names of an inductive block's recursors, in the order the builder
/// takes them: one per type, then Lean's `rec_1`, `rec_2`, ... of a nested
/// block.
fn recursor_names(env: &Env, all: &[u32]) -> Vec<String> {
    let first = env.name(all[0]);
    let mut v: Vec<String> = all.iter().map(|t| format!("{}.rec", env.name(*t))).collect();
    let mut j = 1;
    while env.find_name(&format!("{first}.rec_{j}")).is_some() {
        v.push(format!("{first}.rec_{j}"));
        j += 1;
    }
    v
}

/// Proves unit i, giving up after `budget` rows (0: no limit). Run it on a
/// thread with a large stack: terms can be deep.
pub fn prove(env: &Arc<Env>, i: usize, budget: usize) -> Result<Proof, Failure> {
    let mut b = Builder::new();
    b.source = Some(std::rc::Rc::new(Shared(env.clone())));
    b.budget = budget;
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| prove_with(env, &mut b, i)));
    match run {
        Ok(Ok(goal)) => Ok(Proof { goal, derivation: std::mem::take(&mut b.t) }),
        Ok(Err(f)) => Err(f),
        Err(p) => {
            let m = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            let stage = if m.contains("budget") { "budget" } else { "panic" };
            Err(Failure { stage: stage.into(), message: m })
        }
    }
}

fn prove_with(env: &Env, b: &mut Builder, i: usize) -> Result<UnitGoal, Failure> {
    let d = &env.decls[i];
    if !is_unit(env, i) {
        return Err(Failure { stage: "import".into(), message: format!("{} is not a unit", env.name(d.name)) });
    }
    let mut memo = HashMap::new();
    if let Kind::Induct { params, nested, all, .. } = &d.kind {
        let cite = |b: &mut Builder, n: u32| b.cite(&env.key(n)).map_err(fail("cite"));
        let mut types = vec![];
        let mut ctors = vec![];
        for t in all {
            types.push(cite(b, *t)?);
            let Some(Kind::Induct { ctors: cs, .. }) = env.by_name(*t).map(|x| &x.kind) else {
                return Err(fail("import")("a type of the block is missing".into()));
            };
            ctors.push(cs.iter().map(|c| cite(b, *c)).collect::<Result<Vec<_>, _>>()?);
        }
        let rec_keys: Vec<Key> = recursor_names(env, all).iter().map(|n| lean_key_of(env, n)).collect();
        let levels = d.lparams.len() as u32;
        return if *nested > 0 {
            b.nested_block(levels, *params, &types, &ctors, &rec_keys).map(UnitGoal::Nested).map_err(fail("prover"))
        } else {
            b.inductive_block(levels, *params, &types, &ctors, &rec_keys).map(UnitGoal::Block).map_err(fail("prover"))
        };
    }
    let (kind, _) = env.kind_info(d);
    let ty = env.term(d.ty, &d.lparams, &mut memo).map_err(fail("import"))?;
    let ty = b.row_of(&ty).map_err(fail("cite"))?;
    let value = match Env::value_of(d) {
        Some(v) => {
            let v = env.term(v, &d.lparams, &mut memo).map_err(fail("import"))?;
            Some(b.row_of(&v).map_err(fail("cite"))?)
        }
        None => None,
    };
    b.check_decl(kind, d.lparams.len() as u32, ty, value).map(UnitGoal::Decl).map_err(fail("prover"))
}

/// The key of the constant named `name` in env (or of that name, if env
/// has no such constant: the check then refuses what cites it).
fn lean_key_of(env: &Env, name: &str) -> Key {
    env.find_name(name).map_or_else(|| lean_key(name), |d| env.key(d.name))
}

// ---- The check ----

type Digest = [u8; 32];

fn level_digest(l: &Level) -> Digest {
    let mut h = blake3::Hasher::new_derive_key("cas-lean unit level");
    fn feed(l: &Level, h: &mut blake3::Hasher) {
        match l {
            Level::Zero => {
                h.update(&[0]);
            }
            Level::Succ(a) => {
                h.update(&[1]);
                feed(a, h);
            }
            Level::Max(a, b) => {
                h.update(&[2]);
                feed(a, h);
                feed(b, h);
            }
            Level::IMax(a, b) => {
                h.update(&[3]);
                feed(a, h);
                feed(b, h);
            }
            Level::Param(n) => {
                h.update(&[4]);
                h.update(&n.to_le_bytes());
            }
        }
    }
    feed(l, &mut h);
    *h.finalize().as_bytes()
}

/// A term's digest, the same whether the term is a row of a derivation or
/// an imported term: the structure, with constants by key.
fn node_digest(tag: u8, key: Option<&Key>, levels: &[Digest], nums: &[u64], bytes: &[u8], kids: &[Digest]) -> Digest {
    let mut h = blake3::Hasher::new_derive_key("cas-lean unit term");
    h.update(&[tag]);
    if let Some(k) = key {
        h.update(k);
    }
    h.update(&(levels.len() as u64).to_le_bytes());
    for l in levels {
        h.update(l);
    }
    for n in nums {
        h.update(&n.to_le_bytes());
    }
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
    for k in kids {
        h.update(k);
    }
    *h.finalize().as_bytes()
}

fn x_digest(e: &X, memo: &mut HashMap<*const E, Digest>) -> Digest {
    if let Some(d) = memo.get(&std::rc::Rc::as_ptr(e)) {
        return *d;
    }
    let mut r = |x: &X| x_digest(x, memo);
    let d = match &**e {
        E::BVar(n) => node_digest(0, None, &[], &[*n as u64], &[], &[]),
        E::FVar(n) => node_digest(1, None, &[], &[*n as u64], &[], &[]),
        E::Sort(l) => node_digest(2, None, &[level_digest(l)], &[], &[], &[]),
        E::Const(k, ls) => node_digest(3, Some(k), &ls.iter().map(level_digest).collect::<Vec<_>>(), &[], &[], &[]),
        E::App(a, b) => node_digest(4, None, &[], &[], &[], &[r(a), r(b)]),
        E::Lam(a, b) => node_digest(5, None, &[], &[], &[], &[r(a), r(b)]),
        E::Pi(a, b) => node_digest(6, None, &[], &[], &[], &[r(a), r(b)]),
        E::Let(a, v, b) => node_digest(7, None, &[], &[], &[], &[r(a), r(v), r(b)]),
        E::Nat(n) => node_digest(8, None, &[], &[], n, &[]),
        E::Str(s) => node_digest(9, None, &[], &[], s, &[]),
        E::Proj(k, n, x) => node_digest(10, Some(k), &[], &[*n as u64], &[], &[r(x)]),
    };
    memo.insert(std::rc::Rc::as_ptr(e), d);
    d
}

fn list_levels(t: &Derivation, mut ls: Ix) -> Vec<Level> {
    let mut v = vec![];
    while let ListRow::Cons(l, rest) = t.lists[ls as usize] {
        v.push(level_of(t, l));
        ls = rest;
    }
    v
}

/// Row digests of a derivation's terms, computed in table order (rows name
/// only earlier rows).
fn row_digests(t: &Derivation) -> Result<Vec<Digest>, String> {
    let mut v: Vec<Digest> = Vec::with_capacity(t.terms.len());
    let ix = |v: &Vec<Digest>, x: Ix| v.get(x as usize).copied().ok_or_else(|| format!("term {x} is not earlier"));
    let key = |d: Ix| t.decls.get(d as usize).map(|r| r.key).ok_or_else(|| format!("no declaration row {d}"));
    for row in &t.terms {
        let d = match row {
            TermRow::BVar(n) => node_digest(0, None, &[], &[*n as u64], &[], &[]),
            TermRow::FVar(n) => node_digest(1, None, &[], &[*n as u64], &[], &[]),
            TermRow::Sort(l) => node_digest(2, None, &[level_digest(&level_of(t, *l))], &[], &[], &[]),
            TermRow::Const(d, ls) => {
                let ls: Vec<Digest> = list_levels(t, *ls).iter().map(level_digest).collect();
                node_digest(3, Some(&key(*d)?), &ls, &[], &[], &[])
            }
            TermRow::App(a, b) => node_digest(4, None, &[], &[], &[], &[ix(&v, *a)?, ix(&v, *b)?]),
            TermRow::Lam(a, b) => node_digest(5, None, &[], &[], &[], &[ix(&v, *a)?, ix(&v, *b)?]),
            TermRow::Pi(a, b) => node_digest(6, None, &[], &[], &[], &[ix(&v, *a)?, ix(&v, *b)?]),
            TermRow::Let(a, x, b) => node_digest(7, None, &[], &[], &[], &[ix(&v, *a)?, ix(&v, *x)?, ix(&v, *b)?]),
            TermRow::Lit(n) => node_digest(8, None, &[], &[], n, &[]),
            TermRow::Str(s) => node_digest(9, None, &[], &[], s, &[]),
            TermRow::Proj(d, n, x) => node_digest(10, Some(&key(*d)?), &[], &[*n as u64], &[], &[ix(&v, *x)?]),
        };
        v.push(d);
    }
    Ok(v)
}

/// Compares the derivation's declaration row r with env's declaration.
fn same_decl(env: &Env, rows: &[Digest], r: &DeclRow) -> Result<(), String> {
    let d = env.by_key(&r.key).ok_or("cites a constant the environment does not have")?;
    let name = env.name(d.name);
    let bad = |what: &str| Err(format!("{name}: the cited {what} is not the environment's"));
    let row = |x: Ix| rows.get(x as usize).copied().ok_or_else(|| format!("{name}: no term row {x}"));
    let mut memo = HashMap::new();
    let mut xmemo = HashMap::new();
    let mut term = |i: u32| -> Result<Digest, String> {
        let x = env.term(i, &d.lparams, &mut memo)?;
        Ok(x_digest(&x, &mut xmemo))
    };
    let (kind, info) = env.kind_info(d);
    if d.unsafe_ {
        return Err(format!("{name}: cites an unsafe declaration"));
    }
    if r.kind != kind {
        return bad("kind");
    }
    if r.levels != d.lparams.len() as u32 {
        return bad("level count");
    }
    if row(r.ty)? != term(d.ty)? {
        return bad("type");
    }
    if let Some(v) = r.value {
        // Only definitions and theorems are unfolded.
        let ok = match d.kind {
            Kind::Def { value, .. } | Kind::Thm { value } => row(v)? == term(value)?,
            _ => false,
        };
        if !ok {
            return bad("value");
        }
    }
    match (&r.info, &info, &d.kind) {
        (
            Info::Rec { params, motives, minors, indices, rules },
            Info::Rec { params: p, motives: m, minors: n, indices: i, .. },
            Kind::Rec { rules: lean, .. },
        ) => {
            if (params, motives, minors, indices) != (p, m, n, i) || rules.len() != lean.len() {
                return bad("recursor");
            }
            for (got, (c, f, rhs)) in rules.iter().zip(lean) {
                if got.ctor != env.key(*c) || got.fields != *f || row(got.rhs)? != term(*rhs)? {
                    return bad("recursor rule");
                }
            }
        }
        (a, b, _) if a == b => {}
        _ => return bad("information"),
    }
    Ok(())
}

/// The unit's own declarations, as the goal names them, are env's
/// declarations for unit i.
fn goal_is_unit(env: &Env, i: usize, p: &Proof, rows: &[Digest]) -> Result<(), String> {
    let d = &env.decls[i];
    let t = &p.derivation;
    let name = env.name(d.name);
    let key_of = |x: Ix| t.decls.get(x as usize).map(|r| r.key).ok_or_else(|| format!("no declaration row {x}"));
    let block = |types: &[Ix], recursors: &[Ix]| -> Result<(), String> {
        let Kind::Induct { all, nested, .. } = &d.kind else {
            return Err(format!("{name}: a block proof of a declaration that is not a block"));
        };
        let want: Vec<Key> = all.iter().map(|n| env.key(*n)).collect();
        let got = types.iter().map(|x| key_of(*x)).collect::<Result<Vec<_>, _>>()?;
        if got != want {
            return Err(format!("{name}: the block's types are not the environment's"));
        }
        let want: Vec<Key> = recursor_names(env, all).iter().map(|n| lean_key_of(env, n)).collect();
        let got = recursors.iter().map(|x| key_of(*x)).collect::<Result<Vec<_>, _>>()?;
        if got != want || (*nested == 0) != matches!(p.goal, UnitGoal::Block(_)) {
            return Err(format!("{name}: the block's recursors are not the environment's"));
        }
        Ok(())
    };
    match &p.goal {
        UnitGoal::Decl(g) => {
            let (kind, _) = env.kind_info(d);
            if matches!(d.kind, Kind::Induct { .. }) || g.kind != kind || g.levels != d.lparams.len() as u32 {
                return Err(format!("{name}: the goal is not this declaration's"));
            }
            let mut memo = HashMap::new();
            let mut xmemo = HashMap::new();
            let mut term =
                |i: u32| -> Result<Digest, String> { Ok(x_digest(&env.term(i, &d.lparams, &mut memo)?, &mut xmemo)) };
            let row = |x: Ix| rows.get(x as usize).copied().ok_or_else(|| format!("no term row {x}"));
            if row(g.ty)? != term(d.ty)? {
                return Err(format!("{name}: the goal's type is not the declaration's"));
            }
            let value = match Env::value_of(d) {
                Some(v) => Some(term(v)?),
                None => None,
            };
            let got = match g.value {
                Some(v) => Some(row(v)?),
                None => None,
            };
            if got != value {
                return Err(format!("{name}: the goal's value is not the declaration's"));
            }
            Ok(())
        }
        UnitGoal::Block(g) => block(&g.types, &g.recursors),
        UnitGoal::Nested(g) => block(&g.types, &g.recursors),
    }
}

/// Checks a proof of unit i: the checker accepts the derivation, the goal
/// is the unit's, every cited declaration row is env's declaration, and
/// every primitive row is the constant of that name.
pub fn check(env: &Env, i: usize, p: &Proof) -> Result<(), String> {
    if !is_unit(env, i) {
        return Err(format!("{} is not a unit", env.name(env.decls[i].name)));
    }
    let t = &p.derivation;
    match &p.goal {
        UnitGoal::Decl(g) => Checker::check(t, g),
        UnitGoal::Block(g) => Checker::check_block(t, g),
        UnitGoal::Nested(g) => Checker::check_nested(t, g),
    }
    .map_err(|e| format!("the checker refuses: {e}"))?;
    let rows = row_digests(t)?;
    goal_is_unit(env, i, p, &rows)?;
    // A nested block's derivation also holds the auxiliary block Lean's
    // kernel makes of it. Those rows are under keys derived for them, which
    // no environment has and no other proof can cite; the checker has tied
    // them to the block's own rows, which are compared below.
    let mut aux: HashSet<Key> = HashSet::new();
    if let UnitGoal::Nested(g) = &p.goal {
        for x in g.aux.types.iter().chain(&g.aux.recursors) {
            let r = t.decls.get(*x as usize).ok_or_else(|| format!("no declaration row {x}"))?;
            aux.insert(r.key);
            if let Info::Induct { ctors, .. } = &r.info {
                aux.extend(ctors.iter().copied());
            }
        }
    }
    for r in &t.decls {
        if aux.contains(&r.key) && env.by_key(&r.key).is_none() {
            continue;
        }
        same_decl(env, &rows, r)?;
    }
    for (prim, d) in &t.prims {
        let ok = t.decls.get(*d as usize).is_some_and(|r| r.key == lean_key(prim.lean_name()));
        if !ok {
            return Err(format!("the row given for {} is another constant", prim.lean_name()));
        }
    }
    Ok(())
}

/// The keys of the constants a proof cites, other than the unit's own.
pub fn cited(env: &Env, i: usize, p: &Proof) -> Vec<Key> {
    let own: Vec<Key> = own_keys(env, i);
    p.derivation.decls.iter().map(|r| r.key).filter(|k| !own.contains(k)).collect()
}

/// The keys of unit i's own declarations: itself, or a block's types,
/// constructors and recursors.
pub fn own_keys(env: &Env, i: usize) -> Vec<Key> {
    let d: &Decl = &env.decls[i];
    let Kind::Induct { all, .. } = &d.kind else { return vec![env.key(d.name)] };
    let mut v = vec![];
    for t in all {
        v.push(env.key(*t));
        if let Some(Kind::Induct { ctors, .. }) = env.by_name(*t).map(|x| &x.kind) {
            v.extend(ctors.iter().map(|c| env.key(*c)));
        }
    }
    v.extend(recursor_names(env, all).iter().map(|n| lean_key_of(env, n)));
    v
}

/// The encoding of proofs in cas (spec/formats/lean-proof.md): bincode 1.3
/// with its default options (variable-length integers, little endian).
pub fn encode<T: Serialize>(v: &T) -> Vec<u8> {
    use bincode::Options;
    bincode::DefaultOptions::new().serialize(v).expect("in memory")
}

pub fn decode<'a, T: Deserialize<'a>>(b: &'a [u8]) -> Result<T, String> {
    use bincode::Options;
    bincode::DefaultOptions::new().deserialize(b).map_err(|e| e.to_string())
}

/// Decodes the start of `b`, ignoring the rest (to read a header).
pub fn decode_prefix<'a, T: Deserialize<'a>>(b: &'a [u8]) -> Result<T, String> {
    use bincode::Options;
    bincode::DefaultOptions::new().allow_trailing_bytes().deserialize(b).map_err(|e| e.to_string())
}

/// What `cas lean prove` stores for a unit (the `lean-proof` format): the
/// summary first, so that a reader of reports need not decode the proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub summary: Summary,
    /// The declarations the proof was made against: the name and the key
    /// of the `lean-decl` blob of each of the unit's own declarations and
    /// of each constant cited, in name order.
    pub against: Vec<(String, String)>,
    /// The proof, or why there is none.
    pub outcome: Result<Proof, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// The unit's name.
    pub name: String,
    /// `ok`, or the stage that failed (`import`, `cite`, `prover`,
    /// `budget`, `panic`).
    pub status: String,
    pub rows: u64,
    pub terms: u64,
    pub millis: u64,
}

/// The type of declaration i, as text (cut when long).
pub fn show_type(env: &Arc<Env>, i: usize) -> String {
    let d = &env.decls[i];
    let mut b = Builder::new();
    b.source = Some(std::rc::Rc::new(Shared(env.clone())));
    let mut memo = HashMap::new();
    match env.term(d.ty, &d.lparams, &mut memo).and_then(|t| b.row_of(&t)) {
        Ok(t) => b.show(t),
        Err(e) => format!("(unreadable: {e})"),
    }
}
