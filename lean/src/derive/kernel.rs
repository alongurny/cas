//! Reduction and definitional equality as Lean's kernel decides them
//! (kernel/type_checker.cpp: `whnf_core`, `whnf`, `is_def_eq_core` with
//! lazy delta reduction by reducibility hints), each step that holds
//! written as a row. The search is Lean's; only its successful steps reach
//! the tables, and the checker never sees the hints.

use super::{Builder, Res, compare};
use crate::tables::*;

/// What one lazy delta step found (Lean's `reduction_status`).
enum Status {
    Continue,
    Unknown,
    Equal(Ix),
    Diff,
}

/// Lean's `lbool`, with the row for true.
enum L {
    True(Ix),
    False,
    Undef,
}

impl Builder {
    fn has_fvar(&self, e: Ix) -> bool {
        self.meta[e as usize].fv > 0
    }

    /// `Eq(g, f0 args, f args)` from `p: Eq(g, f0, f)`.
    fn cong_fn(&mut self, g: Ix, p: Ix, args: &[Ix]) -> (Ix, Ix) {
        let Fact::Eq { a: mut cur_a, b: mut cur_b, .. } = self.row(p) else { unreachable!() };
        let mut p = p;
        for x in args {
            let rx = self.refl(g, *x);
            let (na, nb) = (self.app(cur_a, *x), self.app(cur_b, *x));
            p = if cur_a == cur_b {
                self.refl(g, na)
            } else {
                self.add(Fact::Eq { g, a: na, b: nb }, Rule::CongApp, vec![p, rx])
            };
            cur_a = na;
            cur_b = nb;
        }
        (p, cur_b)
    }

    /// The projection `proj s idx x` reduced when x reduces to a
    /// constructor application (Lean's `reduce_proj`); with `cheap`, x is
    /// reduced by `whnf_core` only.
    fn reduce_proj(&mut self, g: Ix, e: Ix, s: Ix, idx: u32, x: Ix, cheap: bool) -> Option<(Ix, Ix)> {
        let (p, c) = if cheap { self.whnf_core(g, x, cheap) } else { self.whnf(g, x) };
        let (p, c) = if matches!(self.t.terms[c as usize], TermRow::Str(_)) {
            let (q, c2) = self.lit_as_ctor(g, c)?;
            (self.trans(p, q), c2)
        } else {
            (p, c)
        };
        let (h, args) = self.spine(c);
        let TermRow::Const(k, _) = self.t.terms[h as usize] else { return None };
        let Info::Ctor { params, fields, .. } = self.t.decls[k as usize].info else { return None };
        if args.len() != (params + fields) as usize || idx >= fields {
            return None;
        }
        let e2 = self.term(TermRow::Proj(s, idx, c));
        let cong =
            if c == x { self.refl(g, e) } else { self.add(Fact::Eq { g, a: e, b: e2 }, Rule::CongProj, vec![p]) };
        let r = args[(params + idx) as usize];
        let pc = self.add(Fact::Eq { g, a: e2, b: r }, Rule::ProjCtor, vec![]);
        Some((self.trans(cong, pc), r))
    }

