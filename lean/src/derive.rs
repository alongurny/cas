//! A small kernel that writes the tables (the prover's side). It type
//! checks the core calculus (sorts, constants, applications, binders, let,
//! literals) the way Lean's kernel does, and records each step that holds
//! as a row. A fact proven once is reused, so the rows form a graph.
//!
//! It unfolds definitions eagerly, unlike Lean's lazy delta reduction: it is
//! here to produce tables for tests and to count rows, not to be fast.

mod block;
mod kernel;
mod source;
pub use source::{Cited, Hint, Source, compare};
mod nested;

use crate::level::{Level, same_value};
use crate::nat;
use crate::tables::*;
use num_bigint::BigUint;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default)]
struct Meta {
    lbr: u32,
    fv: u32,
}

#[derive(Default)]
pub struct Builder {
    pub t: Tables,
    levels: HashMap<LevelRow, Ix>,
    lists: HashMap<ListRow, Ix>,
    terms: HashMap<TermRow, Ix>,
    ctxs: HashMap<CtxRow, Ix>,
    decls: HashMap<Key, Ix>,
    prims: HashMap<Prim, Ix>,
    facts: HashMap<Fact, Ix>,
    infers: HashMap<(Ix, Ix), (Ix, Ix)>,
    meta: Vec<Meta>,
    clen: Vec<u32>,
    /// For tests only: write tables that project data out of a
    /// proposition, which the checker must refuse.
    pub lax: bool,
    pub source: Option<std::rc::Rc<dyn Source>>,
    /// For measurements: give up (by panicking) after this many rows; 0 is
    /// no limit.
    pub budget: usize,
    hints: HashMap<Ix, Hint>,
    whnf_core_cache: HashMap<(Ix, Ix), (Ix, Ix)>,
    whnf_cache: HashMap<(Ix, Ix), (Ix, Ix)>,
    failures: std::collections::HashSet<(Ix, Ix, Ix)>,
    depth: u32,
    pending: HashMap<Ix, crate::expr::X>,
}

pub type Res<T> = Result<T, String>;

impl Builder {
    pub fn new() -> Builder {
        Builder::default()
    }

    // ---- Rows, interned ----

    pub fn level(&mut self, l: LevelRow) -> Ix {
        if let Some(i) = self.levels.get(&l) {
            return *i;
        }
        let i = self.t.levels.len() as Ix;
        self.t.levels.push(l.clone());
        self.levels.insert(l, i);
        i
    }
    pub fn zero(&mut self) -> Ix {
        self.level(LevelRow::Zero)
    }
    pub fn succ(&mut self, l: Ix) -> Ix {
        self.level(LevelRow::Succ(l))
    }
    pub fn param(&mut self, n: u32) -> Ix {
        self.level(LevelRow::Param(n))
    }

    pub fn list(&mut self, ls: &[Ix]) -> Ix {
        let mut r = self.list_row(ListRow::Nil);
        for l in ls.iter().rev() {
            r = self.list_row(ListRow::Cons(*l, r));
        }
        r
    }
    fn list_row(&mut self, l: ListRow) -> Ix {
        if let Some(i) = self.lists.get(&l) {
            return *i;
        }
        let i = self.t.lists.len() as Ix;
        self.t.lists.push(l.clone());
        self.lists.insert(l, i);
        i
    }
    fn levels_of(&self, mut ls: Ix) -> Vec<Ix> {
        let mut out = vec![];
        while let ListRow::Cons(h, r) = self.t.lists[ls as usize] {
            out.push(h);
            ls = r;
        }
        out
    }

    pub fn term(&mut self, e: TermRow) -> Ix {
        if let Some(i) = self.terms.get(&e) {
            return *i;
        }
        let m = |j: &Ix| self.meta[*j as usize];
        let meta = match &e {
            TermRow::BVar(n) => Meta { lbr: n + 1, fv: 0 },
            TermRow::FVar(n) => Meta { lbr: 0, fv: n + 1 },
            TermRow::Sort(_) | TermRow::Const(..) | TermRow::Lit(_) | TermRow::Str(_) => Meta::default(),
            TermRow::App(a, b) => Meta { lbr: m(a).lbr.max(m(b).lbr), fv: m(a).fv.max(m(b).fv) },
            TermRow::Lam(a, b) | TermRow::Pi(a, b) => {
                Meta { lbr: m(a).lbr.max(m(b).lbr.saturating_sub(1)), fv: m(a).fv.max(m(b).fv) }
            }
            TermRow::Let(a, v, b) => Meta {
                lbr: m(a).lbr.max(m(v).lbr).max(m(b).lbr.saturating_sub(1)),
                fv: m(a).fv.max(m(v).fv).max(m(b).fv),
            },
            TermRow::Proj(_, _, x) => m(x),
        };
        let i = self.t.terms.len() as Ix;
        self.t.terms.push(e.clone());
        self.terms.insert(e, i);
        self.meta.push(meta);
        i
    }
    pub fn sort(&mut self, l: Ix) -> Ix {
        self.term(TermRow::Sort(l))
    }
    pub fn app(&mut self, f: Ix, a: Ix) -> Ix {
        self.term(TermRow::App(f, a))
    }
    pub fn apps(&mut self, f: Ix, args: &[Ix]) -> Ix {
        args.iter().fold(f, |f, a| self.app(f, *a))
    }
    pub fn lam(&mut self, a: Ix, b: Ix) -> Ix {
        self.term(TermRow::Lam(a, b))
    }
    pub fn pi(&mut self, a: Ix, b: Ix) -> Ix {
        self.term(TermRow::Pi(a, b))
    }
    pub fn bvar(&mut self, n: u32) -> Ix {
        self.term(TermRow::BVar(n))
    }
    pub fn constant(&mut self, d: Ix, levels: &[Ix]) -> Ix {
        let ls = self.list(levels);
        self.term(TermRow::Const(d, ls))
    }

