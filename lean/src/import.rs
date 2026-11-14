//! Reads the declarations `measure/Export.lean` writes (its header gives
//! the format). Terms are kept as numbered nodes and turned into `X` per
//! declaration, since a level parameter's position depends on the
//! declaration. A constant's key is derived from its name here; in cas it
//! will be the key of the declaration's blob.

use crate::derivation::{Key, QuotKind};
use crate::expr::{E, X};
use crate::level::Level;
use num_bigint::BigUint;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Debug)]
enum Item {
    None,
    /// Parent, then a string or a number.
    Name(u32, Result<String, u64>),
    Level(RawLevel),
    Expr(RawExpr),
}

#[derive(Clone, Debug)]
enum RawLevel {
    Zero,
    Succ(u32),
    Max(u32, u32),
    IMax(u32, u32),
    Param(u32),
}

#[derive(Clone, Debug)]
enum RawExpr {
    BVar(u32),
    Sort(u32),
    Const(u32, Vec<u32>),
    App(u32, u32),
    Lam(u32, u32),
    Pi(u32, u32),
    Let(u32, u32, u32),
    Nat(Vec<u8>),
    Str(Vec<u8>),
    Proj(u32, u32, u32),
}

/// Lean's reducibility hint of a definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    Opaque,
    Abbrev,
    Regular(u32),
}

#[derive(Clone, Debug)]
pub enum Kind {
    Axiom,
    Def { value: u32, hint: Hint },
    Thm { value: u32 },
    Opaque { value: u32 },
    Quot(QuotKind),
    Induct { params: u32, indices: u32, rec: bool, nested: u32, all: Vec<u32>, ctors: Vec<u32> },
    Ctor { induct: u32, cidx: u32, params: u32, fields: u32 },
    Rec { params: u32, indices: u32, motives: u32, minors: u32, k: bool, all: Vec<u32>, rules: Vec<(u32, u32, u32)> },
}

/// A declaration: names and terms are node numbers of its `Env`.
#[derive(Clone, Debug)]
pub struct Decl {
    pub name: u32,
    pub ty: u32,
    pub lparams: Vec<u32>,
    pub kind: Kind,
    /// Lean's kernel does not check unsafe declarations fully.
    pub unsafe_: bool,
}

#[derive(Default)]
pub struct Env {
    items: Vec<Item>,
    pub decls: Vec<Decl>,
    by_name: HashMap<u32, usize>,
    by_key: HashMap<Key, usize>,
}

fn unhex(s: &str) -> Result<Vec<u8>, String> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|e| e.to_string())).collect()
}

impl Env {
    pub fn parse(text: &str) -> Result<Env, String> {
        let mut env = Env::default();
        for (ln, line) in text.lines().enumerate() {
            env.line(line).map_err(|e| format!("line {}: {e}: {line:.80}", ln + 1))?;
        }
        for (i, d) in env.decls.iter().enumerate() {
            env.by_name.insert(d.name, i);
        }
        for i in 0..env.decls.len() {
            let k = env.key(env.decls[i].name);
            env.by_key.insert(k, i);
        }
        Ok(env)
    }

    fn put(&mut self, i: u32, item: Item) {
        let i = i as usize;
        if self.items.len() <= i {
            self.items.resize(i + 1, Item::None);
        }
        self.items[i] = item;
    }