    /// Lean's `whnf_core`: beta, zeta, projections of constructors and
    /// recursors on constructors, no unfolding of definitions.
    pub(crate) fn whnf_core(&mut self, g: Ix, e: Ix, cheap_proj: bool) -> (Ix, Ix) {
        if !matches!(self.t.terms[e as usize], TermRow::App(..) | TermRow::Let(..) | TermRow::Proj(..)) {
            return (self.refl(g, e), e);
        }
        if let Some(r) = self.whnf_core_cache.get(&(g, e)) {
            return *r;
        }
        let r = match self.t.terms[e as usize].clone() {
            TermRow::Proj(s, idx, x) => match self.reduce_proj(g, e, s, idx, x, cheap_proj) {
                Some((p, m)) => {
                    let (q, r) = self.whnf_core(g, m, cheap_proj);
                    (self.trans(p, q), r)
                }
                None => (self.refl(g, e), e),
            },
            TermRow::Let(_, v, body) => {
                let (p, r) = self.inst(body, 0, v);
                let z = self.add(Fact::Eq { g, a: e, b: r }, Rule::Zeta, vec![p]);
                let (q, r) = self.whnf_core(g, r, cheap_proj);
                (self.trans(z, q), r)
            }
            TermRow::App(..) => {
                let (f0, args) = self.spine(e);
                let (pf, f) = self.whnf_core(g, f0, cheap_proj);
                if let TermRow::Lam(_, body) = self.t.terms[f as usize] {
                    let (p1, e1) = self.cong_fn(g, pf, &args);
                    // Beta on the first argument; whnf_core goes on.
                    let (q, r1) = self.inst(body, 0, args[0]);
                    let head = self.app(f, args[0]);
                    let beta = self.add(Fact::Eq { g, a: head, b: r1 }, Rule::Beta, vec![q]);
                    let (p2, e2) = self.cong_fn(g, beta, &args[1..]);
                    let _ = e1;
                    let (p3, r) = self.whnf_core(g, e2, cheap_proj);
                    let p = self.trans(p2, p3);
                    (self.trans(p1, p), r)
                } else if f == f0 {
                    match self.iota(g, e).or_else(|| self.quot(g, e)) {
                        Some((p, r1)) => {
                            let (q, r) = self.whnf_core(g, r1, cheap_proj);
                            (self.trans(p, q), r)
                        }
                        // Lean returns here without caching.
                        None => return (self.refl(g, e), e),
                    }
                } else {
                    let (p1, e1) = self.cong_fn(g, pf, &args);
                    let (q, r) = self.whnf_core(g, e1, cheap_proj);
                    (self.trans(p1, q), r)
                }
            }
            _ => unreachable!(),
        };
        if !cheap_proj {
            self.whnf_core_cache.insert((g, e), r);
        }
        r
    }

    /// The definition at the head of e, if it can be unfolded (Lean's
    /// `is_delta`: a definition or theorem).
    fn is_delta(&self, e: Ix) -> Option<Ix> {
        let (h, _) = self.spine(e);
        let TermRow::Const(d, _) = self.t.terms[h as usize] else { return None };
        let decl = &self.t.decls[d as usize];
        let has = decl.value.is_some() || self.pending.contains_key(&d);
        (matches!(decl.kind, DeclKind::Definition | DeclKind::Theorem) && has).then_some(d)
    }

    /// Lean's `unfold_definition`: the head constant replaced by its value.
    fn unfold_definition(&mut self, g: Ix, e: Ix) -> Option<(Ix, Ix)> {
        let d = self.is_delta(e)?;
        let (h, args) = self.spine(e);
        let TermRow::Const(_, ls) = self.t.terms[h as usize] else { return None };
        let decl = self.t.decls[d as usize].clone();
        if self.levels_of(ls).len() != decl.levels as usize {
            return None;
        }
        let v = self.value(d)?;
        let (prem, r) = if decl.levels == 0 {
            (vec![], v)
        } else {
            let (p, r) = self.linst(v, ls);
            (vec![p], r)
        };
        let delta = self.add(Fact::Eq { g, a: h, b: r }, Rule::Delta, prem);
        Some(self.cong_fn(g, delta, &args))
    }

    /// Lean's `whnf`: `whnf_core`, then the kernel's arithmetic, then
    /// unfolding the head, until none applies.
    pub fn whnf(&mut self, g: Ix, e: Ix) -> (Ix, Ix) {
        if matches!(
            self.t.terms[e as usize],
            TermRow::BVar(_)
                | TermRow::Sort(_)
                | TermRow::Pi(..)
                | TermRow::Lit(_)
                | TermRow::Str(_)
                | TermRow::FVar(_)
        ) {
            return (self.refl(g, e), e);
        }
        if let Some(r) = self.whnf_cache.get(&(g, e)) {
            return *r;
        }
        let mut proof = self.refl(g, e);
        let mut t = e;
        let r = loop {
            let (p, t1) = self.whnf_core(g, t, false);
            proof = self.trans(proof, p);
            if let Some((p, v)) = self.reduce_nat(g, t1) {
                break (self.trans(proof, p), v);
            }
            match self.unfold_definition(g, t1) {
                Some((p, t2)) => {
                    proof = self.trans(proof, p);
                    t = t2;
                }
                None => break (proof, t1),
            }
        };
        self.whnf_cache.insert((g, e), r);
        r
    }