    pub fn ctx(&mut self, c: CtxRow) -> Ix {
        if let Some(i) = self.ctxs.get(&c) {
            return *i;
        }
        let len = match c {
            CtxRow::Empty => 0,
            CtxRow::Push(q, _) => self.clen[q as usize] + 1,
        };
        let i = self.t.ctxs.len() as Ix;
        self.t.ctxs.push(c.clone());
        self.ctxs.insert(c, i);
        self.clen.push(len);
        i
    }
    pub fn empty(&mut self) -> Ix {
        self.ctx(CtxRow::Empty)
    }

    /// Names the declaration row `d` as the primitive `p`.
    pub fn prim(&mut self, p: Prim, d: Ix) {
        self.t.prims.push((p, d));
        self.prims.insert(p, d);
    }

    /// Cites a declaration (its own proof is assumed).
    pub fn decl(&mut self, d: DeclRow) -> Ix {
        if let Some(i) = self.decls.get(&d.key) {
            return *i;
        }
        let i = self.t.decls.len() as Ix;
        self.decls.insert(d.key, i);
        self.t.decls.push(d);
        i
    }

    fn add(&mut self, fact: Fact, rule: Rule, premises: Vec<Ix>) -> Ix {
        if let Some(i) = self.facts.get(&fact) {
            return *i;
        }
        if self.budget > 0 && self.t.rows.len() >= self.budget {
            panic!("cas-lean: row budget exhausted");
        }
        let i = self.t.rows.len() as Ix;
        self.t.rows.push(Row { fact, rule, premises });
        self.facts.insert(fact, i);
        i
    }

    fn row(&self, i: Ix) -> Fact {
        self.t.rows[i as usize].fact
    }

    // ---- Substitution ----

    pub fn inst(&mut self, e: Ix, k: u32, a: Ix) -> (Ix, Ix) {
        if self.meta[e as usize].lbr <= k {
            return (self.add(Fact::Inst { e, k, a, r: e }, Rule::InstClosed, vec![]), e);
        }
        let (rule, prem, r) = match self.t.terms[e as usize].clone() {
            TermRow::BVar(j) if j == k => (Rule::InstHit, vec![], a),
            TermRow::BVar(j) => (Rule::InstLower, vec![], self.bvar(j - 1)),
            TermRow::App(f, x) => {
                let ((p, f2), (q, x2)) = (self.inst(f, k, a), self.inst(x, k, a));
                (Rule::InstApp, vec![p, q], self.app(f2, x2))
            }
            TermRow::Lam(d, b) | TermRow::Pi(d, b) => {
                let ((p, d2), (q, b2)) = (self.inst(d, k, a), self.inst(b, k + 1, a));
                let r = if matches!(self.t.terms[e as usize], TermRow::Lam(..)) {
                    self.lam(d2, b2)
                } else {
                    self.pi(d2, b2)
                };
                (Rule::InstBind, vec![p, q], r)
            }
            TermRow::Let(t, v, b) => {
                let ((p, t2), (q, v2), (s, b2)) = (self.inst(t, k, a), self.inst(v, k, a), self.inst(b, k + 1, a));
                (Rule::InstLet, vec![p, q, s], self.term(TermRow::Let(t2, v2, b2)))
            }
            TermRow::Proj(s, n, x) => {
                let (p, x2) = self.inst(x, k, a);
                (Rule::InstProj, vec![p], self.term(TermRow::Proj(s, n, x2)))
            }
            _ => unreachable!("closed terms are handled above"),
        };
        (self.add(Fact::Inst { e, k, a, r }, rule, prem), r)
    }

    /// `e` with `fvar n` replaced by `bvar depth`: the inverse of opening.
    fn abstract_fvar(&mut self, e: Ix, n: u32, depth: u32) -> Ix {
        if self.meta[e as usize].fv <= n {
            return e;
        }
        match self.t.terms[e as usize].clone() {
            TermRow::FVar(m) if m == n => self.bvar(depth),
            TermRow::App(f, x) => {
                let (f2, x2) = (self.abstract_fvar(f, n, depth), self.abstract_fvar(x, n, depth));
                self.app(f2, x2)
            }
            TermRow::Lam(d, b) => {
                let (d2, b2) = (self.abstract_fvar(d, n, depth), self.abstract_fvar(b, n, depth + 1));
                self.lam(d2, b2)
            }
            TermRow::Pi(d, b) => {
                let (d2, b2) = (self.abstract_fvar(d, n, depth), self.abstract_fvar(b, n, depth + 1));
                self.pi(d2, b2)
            }
            TermRow::Let(t, v, b) => {
                let (t2, v2) = (self.abstract_fvar(t, n, depth), self.abstract_fvar(v, n, depth));
                let b2 = self.abstract_fvar(b, n, depth + 1);
                self.term(TermRow::Let(t2, v2, b2))
            }
            TermRow::Proj(s, k, x) => {
                let x2 = self.abstract_fvar(x, n, depth);
                self.term(TermRow::Proj(s, k, x2))
            }
            _ => e,
        }
    }