    fn line(&mut self, line: &str) -> Result<(), String> {
        let w: Vec<&str> = line.split(' ').collect();
        let n = |k: usize| -> Result<u32, String> {
            w.get(k).ok_or("a field is missing")?.parse::<u32>().map_err(|e| e.to_string())
        };
        match w[0] {
            "N" => {
                let c = match w[3] {
                    "s" => Ok(String::from_utf8(unhex(w.get(4).copied().unwrap_or(""))?).map_err(|e| e.to_string())?),
                    _ => Err(w[4].parse::<u64>().map_err(|e| e.to_string())?),
                };
                self.put(n(1)?, Item::Name(n(2)?, c));
            }
            "L" => {
                let l = match w[2] {
                    "z" => RawLevel::Zero,
                    "s" => RawLevel::Succ(n(3)?),
                    "m" => RawLevel::Max(n(3)?, n(4)?),
                    "i" => RawLevel::IMax(n(3)?, n(4)?),
                    "p" => RawLevel::Param(n(3)?),
                    k => return Err(format!("level kind {k}")),
                };
                self.put(n(1)?, Item::Level(l));
            }
            "E" => {
                let e = match w[2] {
                    "v" => RawExpr::BVar(n(3)?),
                    "s" => RawExpr::Sort(n(3)?),
                    "c" => RawExpr::Const(n(3)?, (4..w.len()).map(n).collect::<Result<_, _>>()?),
                    "a" => RawExpr::App(n(3)?, n(4)?),
                    "l" => RawExpr::Lam(n(3)?, n(4)?),
                    "f" => RawExpr::Pi(n(3)?, n(4)?),
                    "z" => RawExpr::Let(n(3)?, n(4)?, n(5)?),
                    "n" => {
                        let v = BigUint::parse_bytes(w[3].as_bytes(), 10).ok_or("a bad literal")?;
                        RawExpr::Nat(crate::nat::bytes(&v))
                    }
                    "t" => RawExpr::Str(unhex(w.get(3).copied().unwrap_or(""))?),
                    "j" => RawExpr::Proj(n(3)?, n(4)?, n(5)?),
                    k => return Err(format!("term kind {k}")),
                };
                self.put(n(1)?, Item::Expr(e));
            }
            "D" => {
                let bar = w.iter().position(|x| *x == "|").ok_or("no bar")?;
                // `D name ty nlevels lparams... [u] | rest`: u marks an unsafe declaration.
                let nl = n(3)? as usize;
                let lparams = (4..4 + nl).map(n).collect::<Result<_, _>>()?;
                let unsafe_ = match bar - 4 - nl {
                    0 => false,
                    1 if w[4 + nl] == "u" => true,
                    _ => return Err("a bad declaration header".into()),
                };
                let r = &w[bar + 1..];
                let m = |k: usize| -> Result<u32, String> {
                    r.get(k).ok_or("a field is missing")?.parse::<u32>().map_err(|e| e.to_string())
                };
                let list = |from: &str, to: Option<&str>| -> Result<Vec<u32>, String> {
                    let s = r.iter().position(|x| *x == from).ok_or("no list")? + 1;
                    let e = to.and_then(|t| r.iter().position(|x| *x == t)).unwrap_or(r.len());
                    r[s..e].iter().map(|x| x.parse::<u32>().map_err(|e| e.to_string())).collect()
                };
                let kind = match r[0] {
                    "axiom" => Kind::Axiom,
                    "def" => {
                        let hint = match r[2] {
                            "o" => Hint::Opaque,
                            "a" => Hint::Abbrev,
                            h => Hint::Regular(h.parse().map_err(|_| "a bad hint")?),
                        };
                        Kind::Def { value: m(1)?, hint }
                    }
                    "thm" => Kind::Thm { value: m(1)? },
                    "opaque" => Kind::Opaque { value: m(1)? },
                    "quot" => Kind::Quot(match r[1] {
                        "type" => QuotKind::Type,
                        "ctor" => QuotKind::Mk,
                        "lift" => QuotKind::Lift,
                        _ => QuotKind::Ind,
                    }),
                    "induct" => Kind::Induct {
                        params: m(1)?,
                        indices: m(2)?,
                        rec: m(3)? == 1,
                        nested: m(4)?,
                        all: list("all", Some("ctors"))?,
                        ctors: list("ctors", None)?,
                    },
                    "ctor" => Kind::Ctor { induct: m(1)?, cidx: m(2)?, params: m(3)?, fields: m(4)? },
                    "rec" => {
                        let flat = list("rules", None)?;
                        Kind::Rec {
                            params: m(1)?,
                            indices: m(2)?,
                            motives: m(3)?,
                            minors: m(4)?,
                            k: m(5)? == 1,
                            all: list("all", Some("rules"))?,
                            rules: flat.chunks(3).map(|c| (c[0], c[1], c[2])).collect(),
                        }
                    }
                    k => return Err(format!("declaration kind {k}")),
                };
                self.decls.push(Decl { name: n(1)?, ty: n(2)?, lparams, kind, unsafe_ });
            }
            k => return Err(format!("line kind {k}")),
        }
        Ok(())
    }