    /// Lean's `reduce_nat`: only on terms without free variables.
    fn reduce_nat(&mut self, g: Ix, e: Ix) -> Option<(Ix, Ix)> {
        if self.has_fvar(e) {
            return None;
        }
        self.nat_step(g, e)
    }

    /// Lean's `is_def_eq`: a row `Eq(g, a, b)` if they are definitionally
    /// equal.
    pub fn defeq(&mut self, g: Ix, a: Ix, b: Ix) -> Res<Option<Ix>> {
        if a == b {
            return Ok(Some(self.refl(g, a)));
        }
        if let Some(i) = self.facts.get(&Fact::Eq { g, a, b }) {
            return Ok(Some(*i));
        }
        if let Some(i) = self.facts.get(&Fact::Eq { g, a: b, b: a }).copied() {
            return Ok(Some(self.symm(i)));
        }
        self.defeq_core(g, a, b)
    }

    /// Lean's `quick_is_def_eq`.
    fn quick(&mut self, g: Ix, t: Ix, s: Ix) -> Res<L> {
        if t == s {
            return Ok(L::True(self.refl(g, t)));
        }
        if let Some(i) = self.facts.get(&Fact::Eq { g, a: t, b: s }) {
            return Ok(L::True(*i));
        }
        let fact = Fact::Eq { g, a: t, b: s };
        Ok(match (self.t.terms[t as usize].clone(), self.t.terms[s as usize].clone()) {
            (TermRow::Lam(d1, b1), TermRow::Lam(d2, b2)) | (TermRow::Pi(d1, b1), TermRow::Pi(d2, b2)) => {
                let rule =
                    if matches!(self.t.terms[t as usize], TermRow::Lam(..)) { Rule::CongLam } else { Rule::CongPi };
                let Some(p) = self.defeq(g, d1, d2)? else { return Ok(L::False) };
                let (ext, fvar, q1, o1) = self.open(g, d1, b1);
                let (q2, o2) = self.inst(b2, 0, fvar);
                let Some(r) = self.defeq(ext, o1, o2)? else { return Ok(L::False) };
                L::True(self.add(fact, rule, vec![p, q2, q1, r]))
            }
            (TermRow::Sort(l), TermRow::Sort(m)) => match self.leq(l, m) {
                Some(p) => L::True(self.add(fact, Rule::CongSort, vec![p])),
                None => L::False,
            },
            (TermRow::Lit(_), TermRow::Lit(_)) | (TermRow::Str(_), TermRow::Str(_)) => L::False,
            _ => L::Undef,
        })
    }

    /// `Eq(g, t, s)` from `pt: Eq(t, t')`, `core: Eq(t', s')`, `ps: Eq(s, s')`.
    fn around(&mut self, pt: Ix, core: Ix, ps: Ix) -> Ix {
        let sb = self.symm(ps);
        let r = self.trans(core, sb);
        self.trans(pt, r)
    }

    /// Lean's `is_def_eq_proof_irrel`.
    fn proof_irrel_l(&mut self, g: Ix, a: Ix, b: Ix) -> Res<L> {
        let (p0, ta) = self.infer(g, a)?;
        let (p3, s) = self.infer(g, ta)?;
        let (p4, z) = self.whnf(g, s);
        let TermRow::Sort(l) = self.t.terms[z as usize] else { return Ok(L::Undef) };
        if self.t.levels[l as usize] != LevelRow::Zero {
            return Ok(L::Undef);
        }
        let (p1, tb) = self.infer(g, b)?;
        let Some(p2) = self.defeq(g, ta, tb)? else { return Ok(L::False) };
        Ok(L::True(self.add(Fact::Eq { g, a, b }, Rule::ProofIrrel, vec![p0, p1, p2, p3, p4])))
    }