    fn has_params(&self, e: Ix) -> bool {
        match &self.t.terms[e as usize] {
            TermRow::Sort(l) => self.level_has_params(*l),
            TermRow::Const(_, ls) => self.levels_of(*ls).iter().any(|l| self.level_has_params(*l)),
            TermRow::App(a, b) | TermRow::Lam(a, b) | TermRow::Pi(a, b) => self.has_params(*a) || self.has_params(*b),
            TermRow::Let(a, v, b) => self.has_params(*a) || self.has_params(*v) || self.has_params(*b),
            TermRow::Proj(_, _, x) => self.has_params(*x),
            _ => false,
        }
    }
    fn level_has_params(&self, l: Ix) -> bool {
        match self.t.levels[l as usize] {
            LevelRow::Zero => false,
            LevelRow::Succ(a) => self.level_has_params(a),
            LevelRow::Max(a, b) | LevelRow::IMax(a, b) => self.level_has_params(a) || self.level_has_params(b),
            LevelRow::Param(_) => true,
        }
    }

    pub fn linstl(&mut self, l: Ix, ls: Ix) -> (Ix, Ix) {
        let (rule, prem, r) = match self.t.levels[l as usize].clone() {
            LevelRow::Zero => (Rule::LInstLZero, vec![], l),
            LevelRow::Succ(a) => {
                let (p, a2) = self.linstl(a, ls);
                (Rule::LInstLSucc, vec![p], self.succ(a2))
            }
            LevelRow::Max(a, b) | LevelRow::IMax(a, b) => {
                let ((p, a2), (q, b2)) = (self.linstl(a, ls), self.linstl(b, ls));
                let max = matches!(self.t.levels[l as usize], LevelRow::Max(..));
                let r = self.level(if max { LevelRow::Max(a2, b2) } else { LevelRow::IMax(a2, b2) });
                (if max { Rule::LInstLMax } else { Rule::LInstLIMax }, vec![p, q], r)
            }
            LevelRow::Param(n) => (Rule::LInstLParam, vec![], self.levels_of(ls)[n as usize]),
        };
        (self.add(Fact::LInstL { l, ls, r }, rule, prem), r)
    }

    pub fn linst(&mut self, e: Ix, ls: Ix) -> (Ix, Ix) {
        if !self.has_params(e) {
            return (self.add(Fact::LInst { e, ls, r: e }, Rule::LInstNone, vec![]), e);
        }
        let (rule, prem, r) = match self.t.terms[e as usize].clone() {
            TermRow::Sort(l) => {
                let (p, l2) = self.linstl(l, ls);
                (Rule::LInstSort, vec![p], self.sort(l2))
            }
            TermRow::Const(d, ms) => {
                let (mut prem, mut out) = (vec![], vec![]);
                for m in self.levels_of(ms) {
                    let (p, m2) = self.linstl(m, ls);
                    prem.push(p);
                    out.push(m2);
                }
                (Rule::LInstConst, prem, self.constant(d, &out))
            }
            TermRow::App(a, b) | TermRow::Lam(a, b) | TermRow::Pi(a, b) => {
                let ((p, a2), (q, b2)) = (self.linst(a, ls), self.linst(b, ls));
                let (rule, r) = match self.t.terms[e as usize] {
                    TermRow::App(..) => (Rule::LInstApp, self.app(a2, b2)),
                    TermRow::Lam(..) => (Rule::LInstBind, self.lam(a2, b2)),
                    _ => (Rule::LInstBind, self.pi(a2, b2)),
                };
                (rule, vec![p, q], r)
            }
            TermRow::Let(a, v, b) => {
                let ((p, a2), (q, v2), (s, b2)) = (self.linst(a, ls), self.linst(v, ls), self.linst(b, ls));
                (Rule::LInstLet, vec![p, q, s], self.term(TermRow::Let(a2, v2, b2)))
            }
            TermRow::Proj(s, n, x) => {
                let (p, x2) = self.linst(x, ls);
                (Rule::LInstProj, vec![p], self.term(TermRow::Proj(s, n, x2)))
            }
            _ => unreachable!("no levels"),
        };
        (self.add(Fact::LInst { e, ls, r }, rule, prem), r)
    }

    fn to_level(&self, l: Ix) -> Level {
        match self.t.levels[l as usize] {
            LevelRow::Zero => Level::Zero,
            LevelRow::Succ(a) => self.to_level(a).succ(),
            LevelRow::Max(a, b) => Level::max(self.to_level(a), self.to_level(b)),
            LevelRow::IMax(a, b) => Level::imax(self.to_level(a), self.to_level(b)),
            LevelRow::Param(n) => Level::Param(n),
        }
    }

    pub fn leq(&mut self, l: Ix, m: Ix) -> Option<Ix> {
        same_value(&self.to_level(l), &self.to_level(m)).then(|| self.add(Fact::LEq { l, m }, Rule::LEqNorm, vec![]))
    }

    // ---- Contexts ----