    /// The name as Lean prints it.
    pub fn name(&self, i: u32) -> String {
        match self.items.get(i as usize) {
            Some(Item::Name(p, c)) => {
                let c = match c {
                    Ok(s) => s.clone(),
                    Err(k) => k.to_string(),
                };
                if *p == 0 { c } else { format!("{}.{c}", self.name(*p)) }
            }
            _ => String::new(),
        }
    }

    /// The key of the constant named i: derived from the name's components,
    /// each with its kind and length, so that no two names share one.
    pub fn key(&self, i: u32) -> Key {
        fn feed(env: &Env, i: u32, h: &mut blake3::Hasher) {
            if let Some(Item::Name(p, c)) = env.items.get(i as usize) {
                feed(env, *p, h);
                match c {
                    Ok(s) => {
                        h.update(&[0]);
                        h.update(&(s.len() as u64).to_le_bytes());
                        h.update(s.as_bytes());
                    }
                    Err(k) => {
                        h.update(&[1]);
                        h.update(&k.to_le_bytes());
                    }
                }
            }
        }
        let mut h = blake3::Hasher::new_derive_key("cas-lean lean constant name");
        feed(self, i, &mut h);
        *h.finalize().as_bytes()
    }

    pub fn by_name(&self, name: u32) -> Option<&Decl> {
        self.by_name.get(&name).map(|i| &self.decls[*i])
    }
    pub fn by_key(&self, k: &Key) -> Option<&Decl> {
        self.by_key.get(k).map(|i| &self.decls[*i])
    }
    pub fn find(&self, name: &str) -> Option<&Decl> {
        self.decls.iter().find(|d| self.name(d.name) == name)
    }

    pub fn level(&self, i: u32, lparams: &[u32]) -> Result<Level, String> {
        let Some(Item::Level(l)) = self.items.get(i as usize) else { return Err(format!("no level {i}")) };
        Ok(match *l {
            RawLevel::Zero => Level::Zero,
            RawLevel::Succ(a) => self.level(a, lparams)?.succ(),
            RawLevel::Max(a, b) => Level::max(self.level(a, lparams)?, self.level(b, lparams)?),
            RawLevel::IMax(a, b) => Level::imax(self.level(a, lparams)?, self.level(b, lparams)?),
            RawLevel::Param(n) => {
                let p = lparams.iter().position(|q| *q == n).ok_or("a level parameter not declared")?;
                Level::Param(p as u32)
            }
        })
    }

    /// Term i of a declaration with these level parameters.
    /// The names of the constants term `i` mentions (constants and the
    /// structures of projections), added to `out`; `seen` holds terms
    /// already walked.
    pub fn constants(&self, i: u32, seen: &mut std::collections::HashSet<u32>, out: &mut std::collections::HashSet<u32>) {
        let mut stack = vec![i];
        while let Some(i) = stack.pop() {
            if !seen.insert(i) {
                continue;
            }
            let Some(Item::Expr(e)) = self.items.get(i as usize) else { continue };
            match e {
                RawExpr::Const(c, _) => {
                    out.insert(*c);
                }
                RawExpr::App(a, b) | RawExpr::Lam(a, b) | RawExpr::Pi(a, b) => stack.extend([*a, *b]),
                RawExpr::Let(a, v, b) => stack.extend([*a, *v, *b]),
                RawExpr::Proj(st, _, x) => {
                    out.insert(*st);
                    stack.push(*x);
                }
                _ => {}
            }
        }
    }