    fn nat_zero(&self, t: Ix) -> bool {
        self.prim_of(t) == Some(Prim::NatZero) || matches!(&self.t.terms[t as usize], TermRow::Lit(b) if b.is_empty())
    }

    /// `Nat.succ x` for t a positive literal or `Nat.succ x` itself: the
    /// row `Eq(g, t, Nat.succ x)` and x.
    fn nat_succ(&mut self, g: Ix, t: Ix) -> Option<(Ix, Ix)> {
        match &self.t.terms[t as usize] {
            TermRow::Lit(b) if !b.is_empty() => {
                let (p, c) = self.lit_as_ctor(g, t)?;
                let TermRow::App(_, x) = self.t.terms[c as usize] else { return None };
                Some((p, x))
            }
            TermRow::App(f, x) if self.prim_of(*f) == Some(Prim::NatSucc) => {
                let x = *x;
                Some((self.refl(g, t), x))
            }
            _ => None,
        }
    }

    /// Lean's `is_def_eq_offset`.
    fn offset(&mut self, g: Ix, t: Ix, s: Ix) -> Res<L> {
        if self.nat_zero(t) && self.nat_zero(s) {
            let lit0 = |b: &mut Builder, x: Ix| {
                if matches!(b.t.terms[x as usize], TermRow::Lit(_)) {
                    b.refl(g, x)
                } else {
                    let l = b.nat_lit(&num_bigint::BigUint::ZERO);
                    b.add(Fact::Eq { g, a: x, b: l }, Rule::NatZero, vec![])
                }
            };
            let (pt, ps) = (lit0(self, t), lit0(self, s));
            let z = self.nat_lit(&num_bigint::BigUint::ZERO);
            let r = self.refl(g, z);
            return Ok(L::True(self.around(pt, r, ps)));
        }
        let (Some((pt, xt)), Some((ps, xs))) = (self.nat_succ(g, t), self.nat_succ(g, s)) else { return Ok(L::Undef) };
        let Some(q) = self.defeq_core(g, xt, xs)? else { return Ok(L::False) };
        let Fact::Eq { b: ct, .. } = self.row(pt) else { unreachable!() };
        let TermRow::App(succ, _) = self.t.terms[ct as usize] else { unreachable!() };
        let rs = self.refl(g, succ);
        let (a, b) = (self.app(succ, xt), self.app(succ, xs));
        let core = if a == b { self.refl(g, a) } else { self.add(Fact::Eq { g, a, b }, Rule::CongApp, vec![rs, q]) };
        Ok(L::True(self.around(pt, core, ps)))
    }

    /// `e'` when e is a projection applied that `whnf_core` reduces.
    fn try_unfold_proj_app(&mut self, g: Ix, e: Ix) -> Option<(Ix, Ix)> {
        let (h, _) = self.spine(e);
        if !matches!(self.t.terms[h as usize], TermRow::Proj(..)) {
            return None;
        }
        let (p, r) = self.whnf_core(g, e, false);
        (r != e).then_some((p, r))
    }

    fn unfold_cheap(&mut self, g: Ix, e: Ix) -> (Ix, Ix) {
        let (p, u) = self.unfold_definition(g, e).expect("is_delta said so");
        let (q, r) = self.whnf_core(g, u, true);
        (self.trans(p, q), r)
    }