    fn has(&mut self, g: Ix, n: u32) -> Res<(Ix, Ix)> {
        match self.t.ctxs[g as usize] {
            CtxRow::Empty => Err(format!("fvar {n} is not in the context")),
            CtxRow::Push(q, a) if self.clen[q as usize] == n => {
                Ok((self.add(Fact::Has { g, n, a }, Rule::HasLast, vec![]), a))
            }
            CtxRow::Push(q, _) => {
                let (p, a) = self.has(q, n)?;
                Ok((self.add(Fact::Has { g, n, a }, Rule::HasUp, vec![p]), a))
            }
        }
    }

    /// Opens a binder body: the extended context, the variable, the
    /// substitution row and the opened body.
    fn open(&mut self, g: Ix, a: Ix, b: Ix) -> (Ix, Ix, Ix, Ix) {
        let n = self.clen[g as usize];
        let fvar = self.term(TermRow::FVar(n));
        let ext = self.ctx(CtxRow::Push(g, a));
        let (p, bo) = self.inst(b, 0, fvar);
        (ext, fvar, p, bo)
    }

    // ---- Equality ----

    fn refl(&mut self, g: Ix, a: Ix) -> Ix {
        self.add(Fact::Eq { g, a, b: a }, Rule::Refl, vec![])
    }
    fn symm(&mut self, p: Ix) -> Ix {
        let Fact::Eq { g, a, b } = self.row(p) else { unreachable!() };
        if a == b {
            return p;
        }
        self.add(Fact::Eq { g, a: b, b: a }, Rule::Symm, vec![p])
    }
    fn trans(&mut self, p: Ix, q: Ix) -> Ix {
        let (Fact::Eq { g, a, b: m }, Fact::Eq { b, .. }) = (self.row(p), self.row(q)) else { unreachable!() };
        if a == m {
            return q;
        }
        if m == b {
            return p;
        }
        self.add(Fact::Eq { g, a, b }, Rule::Trans, vec![p, q])
    }

    /// One reduction step at the head, if any.
    fn spine(&self, mut e: Ix) -> (Ix, Vec<Ix>) {
        let mut args = vec![];
        while let TermRow::App(f, x) = self.t.terms[e as usize] {
            args.push(x);
            e = f;
        }
        args.reverse();
        (e, args)
    }

    /// `Eq(g, e, e')` where e' is e with its argument at `pos` (counted from
    /// the head) replaced, given `p: Eq(g, arg, new)`.
    fn replace_arg(&mut self, g: Ix, e: Ix, pos: usize, nargs: usize, p: Ix) -> (Ix, Ix) {
        let TermRow::App(f, x) = self.t.terms[e as usize] else { unreachable!() };
        let Fact::Eq { b: new, .. } = self.row(p) else { unreachable!() };
        if pos + 1 == nargs {
            let rf = self.refl(g, f);
            let e2 = self.app(f, new);
            (self.add(Fact::Eq { g, a: e, b: e2 }, Rule::CongApp, vec![rf, p]), e2)
        } else {
            let (q, f2) = self.replace_arg(g, f, pos, nargs - 1, p);
            let rx = self.refl(g, x);
            let e2 = self.app(f2, x);
            (self.add(Fact::Eq { g, a: e, b: e2 }, Rule::CongApp, vec![q, rx]), e2)
        }
    }

    /// One iota step: a recursor whose major premise reduces to a
    /// constructor application.
    fn iota(&mut self, g: Ix, e: Ix) -> Option<(Ix, Ix)> {
        let (head, args) = self.spine(e);
        let TermRow::Const(r, ls) = self.t.terms[head as usize] else { return None };
        let decl = self.t.decls[r as usize].clone();
        let Info::Rec { params, motives, minors, indices, rules } = decl.info else { return None };
        let before = (params + motives + minors) as usize;
        let at = before + indices as usize;
        let major = *args.get(at)?;
        let (pm, m2) = self.whnf(g, major);
        let (pm, m2) = match self.as_ctor(g, m2, &rules) {
            Some((p, c)) => (self.trans(pm, p), c),
            None => (pm, m2),
        };
        let (mh, margs) = self.spine(m2);
        let TermRow::Const(c, _) = self.t.terms[mh as usize] else { return None };
        let key = self.t.decls[c as usize].key;
        let Info::Ctor { params: cp, fields, .. } = self.t.decls[c as usize].info else { return None };
        if margs.len() != (cp + fields) as usize {
            return None;
        }
        let rule = rules.iter().find(|x| x.ctor == key)?.clone();
        let (cong, e2) = if m2 == major { (self.refl(g, e), e) } else { self.replace_arg(g, e, at, args.len(), pm) };
        let (prem, rhs) = if decl.levels == 0 {
            (vec![], rule.rhs)
        } else {
            let (p, r) = self.linst(rule.rhs, ls);
            (vec![p], r)
        };
        let mut want: Vec<Ix> = args[..before].to_vec();
        want.extend_from_slice(&margs[cp as usize..]);
        want.extend_from_slice(&args[at + 1..]);
        let b = self.apps(rhs, &want);
        let step = self.add(Fact::Eq { g, a: e2, b }, Rule::Iota, prem);
        Some((self.trans(cong, step), b))
    }

