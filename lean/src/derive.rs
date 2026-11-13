//! A small kernel that writes the tables (the prover's side). It type
//! checks the core calculus (sorts, constants, applications, binders, let,
//! literals) the way Lean's kernel does, and records each step that holds
//! as a row. A fact proven once is reused, so the rows form a graph.
//!
//! It unfolds definitions eagerly, unlike Lean's lazy delta reduction: it is
//! here to produce tables for tests and to count rows, not to be fast.

use crate::level::{Level, equivalent};
use crate::tables::*;
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
    facts: HashMap<Fact, Ix>,
    infers: HashMap<(Ix, Ix), (Ix, Ix)>,
    meta: Vec<Meta>,
    clen: Vec<u32>,
    /// For tests only: write tables that project data out of a
    /// proposition, which the checker must refuse.
    pub lax: bool,
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
        equivalent(&self.to_level(l), &self.to_level(m)).then(|| self.add(Fact::LEq { l, m }, Rule::LEqNorm, vec![]))
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

    fn step(&mut self, g: Ix, e: Ix) -> Option<(Ix, Ix)> {
        if let Some(r) = self.iota(g, e) {
            return Some(r);
        }
        match self.t.terms[e as usize].clone() {
            TermRow::App(f, x) => {
                if let TermRow::Lam(_, body) = self.t.terms[f as usize] {
                    let (p, r) = self.inst(body, 0, x);
                    return Some((self.add(Fact::Eq { g, a: e, b: r }, Rule::Beta, vec![p]), r));
                }
                let (p, f2) = self.step(g, f)?;
                let e2 = self.app(f2, x);
                let rx = self.refl(g, x);
                Some((self.add(Fact::Eq { g, a: e, b: e2 }, Rule::CongApp, vec![p, rx]), e2))
            }
            TermRow::Const(d, ls) => {
                let decl = self.t.decls[d as usize].clone();
                if !matches!(decl.kind, DeclKind::Definition | DeclKind::Theorem) {
                    return None;
                }
                let v = decl.value?;
                let (prem, r) = if decl.levels == 0 {
                    (vec![], v)
                } else {
                    let (p, r) = self.linst(v, ls);
                    (vec![p], r)
                };
                Some((self.add(Fact::Eq { g, a: e, b: r }, Rule::Delta, prem), r))
            }
            TermRow::Let(_, v, body) => {
                let (p, r) = self.inst(body, 0, v);
                Some((self.add(Fact::Eq { g, a: e, b: r }, Rule::Zeta, vec![p]), r))
            }
            TermRow::Proj(s, idx, x) => {
                let (p, x2) = self.whnf(g, x);
                let (h, args) = self.spine(x2);
                let TermRow::Const(c, _) = self.t.terms[h as usize] else { return None };
                let Info::Ctor { params, fields, .. } = self.t.decls[c as usize].info else { return None };
                if args.len() != (params + fields) as usize || idx >= fields {
                    return None;
                }
                let e2 = self.term(TermRow::Proj(s, idx, x2));
                let cong = if x2 == x {
                    self.refl(g, e)
                } else {
                    self.add(Fact::Eq { g, a: e, b: e2 }, Rule::CongProj, vec![p])
                };
                let r = args[(params + idx) as usize];
                let pc = self.add(Fact::Eq { g, a: e2, b: r }, Rule::ProjCtor, vec![]);
                Some((self.trans(cong, pc), r))
            }
            _ => None,
        }
    }

    /// The weak head normal form of `e`, and a row `Eq(g, e, w)`.
    pub fn whnf(&mut self, g: Ix, e: Ix) -> (Ix, Ix) {
        let mut proof = self.refl(g, e);
        let mut cur = e;
        while let Some((p, next)) = self.step(g, cur) {
            proof = self.trans(proof, p);
            cur = next;
        }
        (proof, cur)
    }

    /// A row `Eq(g, a, b)`, if they are definitionally equal.
    pub fn defeq(&mut self, g: Ix, a: Ix, b: Ix) -> Res<Option<Ix>> {
        if a == b {
            return Ok(Some(self.refl(g, a)));
        }
        if let Some(i) = self.facts.get(&Fact::Eq { g, a, b }) {
            return Ok(Some(*i));
        }
        if let Some(p) = self.proof_irrel(g, a, b)? {
            return Ok(Some(p));
        }
        let ((pa, wa), (pb, wb)) = (self.whnf(g, a), self.whnf(g, b));
        let Some(core) = self.defeq_whnf(g, wa, wb)? else { return Ok(None) };
        let sb = self.symm(pb);
        let r = self.trans(core, sb);
        Ok(Some(self.trans(pa, r)))
    }

    fn proof_irrel(&mut self, g: Ix, a: Ix, b: Ix) -> Res<Option<Ix>> {
        let (p0, ta) = self.infer(g, a)?;
        let (p3, s) = self.infer(g, ta)?;
        let (p4, z) = self.whnf(g, s);
        let TermRow::Sort(l) = self.t.terms[z as usize] else { return Ok(None) };
        if self.t.levels[l as usize] != LevelRow::Zero {
            return Ok(None);
        }
        let (p1, tb) = self.infer(g, b)?;
        let Some(p2) = self.defeq(g, ta, tb)? else { return Ok(None) };
        Ok(Some(self.add(Fact::Eq { g, a, b }, Rule::ProofIrrel, vec![p0, p1, p2, p3, p4])))
    }

    fn defeq_whnf(&mut self, g: Ix, a: Ix, b: Ix) -> Res<Option<Ix>> {
        if a == b {
            return Ok(Some(self.refl(g, a)));
        }
        let (ta, tb) = (self.t.terms[a as usize].clone(), self.t.terms[b as usize].clone());
        let fact = Fact::Eq { g, a, b };
        Ok(match (ta, tb) {
            (TermRow::Sort(l), TermRow::Sort(m)) => self.leq(l, m).map(|p| self.add(fact, Rule::CongSort, vec![p])),
            (TermRow::Const(d, ls), TermRow::Const(d2, ms))
                if self.t.decls[d as usize].key == self.t.decls[d2 as usize].key =>
            {
                let (ls, ms) = (self.levels_of(ls), self.levels_of(ms));
                let mut prem = vec![];
                for (l, m) in ls.iter().zip(&ms) {
                    match self.leq(*l, *m) {
                        Some(p) => prem.push(p),
                        None => return Ok(None),
                    }
                }
                Some(self.add(fact, Rule::CongConst, prem))
            }
            (TermRow::App(f, x), TermRow::App(f2, x2)) => {
                let (Some(p), Some(q)) = (self.defeq(g, f, f2)?, self.defeq(g, x, x2)?) else { return Ok(None) };
                Some(self.add(fact, Rule::CongApp, vec![p, q]))
            }
            (TermRow::Lam(d1, b1), TermRow::Lam(d2, b2)) | (TermRow::Pi(d1, b1), TermRow::Pi(d2, b2)) => {
                let rule =
                    if matches!(self.t.terms[a as usize], TermRow::Lam(..)) { Rule::CongLam } else { Rule::CongPi };
                let Some(p) = self.defeq(g, d1, d2)? else { return Ok(None) };
                let (ext, fvar, q1, o1) = self.open(g, d1, b1);
                let (q2, o2) = self.inst(b2, 0, fvar);
                let Some(r) = self.defeq(ext, o1, o2)? else { return Ok(None) };
                Some(self.add(fact, rule, vec![p, q2, q1, r]))
            }
            (TermRow::Proj(s, n, x), TermRow::Proj(s2, n2, y))
                if n == n2 && self.t.decls[s as usize].key == self.t.decls[s2 as usize].key =>
            {
                self.defeq(g, x, y)?.map(|p| self.add(fact, Rule::CongProj, vec![p]))
            }
            (TermRow::Lam(d, _), _) => self.eta(g, a, d, b)?,
            (_, TermRow::Lam(d, _)) => self.eta(g, b, d, a)?.map(|p| self.symm(p)),
            _ => None,
        })
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
                let p3 = self.defeq(g, at, dom)?.ok_or("the argument's type is not the domain")?;
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
                let l = self.level(LevelRow::IMax(u, v));
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
            TermRow::Lit(_) => {
                let d = self.t.nat.ok_or("no Nat for literals")?;
                (Rule::LitNat, vec![], self.constant(d, &[]))
            }
            TermRow::Str(_) => {
                let d = self.t.string.ok_or("no String for literals")?;
                (Rule::LitStr, vec![], self.constant(d, &[]))
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
        let is_prop = equivalent(&self.to_level(l), &Level::Zero);
        let (head, ps) = self.spine(iapp);
        let TermRow::Const(d, ls) = self.t.terms[head as usize] else { return Err("not a structure".into()) };
        let induct = self.t.decls[d as usize].clone();
        let Info::Induct { params, ctors, .. } = &induct.info else { return Err("not an inductive type".into()) };
        if induct.key != self.t.decls[s as usize].key || ctors.len() != 1 {
            return Err("not the structure projected".into());
        }
        let c = *self.decls.get(&ctors[0]).ok_or("the constructor is not cited")?;
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