    /// Lean's `lazy_delta_reduction_step`, on `(pt: Eq(t, tn), tn)` and
    /// `(ps: Eq(s, sn), sn)`.
    fn lazy_step(&mut self, g: Ix, t: &mut (Ix, Ix), s: &mut (Ix, Ix)) -> Res<Status> {
        let (dt, ds) = (self.is_delta(t.1), self.is_delta(s.1));
        let adv = |b: &mut Builder, x: &mut (Ix, Ix), step: (Ix, Ix)| {
            x.0 = b.trans(x.0, step.0);
            x.1 = step.1;
        };
        match (dt, ds) {
            (None, None) => return Ok(Status::Unknown),
            (Some(_), None) => match self.try_unfold_proj_app(g, s.1) {
                Some(st) => adv(self, s, st),
                None => {
                    let st = self.unfold_cheap(g, t.1);
                    adv(self, t, st)
                }
            },
            (None, Some(_)) => match self.try_unfold_proj_app(g, t.1) {
                Some(st) => adv(self, t, st),
                None => {
                    let st = self.unfold_cheap(g, s.1);
                    adv(self, s, st)
                }
            },
            (Some(a), Some(b)) => {
                let (ha, hb) = (self.hint(a), self.hint(b));
                let c = compare(ha, hb);
                if c < 0 {
                    let st = self.unfold_cheap(g, t.1);
                    adv(self, t, st);
                } else if c > 0 {
                    let st = self.unfold_cheap(g, s.1);
                    adv(self, s, st);
                } else {
                    let apps = matches!(self.t.terms[t.1 as usize], TermRow::App(..))
                        && matches!(self.t.terms[s.1 as usize], TermRow::App(..));
                    let same = self.t.decls[a as usize].key == self.t.decls[b as usize].key;
                    if apps && same && matches!(ha, super::Hint::Regular(_)) && !self.failed_before(g, t.1, s.1) {
                        if let Some(p) = self.same_head_args(g, t.1, s.1)? {
                            let core = self.around(t.0, p, s.0);
                            return Ok(Status::Equal(core));
                        }
                        self.cache_failure(g, t.1, s.1);
                    }
                    let st = self.unfold_cheap(g, t.1);
                    adv(self, t, st);
                    let st = self.unfold_cheap(g, s.1);
                    adv(self, s, st);
                }
            }
        }
        Ok(match self.quick(g, t.1, s.1)? {
            L::True(p) => Status::Equal(self.around(t.0, p, s.0)),
            L::False => Status::Diff,
            L::Undef => Status::Continue,
        })
    }

    fn failed_before(&self, g: Ix, a: Ix, b: Ix) -> bool {
        self.failures.contains(&(g, a, b)) || self.failures.contains(&(g, b, a))
    }
    fn cache_failure(&mut self, g: Ix, a: Ix, b: Ix) {
        self.failures.insert((g, a, b));
    }

    /// `f ls as =?= f ms bs` by levels and arguments (the same head
    /// constant), as Lean's `is_def_eq_args` with `is_def_eq` on levels.
    fn same_head_args(&mut self, g: Ix, t: Ix, s: Ix) -> Res<Option<Ix>> {
        let (ht, at) = self.spine(t);
        let (hs, as_) = self.spine(s);
        if at.len() != as_.len() {
            return Ok(None);
        }
        let Some(ph) = self.defeq_const(g, ht, hs) else { return Ok(None) };
        self.args_after(g, ph, &at, &as_)
    }

    /// `c ls =?= c ms` for the same constant.
    fn defeq_const(&mut self, g: Ix, a: Ix, b: Ix) -> Option<Ix> {
        let (TermRow::Const(d, ls), TermRow::Const(d2, ms)) =
            (self.t.terms[a as usize].clone(), self.t.terms[b as usize].clone())
        else {
            return None;
        };
        if self.t.decls[d as usize].key != self.t.decls[d2 as usize].key {
            return None;
        }
        let (ls, ms) = (self.levels_of(ls), self.levels_of(ms));
        if ls.len() != ms.len() {
            return None;
        }
        let mut prem = vec![];
        for (l, m) in ls.iter().zip(&ms) {
            prem.push(self.leq(*l, *m)?);
        }
        Some(if a == b { self.refl(g, a) } else { self.add(Fact::Eq { g, a, b }, Rule::CongConst, prem) })
    }