    /// A major premise `m` in weak head normal form that is not a
    /// constructor applied, shown equal to one: by proof irrelevance for a
    /// proposition with one constructor without fields (K-like reduction),
    /// or by eta for a structure that is not a proposition (Lean's
    /// `to_cnstr_when_K` and `to_cnstr_when_structure`).
    fn as_ctor(&mut self, g: Ix, m: Ix, rules: &[RecRule]) -> Option<(Ix, Ix)> {
        if let Some((p, c)) = self.lit_as_ctor(g, m) {
            if matches!(self.t.terms[m as usize], TermRow::Str(_)) {
                // `whnf(string_lit_to_constructor(major))`, as in Lean.
                let (q, w) = self.whnf(g, c);
                return Some((self.trans(p, q), w));
            }
            return Some((p, c));
        }
        let (h, _) = self.spine(m);
        if let TermRow::Const(c, _) = self.t.terms[h as usize]
            && matches!(self.t.decls[c as usize].info, Info::Ctor { .. })
        {
            return None;
        }
        let (_, ty) = self.infer(g, m).ok()?;
        let (_, w) = self.whnf(g, ty);
        let (ih, iargs) = self.spine(w);
        let TermRow::Const(i, ls) = self.t.terms[ih as usize] else { return None };
        let (_, s) = self.infer(g, w).ok()?;
        let (_, s) = self.whnf(g, s);
        let is_prop =
            matches!(self.t.terms[s as usize], TermRow::Sort(l) if self.t.levels[l as usize] == LevelRow::Zero);
        let ikey = self.t.decls[i as usize].key;
        if is_prop {
            let [rule] = rules else { return None };
            let c = self.cite(&rule.ctor).ok()?;
            let Info::Ctor { params, fields: 0, .. } = self.t.decls[c as usize].info else { return None };
            let head = self.term(TermRow::Const(c, ls));
            let built = self.apps(head, iargs.get(..params as usize)?);
            let p = self.proof_irrel(g, m, built).ok()??;
            Some((p, built))
        } else {
            let c = self.structure_ctor(&ikey)?;
            let Info::Ctor { params, fields, .. } = self.t.decls[c as usize].info else { return None };
            let head = self.term(TermRow::Const(c, ls));
            let mut built = self.apps(head, iargs.get(..params as usize)?);
            for k in 0..fields {
                let pk = self.term(TermRow::Proj(i, k, m));
                built = self.app(built, pk);
            }
            let p = self.eta_struct(g, m, built).ok()??;
            Some((p, built))
        }
    }

    /// `Quot.lift f h q` or `Quot.ind h q` (with more arguments) whose q
    /// reduces to `Quot.mk r a`.
    fn quot(&mut self, g: Ix, e: Ix) -> Option<(Ix, Ix)> {
        let (head, args) = self.spine(e);
        let TermRow::Const(d, _) = self.t.terms[head as usize] else { return None };
        let at = match self.t.decls[d as usize].info {
            Info::Quot(QuotKind::Lift) => 5,
            Info::Quot(QuotKind::Ind) => 4,
            _ => return None,
        };
        let q = *args.get(at)?;
        let (pq, q2) = self.whnf(g, q);
        let (mh, margs) = self.spine(q2);
        let TermRow::Const(m, _) = self.t.terms[mh as usize] else { return None };
        if self.t.decls[m as usize].info != Info::Quot(QuotKind::Mk) || margs.len() != 3 {
            return None;
        }
        let (cong, e2) = if q2 == q { (self.refl(g, e), e) } else { self.replace_arg(g, e, at, args.len(), pq) };
        let fa = self.app(args[3], margs[2]);
        let b = self.apps(fa, &args[at + 1..]);
        let step = self.add(Fact::Eq { g, a: e2, b }, Rule::QuotLift, vec![]);
        Some((self.trans(cong, step), b))
    }

    // ---- Literals ----

    fn prim_const(&mut self, p: Prim, levels: &[Ix]) -> Option<Ix> {
        // Lean's kernel knows these by name: cite one the first time.
        if !self.prims.contains_key(&p) && self.source.is_some() {
            self.cite(&crate::tables::lean_key(p.lean_name())).ok()?;
        }
        let d = *self.prims.get(&p)?;
        Some(self.constant(d, levels))
    }

    /// The primitive `e` is, used without levels.
    fn prim_of(&self, e: Ix) -> Option<Prim> {
        let TermRow::Const(d, ls) = self.t.terms[e as usize] else { return None };
        let key = self.t.decls[d as usize].key;
        let p = self.t.prims.iter().find(|(_, x)| self.t.decls[*x as usize].key == key)?.0;
        self.levels_of(ls).is_empty().then_some(p)
    }

    pub fn nat_lit(&mut self, n: &BigUint) -> Ix {
        self.term(TermRow::Lit(nat::bytes(n)))
    }

    fn lit_val(&self, l: Ix) -> BigUint {
        match &self.t.terms[l as usize] {
            TermRow::Lit(b) => nat::value(b),
            _ => unreachable!("a literal"),
        }
    }