    pub fn term(&self, i: u32, lparams: &[u32], memo: &mut HashMap<u32, X>) -> Result<X, String> {
        if let Some(x) = memo.get(&i) {
            return Ok(x.clone());
        }
        let Some(Item::Expr(e)) = self.items.get(i as usize) else { return Err(format!("no term {i}")) };
        let mut r = |j: u32| self.term(j, lparams, memo);
        let x = Rc::new(match e {
            RawExpr::BVar(n) => E::BVar(*n),
            RawExpr::Sort(l) => E::Sort(self.level(*l, lparams)?),
            RawExpr::Const(c, ls) => {
                let ls = ls.iter().map(|l| self.level(*l, lparams)).collect::<Result<_, _>>()?;
                E::Const(self.key(*c), ls)
            }
            RawExpr::App(a, b) => E::App(r(*a)?, r(*b)?),
            RawExpr::Lam(a, b) => E::Lam(r(*a)?, r(*b)?),
            RawExpr::Pi(a, b) => E::Pi(r(*a)?, r(*b)?),
            RawExpr::Let(a, v, b) => E::Let(r(*a)?, r(*v)?, r(*b)?),
            RawExpr::Nat(n) => E::Nat(n.clone()),
            RawExpr::Str(s) => E::Str(s.clone()),
            RawExpr::Proj(s, k, x) => E::Proj(self.key(*s), *k, r(*x)?),
        });
        memo.insert(i, x.clone());
        Ok(x)
    }
}

// ---- As the prover's source of constants ----

use crate::derivation::{DeclKind, Info, Prim};
use crate::derive::{Cited, Hint as BHint, Source};

fn prim_named(name: &str) -> Option<Prim> {
    use Prim::*;
    [
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
    ]
    .into_iter()
    .find(|p| p.lean_name() == name)
}

impl Env {
    /// The kind and information of a declaration as rows hold them.
    pub fn kind_info(&self, d: &Decl) -> (DeclKind, Info) {
        let keys = |ns: &[u32]| ns.iter().map(|n| self.key(*n)).collect::<Vec<_>>();
        match &d.kind {
            Kind::Axiom => (DeclKind::Axiom, Info::None),
            Kind::Def { .. } => (DeclKind::Definition, Info::None),
            Kind::Thm { .. } => (DeclKind::Theorem, Info::None),
            Kind::Opaque { .. } => (DeclKind::Opaque, Info::None),
            Kind::Quot(k) => (DeclKind::Quot, Info::Quot(*k)),
            Kind::Induct { params, indices, rec, all, ctors, .. } => (
                DeclKind::Inductive,
                Info::Induct {
                    params: *params,
                    indices: *indices,
                    ctors: keys(ctors),
                    recursive: *rec,
                    all: keys(all),
                },
            ),
            Kind::Ctor { induct, params, fields, .. } => {
                (DeclKind::Constructor, Info::Ctor { induct: self.key(*induct), params: *params, fields: *fields })
            }
            Kind::Rec { params, indices, motives, minors, .. } => (
                DeclKind::Recursor,
                Info::Rec { params: *params, motives: *motives, minors: *minors, indices: *indices, rules: vec![] },
            ),
        }
    }

    /// The value of a definition, theorem or opaque declaration.
    pub fn value_of(d: &Decl) -> Option<u32> {
        match d.kind {
            Kind::Def { value, .. } | Kind::Thm { value } | Kind::Opaque { value } => Some(value),
            _ => None,
        }
    }
}

impl Source for Env {
    fn cite(&self, key: &Key) -> Option<Cited> {
        let d = self.by_key(key)?;
        let mut memo = HashMap::new();
        let ty = self.term(d.ty, &d.lparams, &mut memo).ok()?;
        let (kind, info) = self.kind_info(d);
        // An opaque declaration's value is never unfolded.
        let value = match d.kind {
            Kind::Def { value, .. } | Kind::Thm { value } => Some(self.term(value, &d.lparams, &mut memo).ok()?),
            _ => None,
        };
        let hint = match d.kind {
            Kind::Def { hint: Hint::Abbrev, .. } => BHint::Abbrev,
            Kind::Def { hint: Hint::Regular(h), .. } => BHint::Regular(h),
            _ => BHint::Opaque,
        };
        let rules = match &d.kind {
            Kind::Rec { rules, .. } => rules
                .iter()
                .map(|(c, n, rhs)| Some((self.key(*c), *n, self.term(*rhs, &d.lparams, &mut memo).ok()?)))
                .collect::<Option<Vec<_>>>()?,
            _ => vec![],
        };
        Some(Cited {
            kind,
            levels: d.lparams.len() as u32,
            ty,
            value,
            hint,
            info,
            rules,
            prim: prim_named(&self.name(d.name)),
        })
    }
}