    /// From `ph: Eq(f, f')`, `Eq(f as, f' bs)` if each pair of arguments is
    /// definitionally equal.
    fn args_after(&mut self, g: Ix, ph: Ix, at: &[Ix], bs: &[Ix]) -> Res<Option<Ix>> {
        let Fact::Eq { a: mut ca, b: mut cb, .. } = self.row(ph) else { unreachable!() };
        let mut p = ph;
        for (x, y) in at.iter().zip(bs) {
            let Some(q) = self.defeq(g, *x, *y)? else { return Ok(None) };
            let (na, nb) = (self.app(ca, *x), self.app(cb, *y));
            p = if na == nb {
                self.refl(g, na)
            } else {
                self.add(Fact::Eq { g, a: na, b: nb }, Rule::CongApp, vec![p, q])
            };
            ca = na;
            cb = nb;
        }
        Ok(Some(p))
    }

    /// Lean's `lazy_delta_reduction`.
    fn lazy_delta(&mut self, g: Ix, t: &mut (Ix, Ix), s: &mut (Ix, Ix)) -> Res<L> {
        loop {
            match self.offset(g, t.1, s.1)? {
                L::True(p) => return Ok(L::True(self.around(t.0, p, s.0))),
                L::False => return Ok(L::False),
                L::Undef => {}
            }
            if !self.has_fvar(t.1) && !self.has_fvar(s.1) {
                if let Some((p, v)) = self.nat_step(g, t.1) {
                    let pt = self.trans(t.0, p);
                    return Ok(match self.defeq_core(g, v, s.1)? {
                        Some(q) => L::True(self.around(pt, q, s.0)),
                        None => L::False,
                    });
                }
                if let Some((p, v)) = self.nat_step(g, s.1) {
                    let ps = self.trans(s.0, p);
                    return Ok(match self.defeq_core(g, t.1, v)? {
                        Some(q) => L::True(self.around(t.0, q, ps)),
                        None => L::False,
                    });
                }
            }
            match self.lazy_step(g, t, s)? {
                Status::Continue => {}
                Status::Unknown => return Ok(L::Undef),
                Status::Equal(p) => return Ok(L::True(p)),
                Status::Diff => return Ok(L::False),
            }
        }
    }

    /// Lean's `lazy_delta_proj_reduction` on the structures of two
    /// projections with the same index: the row `Eq(g, t, s)` for the
    /// projections `t` (by `pt: Eq(t0, t)`) and `s` (by `ps`), wrapped back.
    fn lazy_delta_proj(&mut self, g: Ix, pt: Ix, ps: Ix, idx: u32) -> Res<Option<Ix>> {
        let (Fact::Eq { b: tc, .. }, Fact::Eq { b: sc, .. }) = (self.row(pt), self.row(ps)) else { unreachable!() };
        let (TermRow::Proj(st, _, x), TermRow::Proj(ss, _, y)) =
            (self.t.terms[tc as usize].clone(), self.t.terms[sc as usize].clone())
        else {
            unreachable!()
        };
        if self.t.decls[st as usize].key != self.t.decls[ss as usize].key {
            return Ok(None);
        }
        let mut t = (self.refl(g, x), x);
        let mut s = (self.refl(g, y), y);
        // `Eq(g, proj x, proj y)` from `Eq(g, x, y)`.
        let wrap = |b: &mut Builder, p: Ix| -> Ix {
            let q = if x == y { b.refl(g, tc) } else { b.add(Fact::Eq { g, a: tc, b: sc }, Rule::CongProj, vec![p]) };
            b.around(pt, q, ps)
        };
        loop {
            match self.lazy_step(g, &mut t, &mut s)? {
                Status::Continue => {}
                Status::Equal(p) => return Ok(Some(wrap(self, p))),
                Status::Unknown | Status::Diff => {
                    // The structures, reduced as far as the steps went:
                    // their fields if both are constructors applied.
                    let (pt2, ps2) = (t.0, s.0);
                    let (tn, sn) = (t.1, s.1);
                    let ft = self.ctor_field(tn, idx);
                    let fs = self.ctor_field(sn, idx);
                    if let (Some(a), Some(b)) = (ft, fs) {
                        let Some(core) = self.defeq_core(g, a, b)? else { return Ok(None) };
                        // proj x = proj tn = a, and the same for y.
                        let ta = self.proj_to_field(g, st, idx, tc, pt2, tn, a);
                        let sb = self.proj_to_field(g, ss, idx, sc, ps2, sn, b);
                        let inner = self.around(ta, core, sb);
                        return Ok(Some(self.around(pt, inner, ps)));
                    }
                    let Some(core) = self.defeq_core(g, tn, sn)? else { return Ok(None) };
                    let p = self.around(pt2, core, ps2);
                    return Ok(Some(wrap(self, p)));
                }
            }
        }
    }