    /// A literal written as a constructor application, and `Eq(g, m, c)`:
    /// `Nat.zero` or `Nat.succ (lit n)`, or `String.mk` of the characters.
    fn lit_as_ctor(&mut self, g: Ix, m: Ix) -> Option<(Ix, Ix)> {
        match self.t.terms[m as usize].clone() {
            TermRow::Lit(bytes) => {
                let n = nat::value(&bytes);
                let (c, rule) = if n == BigUint::ZERO {
                    (self.prim_const(Prim::NatZero, &[])?, Rule::NatZero)
                } else {
                    let prev = self.nat_lit(&(n - 1u32));
                    let succ = self.prim_const(Prim::NatSucc, &[])?;
                    (self.app(succ, prev), Rule::NatSucc)
                };
                let p = self.add(Fact::Eq { g, a: c, b: m }, rule, vec![]);
                Some((self.symm(p), c))
            }
            TermRow::Str(s) => {
                let s = String::from_utf8(s).ok()?;
                let zero = self.zero();
                let ch = self.prim_const(Prim::Char, &[])?;
                let nil = self.prim_const(Prim::ListNil, &[zero])?;
                let cons = self.prim_const(Prim::ListCons, &[zero])?;
                let of_nat = self.prim_const(Prim::CharOfNat, &[])?;
                let mut list = self.app(nil, ch);
                for c in s.chars().rev() {
                    let l = self.nat_lit(&(c as u32).into());
                    let cv = self.app(of_nat, l);
                    list = self.apps(cons, &[ch, cv, list]);
                }
                let mk = self.prim_const(Prim::StringMk, &[])?;
                let b = self.app(mk, list);
                Some((self.add(Fact::Eq { g, a: m, b }, Rule::StrLit, vec![]), b))
            }
            _ => None,
        }
    }

    /// `Eq(g, e, lit)` when e reduces to a literal or to `Nat.zero`.
    fn reduce_to_lit(&mut self, g: Ix, e: Ix) -> Option<(Ix, Ix)> {
        let (p, w) = self.whnf(g, e);
        if matches!(self.t.terms[w as usize], TermRow::Lit(_)) {
            return Some((p, w));
        }
        if self.prim_of(w) == Some(Prim::NatZero) {
            let l = self.nat_lit(&BigUint::ZERO);
            let q = self.add(Fact::Eq { g, a: w, b: l }, Rule::NatZero, vec![]);
            return Some((self.trans(p, q), l));
        }
        None
    }

    /// Lean's `reduce_nat`: `Nat.succ` of a literal, and the binary
    /// operations on two literals.
    fn nat_step(&mut self, g: Ix, e: Ix) -> Option<(Ix, Ix)> {
        let TermRow::App(f, y) = self.t.terms[e as usize] else { return None };
        if self.prim_of(f) == Some(Prim::NatSucc) {
            let (p, l) = self.reduce_to_lit(g, y)?;
            let r = self.nat_lit(&(self.lit_val(l) + 1u32));
            let e2 = self.app(f, l);
            let cong = if l == y {
                self.refl(g, e)
            } else {
                let rf = self.refl(g, f);
                self.add(Fact::Eq { g, a: e, b: e2 }, Rule::CongApp, vec![rf, p])
            };
            let s = self.add(Fact::Eq { g, a: e2, b: r }, Rule::NatSucc, vec![]);
            return Some((self.trans(cong, s), r));
        }
        let TermRow::App(op, x) = self.t.terms[f as usize] else { return None };
        let o = self.prim_of(op)?;
        let zero = BigUint::ZERO;
        nat::eval(o, &zero, &zero)?;
        let (px, lx) = self.reduce_to_lit(g, x)?;
        let (py, ly) = self.reduce_to_lit(g, y)?;
        let r = match nat::eval(o, &self.lit_val(lx), &self.lit_val(ly))? {
            nat::Value::Nat(n) => self.nat_lit(&n),
            nat::Value::Bool(v) => self.prim_const(if v { Prim::BoolTrue } else { Prim::BoolFalse }, &[])?,
        };
        let f2 = self.app(op, lx);
        let pf = if lx == x {
            self.refl(g, f)
        } else {
            let rop = self.refl(g, op);
            self.add(Fact::Eq { g, a: f, b: f2 }, Rule::CongApp, vec![rop, px])
        };
        let e2 = self.app(f2, ly);
        let cong =
            if e2 == e { self.refl(g, e) } else { self.add(Fact::Eq { g, a: e, b: e2 }, Rule::CongApp, vec![pf, py]) };
        let s = self.add(Fact::Eq { g, a: e2, b: r }, Rule::NatBin, vec![]);
        Some((self.trans(cong, s), r))
    }

    /// The constructor row of a structure-like type with this key.
    fn structure_ctor(&mut self, induct: &Key) -> Option<Ix> {
        let d = self.cite(induct).ok()?;
        match self.t.decls[d as usize].info.clone() {
            Info::Induct { indices: 0, ctors, recursive: false, .. } if ctors.len() == 1 => self.cite(&ctors[0]).ok(),
            _ => None,
        }
    }

    /// `Eq(g, t, s)` when s is the constructor of a structure applied and
    /// each field equals the projection of t.
    fn eta_struct(&mut self, g: Ix, t: Ix, s: Ix) -> Res<Option<Ix>> {
        let (h, args) = self.spine(s);
        let TermRow::Const(c, _) = self.t.terms[h as usize] else { return Ok(None) };
        let Info::Ctor { induct, params, fields } = self.t.decls[c as usize].info else { return Ok(None) };
        if self.structure_ctor(&induct).is_none() || args.len() != (params + fields) as usize {
            return Ok(None);
        }
        let d = self.cite(&induct)?;
        let (p0, ty_t) = self.infer(g, t)?;
        let (p1, ty_s) = self.infer(g, s)?;
        let Some(p2) = self.defeq(g, ty_t, ty_s)? else { return Ok(None) };
        let mut prem = vec![p0, p1, p2];
        for k in 0..fields {
            let pk = self.term(TermRow::Proj(d, k, t));
            let Some(q) = self.defeq(g, pk, args[(params + k) as usize])? else { return Ok(None) };
            prem.push(q);
        }
        Ok(Some(self.add(Fact::Eq { g, a: t, b: s }, Rule::EtaStruct, prem)))
    }