    /// The field idx of e when e is a constructor applied in full.
    fn ctor_field(&self, e: Ix, idx: u32) -> Option<Ix> {
        let (h, args) = self.spine(e);
        let TermRow::Const(k, _) = self.t.terms[h as usize] else { return None };
        let Info::Ctor { params, fields, .. } = self.t.decls[k as usize].info else { return None };
        (args.len() == (params + fields) as usize && idx < fields).then(|| args[(params + idx) as usize])
    }

    /// `Eq(g, proj, field)` for `proj = proj s idx x`, `p: Eq(g, x, c)`
    /// and c a constructor applied whose field idx is `field`.
    #[allow(clippy::too_many_arguments)]
    fn proj_to_field(&mut self, g: Ix, s: Ix, idx: u32, proj: Ix, p: Ix, c: Ix, field: Ix) -> Ix {
        let pc = self.term(TermRow::Proj(s, idx, c));
        let cong = if pc == proj {
            self.refl(g, proj)
        } else {
            self.add(Fact::Eq { g, a: proj, b: pc }, Rule::CongProj, vec![p])
        };
        let f = self.add(Fact::Eq { g, a: pc, b: field }, Rule::ProjCtor, vec![]);
        self.trans(cong, f)
    }

    /// Lean's `is_def_eq_core`.
    pub(crate) fn defeq_core(&mut self, g: Ix, t: Ix, s: Ix) -> Res<Option<Ix>> {
        self.depth += 1;
        if self.depth > 2000 {
            panic!("defeq too deep: {} =?= {}", self.show(t), self.show(s));
        }
        let r = self.defeq_core_(g, t, s);
        self.depth -= 1;
        r
    }

    fn defeq_core_(&mut self, g: Ix, t: Ix, s: Ix) -> Res<Option<Ix>> {
        match self.quick(g, t, s)? {
            L::True(p) => return Ok(Some(p)),
            L::False => return Ok(None),
            L::Undef => {}
        }
        // `decide`: a closed term against `Bool.true`, by evaluation.
        if !self.has_fvar(t) && self.prim_of(s) == Some(Prim::BoolTrue) {
            let (p, w) = self.whnf(g, t);
            if self.prim_of(w) == Some(Prim::BoolTrue) {
                let r = self.defeq_const(g, w, s).ok_or("Bool.true")?;
                return Ok(Some(self.trans(p, r)));
            }
        }
        let (pt, tn) = self.whnf_core(g, t, true);
        let (ps, sn) = self.whnf_core(g, s, true);
        if tn != t || sn != s {
            match self.quick(g, tn, sn)? {
                L::True(p) => return Ok(Some(self.around(pt, p, ps))),
                L::False => return Ok(None),
                L::Undef => {}
            }
        }
        match self.proof_irrel_l(g, tn, sn)? {
            L::True(p) => return Ok(Some(self.around(pt, p, ps))),
            L::False => return Ok(None),
            L::Undef => {}
        }
        let (mut tt, mut ss) = ((pt, tn), (ps, sn));
        match self.lazy_delta(g, &mut tt, &mut ss)? {
            L::True(p) => return Ok(Some(p)),
            L::False => return Ok(None),
            L::Undef => {}
        }
        let ((pt, tn), (ps, sn)) = (tt, ss);
        if let Some(p) = self.defeq_const(g, tn, sn) {
            return Ok(Some(self.around(pt, p, ps)));
        }
        if let (TermRow::FVar(a), TermRow::FVar(b)) = (&self.t.terms[tn as usize], &self.t.terms[sn as usize])
            && a == b
        {
            let r = self.refl(g, tn);
            return Ok(Some(self.around(pt, r, ps)));
        }
        if let (TermRow::Proj(_, i, _), TermRow::Proj(_, j, _)) =
            (&self.t.terms[tn as usize], &self.t.terms[sn as usize])
            && i == j
        {
            let i = *i;
            if let Some(p) = self.lazy_delta_proj(g, pt, ps, i)? {
                return Ok(Some(p));
            }
        }
        let (qt, tnn) = self.whnf_core(g, tn, false);
        let (qs, snn) = self.whnf_core(g, sn, false);
        if tnn != tn || snn != sn {
            let Some(core) = self.defeq_core(g, tnn, snn)? else { return Ok(None) };
            let (a, b) = (self.trans(pt, qt), self.trans(ps, qs));
            return Ok(Some(self.around(a, core, b)));
        }
        // is_def_eq_app
        let (ht, at) = self.spine(tn);
        let (hs, as_) = self.spine(sn);
        if !at.is_empty()
            && at.len() == as_.len()
            && let Some(ph) = self.defeq(g, ht, hs)?
            && let Some(p) = self.args_after(g, ph, &at, &as_)?
        {
            return Ok(Some(self.around(pt, p, ps)));
        }
        // try_eta_expansion
        if let TermRow::Lam(d, _) = self.t.terms[tn as usize]
            && !matches!(self.t.terms[sn as usize], TermRow::Lam(..))
            && let Some(p) = self.eta(g, tn, d, sn)?
        {
            return Ok(Some(self.around(pt, p, ps)));
        }
        if let TermRow::Lam(d, _) = self.t.terms[sn as usize]
            && !matches!(self.t.terms[tn as usize], TermRow::Lam(..))
            && let Some(p) = self.eta(g, sn, d, tn)?
        {
            let p = self.symm(p);
            return Ok(Some(self.around(pt, p, ps)));
        }
        // try_eta_struct
        if let Some(p) = self.eta_struct(g, tn, sn)? {
            return Ok(Some(self.around(pt, p, ps)));
        }
        if let Some(p) = self.eta_struct(g, sn, tn)? {
            let p = self.symm(p);
            return Ok(Some(self.around(pt, p, ps)));
        }
        // try_string_lit_expansion
        for (x, y, flip) in [(tn, sn, false), (sn, tn, true)] {
            let (h, args) = self.spine(y);
            if matches!(self.t.terms[x as usize], TermRow::Str(_))
                && args.len() == 1
                && self.prim_of(h) == Some(Prim::StringMk)
            {
                let Some((p, c)) = self.lit_as_ctor(g, x) else { continue };
                let (q, w) = self.whnf(g, c);
                let Some(core) = self.defeq_core(g, w, y)? else { return Ok(None) };
                let px = self.trans(p, q);
                let r = self.trans(px, core);
                let r = if flip { self.symm(r) } else { r };
                return Ok(Some(self.around(pt, r, ps)));
            }
        }
        if let Some(p) = self.unit_like(g, tn, sn)? {
            return Ok(Some(self.around(pt, p, ps)));
        }
        Ok(None)
    }
}

impl Builder {
    /// `Eq(g, a, b)` by proof irrelevance, if it applies.
    pub(crate) fn proof_irrel(&mut self, g: Ix, a: Ix, b: Ix) -> Res<Option<Ix>> {
        Ok(match self.proof_irrel_l(g, a, b)? {
            L::True(p) => Some(p),
            _ => None,
        })
    }
}