    /// `Eq(g, a, b)` when their type is a structure whose constructor has
    /// no fields.
    fn unit_like(&mut self, g: Ix, a: Ix, b: Ix) -> Res<Option<Ix>> {
        let (p0, ty_a) = self.infer(g, a)?;
        let (p1, w) = self.whnf(g, ty_a);
        let (h, _) = self.spine(w);
        let TermRow::Const(d, _) = self.t.terms[h as usize] else { return Ok(None) };
        let key = self.t.decls[d as usize].key;
        let Some(c) = self.structure_ctor(&key) else { return Ok(None) };
        if !matches!(self.t.decls[c as usize].info, Info::Ctor { fields: 0, .. }) {
            return Ok(None);
        }
        let (p2, ty_b) = self.infer(g, b)?;
        let Some(p3) = self.defeq(g, w, ty_b)? else { return Ok(None) };
        Ok(Some(self.add(Fact::Eq { g, a, b }, Rule::UnitLike, vec![p0, p1, p2, p3])))
    }

    /// `Eq(g, lam, f)` through `lam d (f (bvar 0))`.
    fn eta(&mut self, g: Ix, lam: Ix, d: Ix, f: Ix) -> Res<Option<Ix>> {
        let (p0, ft) = self.infer(g, f)?;
        let (p1, w) = self.whnf(g, ft);
        let TermRow::Pi(d2, _) = self.t.terms[w as usize] else { return Ok(None) };
        let Some(p2) = self.defeq(g, d, d2)? else { return Ok(None) };
        let x = self.bvar(0);
        let body = self.app(f, x);
        let expanded = self.lam(d, body);
        let pe = self.add(Fact::Eq { g, a: expanded, b: f }, Rule::Eta, vec![p0, p1, p2]);
        let Some(q) = self.defeq(g, lam, expanded)? else { return Ok(None) };
        Ok(Some(self.trans(q, pe)))
    }

    // ---- Typing ----

    fn sort_whnf(&mut self, g: Ix, t: Ix) -> Res<(Ix, Ix, Ix)> {
        let (p, w) = self.whnf(g, t);
        match self.t.terms[w as usize] {
            TermRow::Sort(l) => Ok((p, w, l)),
            _ => Err("not a type".into()),
        }
    }

    /// A row `Infer(g, e, t)` and the type t.
    pub fn infer(&mut self, g: Ix, e: Ix) -> Res<(Ix, Ix)> {
        if let Some(r) = self.infers.get(&(g, e)) {
            return Ok(*r);
        }
        let (rule, prem, t) = match self.t.terms[e as usize].clone() {
            TermRow::Sort(l) => {
                let s = self.succ(l);
                (Rule::Sort, vec![], self.sort(s))
            }
            TermRow::FVar(n) => {
                let (p, a) = self.has(g, n)?;
                (Rule::FVar, vec![p], a)
            }
            TermRow::Const(d, ls) => {
                let decl = self.t.decls[d as usize].clone();
                if decl.levels == 0 {
                    (Rule::Const, vec![], decl.ty)
                } else {
                    let (p, t) = self.linst(decl.ty, ls);
                    (Rule::Const, vec![p], t)
                }
            }
            TermRow::App(f, a) => {
                let (p0, ft) = self.infer(g, f)?;
                let (p1, w) = self.whnf(g, ft);
                let TermRow::Pi(dom, cod) = self.t.terms[w as usize] else {
                    return Err("applying a non-function".into());
                };
                let (p2, at) = self.infer(g, a)?;
                let Some(p3) = self.defeq(g, at, dom)? else {
                    return Err(format!(
                        "the argument's type is not the domain: {} =?= {} in {} applied to {}",
                        self.show(at),
                        self.show(dom),
                        self.show(f),
                        self.show(a)
                    ));
                };
                let (p4, t) = self.inst(cod, 0, a);
                (Rule::App, vec![p0, p1, p2, p3, p4], t)
            }
            TermRow::Lam(a, b) => {
                let (p0, s) = self.infer(g, a)?;
                let (p1, ..) = self.sort_whnf(g, s)?;
                let (ext, fvar, p2, bo) = self.open(g, a, b);
                let (p3, bt) = self.infer(ext, bo)?;
                let n = self.clen[g as usize];
                let cod = self.abstract_fvar(bt, n, 0);
                let (p4, _) = self.inst(cod, 0, fvar);
                (Rule::Lam, vec![p0, p1, p2, p3, p4], self.pi(a, cod))
            }
            TermRow::Pi(a, b) => {
                let (p0, s) = self.infer(g, a)?;
                let (p1, _, u) = self.sort_whnf(g, s)?;
                let (ext, _, p2, bo) = self.open(g, a, b);
                let (p3, s2) = self.infer(ext, bo)?;
                let (p4, _, v) = self.sort_whnf(ext, s2)?;
                let l = crate::level::lean_imax(&self.to_level(u), &self.to_level(v));
                let l = self.level_row(&l);
                (Rule::Pi, vec![p0, p1, p2, p3, p4], self.sort(l))
            }
            TermRow::Let(a, v, b) => {
                let (p0, s) = self.infer(g, a)?;
                let (p1, ..) = self.sort_whnf(g, s)?;
                let (p2, vt) = self.infer(g, v)?;
                let p3 = self.defeq(g, vt, a)?.ok_or("the value's type is not the declared one")?;
                let (p4, b2) = self.inst(b, 0, v);
                let (p5, t) = self.infer(g, b2)?;
                (Rule::Let, vec![p0, p1, p2, p3, p4, p5], t)
            }
            TermRow::Lit(_) => (Rule::LitNat, vec![], self.prim_const(Prim::Nat, &[]).ok_or("no Nat for literals")?),
            TermRow::Str(_) => {
                (Rule::LitStr, vec![], self.prim_const(Prim::String, &[]).ok_or("no String for literals")?)
            }
            TermRow::Proj(s, idx, x) => {
                let (prem, t) = self.proj_type(g, s, idx, x)?;
                (Rule::Proj, prem, t)
            }
            TermRow::BVar(_) => return Err("a loose bound variable".into()),
        };
        let r = (self.add(Fact::Infer { g, e, t }, rule, prem), t);
        self.infers.insert((g, e), r);
        Ok(r)
    }

    /// The premises of `Infer(g, proj s idx x, t)` (see the checker's
    /// `proj_type` for their order) and t.
    fn proj_type(&mut self, g: Ix, s: Ix, idx: u32, x: Ix) -> Res<(Vec<Ix>, Ix)> {
        let (p0, e) = self.infer(g, x)?;
        let (p1, iapp) = self.whnf(g, e);
        let (p2, se) = self.infer(g, e)?;
        let (p3, _, l) = self.sort_whnf(g, se)?;
        let is_prop = self.t.levels[l as usize] == LevelRow::Zero;
        let (head, ps) = self.spine(iapp);
        let TermRow::Const(d, ls) = self.t.terms[head as usize] else { return Err("not a structure".into()) };
        let induct = self.t.decls[d as usize].clone();
        let Info::Induct { params, ctors, .. } = &induct.info else { return Err("not an inductive type".into()) };
        if induct.key != self.t.decls[s as usize].key || ctors.len() != 1 {
            return Err("not the structure projected".into());
        }
        let c = self.cite(&ctors[0])?;
        let (p4, mut r) = self.linst(self.t.decls[c as usize].ty, ls);
        let mut prem = vec![p0, p1, p2, p3, p4];
        let pi = |b: &mut Builder, prem: &mut Vec<Ix>, r: Ix| -> Res<(Ix, Ix)> {
            let (p, w) = b.whnf(g, r);
            prem.push(p);
            match b.t.terms[w as usize] {
                TermRow::Pi(a, body) => Ok((a, body)),
                _ => Err("the constructor's type has too few binders".into()),
            }
        };
        let prop = |b: &mut Builder, prem: &mut Vec<Ix>, a: Ix| -> Res<()> {
            let (p, s) = b.infer(g, a)?;
            let (q, z) = b.whnf(g, s);
            let ok = matches!(b.t.terms[z as usize], TermRow::Sort(l) if b.t.levels[l as usize] == LevelRow::Zero);
            prem.extend([p, q]);
            if ok || b.lax { Ok(()) } else { Err("projecting data out of a proposition".into()) }
        };
        for p in &ps[..*params as usize] {
            let (_, body) = pi(self, &mut prem, r)?;
            let (q, r2) = self.inst(body, 0, *p);
            prem.push(q);
            r = r2;
        }
        for k in 0..idx {
            let (a, body) = pi(self, &mut prem, r)?;
            if is_prop && self.meta[body as usize].lbr > 0 {
                prop(self, &mut prem, a)?;
            }
            let pk = self.term(TermRow::Proj(s, k, x));
            let (q, r2) = self.inst(body, 0, pk);
            prem.push(q);
            r = r2;
        }
        let (a, _) = pi(self, &mut prem, r)?;
        if is_prop {
            prop(self, &mut prem, a)?;
        }
        Ok((prem, a))
    }

    /// Checks a declaration and returns the goal its tables establish.
    pub fn check_decl(&mut self, kind: DeclKind, levels: u32, ty: Ix, value: Option<Ix>) -> Res<Goal> {
        let g = self.empty();
        let (p, s) = self.infer(g, ty)?;
        let (q, _, u) = self.sort_whnf(g, s)?;
        let value_has_ty = match value {
            Some(v) => {
                let (r, t) = self.infer(g, v)?;
                let s = self.defeq(g, t, ty)?.ok_or("the value's type is not the declared type")?;
                Some((r, s))
            }
            None => None,
        };
        let is_prop = if kind == DeclKind::Theorem {
            let z = self.zero();
            Some(self.leq(u, z).ok_or("a theorem's type is not a proposition")?)
        } else {
            None
        };
        Ok(Goal { kind, levels, ty, value, ty_is_type: (p, q), value_has_ty, is_prop })
    }
}
