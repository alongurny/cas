//! The prover's side of an inductive block: Lean's `add_inductive` run by
//! the small kernel, every step recorded on the tape in the order the
//! checker (`crate::block`) consults it, and the recursors built.

use super::{Builder, Res};
use crate::level::{Level, equivalent, geq};
use crate::tables::*;

/// Free variables the recursor's construction binds and no context holds:
/// motives, minor premises, the major premise and induction hypotheses.
const AUX: u32 = 1 << 30;

#[derive(Clone, Copy)]
struct Fv {
    n: u32,
    term: Ix,
    ty: Ix,
}

struct Ctor {
    key: Key,
    fields: Vec<Fv>,
    recs: Vec<(usize, usize, Vec<Ix>, Vec<Fv>)>,
    indices: Vec<Ix>,
}

struct St {
    tape: Vec<Ix>,
    keys: Vec<Key>,
    nlv: u32,
    params: Vec<Fv>,
    nindices: Vec<usize>,
}

impl Builder {
    fn t_whnf(&mut self, st: &mut St, g: Ix, t: Ix) -> Ix {
        let (p, w) = self.whnf(g, t);
        st.tape.push(p);
        w
    }
    fn t_infer(&mut self, st: &mut St, g: Ix, e: Ix) -> Res<Ix> {
        let (p, t) = self.infer(g, e)?;
        st.tape.push(p);
        Ok(t)
    }
    fn t_inst(&mut self, st: &mut St, b: Ix, x: Ix) -> Ix {
        let (p, r) = self.inst(b, 0, x);
        st.tape.push(p);
        r
    }
    fn t_defeq(&mut self, st: &mut St, g: Ix, a: Ix, b: Ix) -> Res<()> {
        let p = self.defeq(g, a, b)?.ok_or("the parameters of the block differ")?;
        st.tape.push(p);
        Ok(())
    }
    fn t_sort_level(&mut self, st: &mut St, g: Ix, e: Ix) -> Res<Ix> {
        let s = self.t_infer(st, g, e)?;
        let w = self.t_whnf(st, g, s);
        match self.t.terms[w as usize] {
            TermRow::Sort(l) => Ok(l),
            _ => Err("not a type".into()),
        }
    }
    fn fresh(&mut self, g: Ix, a: Ix) -> (Ix, Fv) {
        let n = self.clen[g as usize];
        let term = self.term(TermRow::FVar(n));
        (self.ctx(CtxRow::Push(g, a)), Fv { n, term, ty: a })
    }

    fn has_occ(&self, st: &St, e: Ix) -> bool {
        match self.t.terms[e as usize].clone() {
            TermRow::Const(d, _) => st.keys.contains(&self.t.decls[d as usize].key),
            TermRow::App(a, b) | TermRow::Lam(a, b) | TermRow::Pi(a, b) => self.has_occ(st, a) || self.has_occ(st, b),
            TermRow::Let(a, v, b) => self.has_occ(st, a) || self.has_occ(st, v) || self.has_occ(st, b),
            TermRow::Proj(_, _, x) => self.has_occ(st, x),
            _ => false,
        }
    }

    fn ind_app(&self, st: &St, t: Ix) -> Option<(usize, Vec<Ix>)> {
        let (h, args) = self.spine(t);
        let TermRow::Const(d, ls) = self.t.terms[h as usize] else { return None };
        let i = st.keys.iter().position(|k| *k == self.t.decls[d as usize].key)?;
        let ls = self.levels_of(ls);
        let own = ls.len() == st.nlv as usize
            && ls.iter().enumerate().all(|(j, l)| self.t.levels[*l as usize] == LevelRow::Param(j as u32));
        let np = st.params.len();
        if !own || args.len() != np + st.nindices[i] {
            return None;
        }
        if !args[..np].iter().zip(&st.params).all(|(a, p)| *a == p.term)
            || args[np..].iter().any(|a| self.has_occ(st, *a))
        {
            return None;
        }
        Some((i, args[np..].to_vec()))
    }

    fn positive(&mut self, st: &mut St, g: Ix, t: Ix) -> Res<()> {
        let w = self.t_whnf(st, g, t);
        if !self.has_occ(st, w) {
            return Ok(());
        }
        if let TermRow::Pi(a, b) = self.t.terms[w as usize] {
            if self.has_occ(st, a) {
                return Err("a non positive occurrence of the block".into());
            }
            let (ext, x) = self.fresh(g, a);
            let body = self.t_inst(st, b, x.term);
            return self.positive(st, ext, body);
        }
        self.ind_app(st, w).map(|_| ()).ok_or_else(|| "an invalid occurrence of the block".into())
    }

    fn rec_arg(&mut self, st: &mut St, g: Ix, t: Ix) -> Option<(usize, Vec<Ix>, Vec<Fv>)> {
        let mut g = g;
        let mut w = self.t_whnf(st, g, t);
        let mut xs = vec![];
        while let TermRow::Pi(a, b) = self.t.terms[w as usize] {
            let (ext, x) = self.fresh(g, a);
            let body = self.t_inst(st, b, x.term);
            g = ext;
            xs.push(x);
            w = self.t_whnf(st, g, body);
        }
        self.ind_app(st, w).map(|(i, idx)| (i, idx, xs))
    }

    /// Checks the inductive block whose types are the cited rows `types`
    /// (each of kind inductive, its constructors cited as constructor rows
    /// and named in `ctors`), sets their information, cites the recursors
    /// under `rec_keys` and returns the goal the tables establish.
    pub fn inductive_block(
        &mut self,
        levels: u32,
        params: u32,
        types: &[Ix],
        ctors: &[Vec<Ix>],
        rec_keys: &[Key],
    ) -> Res<BlockGoal> {
        let np = params as usize;
        let keys: Vec<Key> = types.iter().map(|d| self.t.decls[*d as usize].key).collect();
        let mut st = St { tape: vec![], keys, nlv: levels, params: vec![], nindices: vec![] };
        let empty = self.empty();
        let mut pctx = empty;
        let mut result: Option<Level> = None;
        let mut type_indices = vec![];
        for (idx, d) in types.iter().enumerate() {
            let ty = self.t.decls[*d as usize].ty;
            self.t_infer(&mut st, empty, ty)?;
            let mut g = empty;
            let mut t = self.t_whnf(&mut st, g, ty);
            let (mut i, mut indices) = (0, vec![]);
            while let TermRow::Pi(a, b) = self.t.terms[t as usize] {
                let x = if i < np {
                    if idx == 0 {
                        let (ext, x) = self.fresh(g, a);
                        st.params.push(x);
                        pctx = ext;
                        g = ext;
                        x
                    } else {
                        let p = st.params[i];
                        self.t_defeq(&mut st, g, a, p.ty)?;
                        g = self.ctx(CtxRow::Push(g, p.ty));
                        p
                    }
                } else {
                    let (ext, x) = self.fresh(g, a);
                    indices.push(x);
                    g = ext;
                    x
                };
                let body = self.t_inst(&mut st, b, x.term);
                t = self.t_whnf(&mut st, g, body);
                i += 1;
            }
            if st.params.len() != np {
                return Err("the number of parameters".into());
            }
            let TermRow::Sort(l) = self.t.terms[t as usize] else { return Err("a type that is not a sort".into()) };
            let l = self.to_level(l);
            match &result {
                None => result = Some(l),
                Some(r) if equivalent(r, &l) => {}
                _ => return Err("the types of a block live in different universes".into()),
            }
            st.nindices.push(indices.len());
            type_indices.push(indices);
        }
        let result = result.unwrap();
        let recursive = ctors.iter().flatten().any(|c| {
            let mut t = self.t.decls[*c as usize].ty;
            while let TermRow::Pi(a, b) = self.t.terms[t as usize] {
                if self.has_occ(&st, a) {
                    return true;
                }
                t = b;
            }
            false
        });
        for (idx, d) in types.iter().enumerate() {
            let ctor_keys = ctors[idx].iter().map(|c| self.t.decls[*c as usize].key).collect();
            self.t.decls[*d as usize].info =
                Info::Induct { params, indices: st.nindices[idx] as u32, ctors: ctor_keys, recursive };
        }
        let mut built: Vec<Vec<Ctor>> = vec![];
        for (idx, cs) in ctors.iter().enumerate() {
            let mut out = vec![];
            for c in cs {
                let row = self.t.decls[*c as usize].clone();
                self.t_infer(&mut st, empty, row.ty)?;
                let (mut t, mut g, mut i, mut fields) = (row.ty, empty, 0, vec![]);
                while let TermRow::Pi(a, b) = self.t.terms[t as usize] {
                    if i < np {
                        let p = st.params[i];
                        self.t_defeq(&mut st, g, a, p.ty)?;
                        g = self.ctx(CtxRow::Push(g, p.ty));
                        t = self.t_inst(&mut st, b, p.term);
                    } else {
                        let s = self.t_sort_level(&mut st, g, a)?;
                        if !(geq(&result, &self.to_level(s)) || result == Level::Zero) {
                            return Err("a field's universe is too big".into());
                        }
                        self.positive(&mut st, g, a)?;
                        let (ext, x) = self.fresh(g, a);
                        fields.push(x);
                        g = ext;
                        t = self.t_inst(&mut st, b, x.term);
                    }
                    i += 1;
                }
                let (it, indices) = self.ind_app(&st, t).ok_or("a constructor that does not build its type")?;
                if it != idx {
                    return Err("a constructor of another type".into());
                }
                self.t.decls[*c as usize].info =
                    Info::Ctor { induct: st.keys[idx], params, fields: fields.len() as u32 };
                out.push(Ctor { key: row.key, fields, recs: vec![], indices });
            }
            built.push(out);
        }
        let elim_zero = if result.is_not_zero() {
            false
        } else if types.len() > 1 || ctors[0].len() > 1 {
            true
        } else if ctors[0].is_empty() {
            false
        } else {
            let ty = self.t.decls[ctors[0][0] as usize].ty;
            let (mut t, mut g, mut i, mut to_check) = (ty, empty, 0, vec![]);
            while let TermRow::Pi(a, b) = self.t.terms[t as usize] {
                let zero = if i >= np {
                    let s = self.t_sort_level(&mut st, g, a)?;
                    self.t.levels[s as usize] == LevelRow::Zero
                } else {
                    true
                };
                let (ext, x) = self.fresh(g, a);
                if !zero {
                    to_check.push(x);
                }
                g = ext;
                t = self.t_inst(&mut st, b, x.term);
                i += 1;
            }
            let (_, args) = self.spine(t);
            to_check.iter().any(|x| !args.contains(&x.term))
        };
        let shift = if elim_zero { 0 } else { 1 };
        for cs in built.iter_mut() {
            for c in cs.iter_mut() {
                let mut g = pctx;
                let mut flags = vec![];
                for x in &c.fields {
                    flags.push(self.rec_arg(&mut st, g, x.ty).is_some());
                    g = self.ctx(CtxRow::Push(g, x.ty));
                }
                for (k, rec) in flags.into_iter().enumerate() {
                    if rec {
                        let x = c.fields[k];
                        let t = self.t_infer(&mut st, g, x.term)?;
                        let (it, idx, xs) = self.rec_arg(&mut st, g, t).ok_or("a recursive field is not")?;
                        c.recs.push((k, it, idx, xs));
                    }
                }
            }
        }
        let recursors = self.build_recursors(&st, types, &built, &type_indices, shift, rec_keys)?;
        Ok(BlockGoal { levels, params, types: types.to_vec(), recursors, tape: st.tape })
    }

    /// `e` with level parameter n renamed n + shift.
    fn lshift(&mut self, e: Ix, shift: u32) -> Ix {
        if shift == 0 || !self.has_params(e) {
            return e;
        }
        let ls: Vec<Ix> = (0..64).map(|i| self.param(i + shift)).collect();
        let ls = self.list(&ls);
        self.linst_quiet(e, ls)
    }

    /// `e` with level parameters replaced by the list `ls`, without rows.
    fn linst_quiet(&mut self, e: Ix, ls: Ix) -> Ix {
        if !self.has_params(e) {
            return e;
        }
        match self.t.terms[e as usize].clone() {
            TermRow::Sort(l) => {
                let l2 = self.linstl_quiet(l, ls);
                self.sort(l2)
            }
            TermRow::Const(d, ms) => {
                let out: Vec<Ix> = self.levels_of(ms).into_iter().map(|m| self.linstl_quiet(m, ls)).collect();
                self.constant(d, &out)
            }
            TermRow::App(a, b) => {
                let (a, b) = (self.linst_quiet(a, ls), self.linst_quiet(b, ls));
                self.app(a, b)
            }
            TermRow::Lam(a, b) => {
                let (a, b) = (self.linst_quiet(a, ls), self.linst_quiet(b, ls));
                self.lam(a, b)
            }
            TermRow::Pi(a, b) => {
                let (a, b) = (self.linst_quiet(a, ls), self.linst_quiet(b, ls));
                self.pi(a, b)
            }
            TermRow::Let(a, v, b) => {
                let (a, v, b) = (self.linst_quiet(a, ls), self.linst_quiet(v, ls), self.linst_quiet(b, ls));
                self.term(TermRow::Let(a, v, b))
            }
            TermRow::Proj(s, i, x) => {
                let x = self.linst_quiet(x, ls);
                self.term(TermRow::Proj(s, i, x))
            }
            _ => e,
        }
    }
    fn linstl_quiet(&mut self, l: Ix, ls: Ix) -> Ix {
        match self.t.levels[l as usize].clone() {
            LevelRow::Zero => l,
            LevelRow::Succ(a) => {
                let a = self.linstl_quiet(a, ls);
                self.succ(a)
            }
            LevelRow::Max(a, b) => {
                let (a, b) = (self.linstl_quiet(a, ls), self.linstl_quiet(b, ls));
                self.level(LevelRow::Max(a, b))
            }
            LevelRow::IMax(a, b) => {
                let (a, b) = (self.linstl_quiet(a, ls), self.linstl_quiet(b, ls));
                self.level(LevelRow::IMax(a, b))
            }
            LevelRow::Param(n) => self.levels_of(ls)[n as usize],
        }
    }

    /// Binds the free variables `vars` (level, type) around `body`.
    fn bind(&mut self, pi: bool, vars: &[(u32, Ix)], body: Ix) -> Ix {
        let mut r = body;
        for (n, ty) in vars.iter().rev() {
            let b = self.abstract_fvar(r, *n, 0);
            r = if pi { self.pi(*ty, b) } else { self.lam(*ty, b) };
        }
        r
    }

    fn build_recursors(
        &mut self,
        st: &St,
        types: &[Ix],
        ctors: &[Vec<Ctor>],
        indices: &[Vec<Fv>],
        shift: u32,
        rec_keys: &[Key],
    ) -> Res<Vec<Ix>> {
        let nlv = st.nlv;
        let block_lv: Vec<Ix> = (0..nlv).map(|i| self.param(i + shift)).collect();
        let rec_lv: Vec<Ix> = (0..nlv + shift).map(|i| self.param(i)).collect();
        let elim = if shift == 1 { self.param(0) } else { self.zero() };
        let mut aux = AUX;
        let mut fresh = |b: &mut Builder| {
            aux += 1;
            (aux, b.term(TermRow::FVar(aux)))
        };
        let sh = |b: &mut Builder, x: &Fv| (x.n, b.lshift(x.ty, shift));
        let params: Vec<(u32, Ix)> = st.params.iter().map(|x| sh(self, x)).collect();
        let pvars: Vec<Ix> = st.params.iter().map(|x| x.term).collect();
        let mut motives = vec![];
        let mut majors = vec![];
        for (i, idx) in indices.iter().enumerate() {
            let ind = self.constant(types[i], &block_lv);
            let ivars: Vec<Ix> = idx.iter().map(|x| x.term).collect();
            let a = self.apps(ind, &pvars);
            let major = self.apps(a, &ivars);
            let (mn, mt) = fresh(self);
            let sort = self.sort(elim);
            let inner = self.bind(true, &[(mn, major)], sort);
            let ib: Vec<(u32, Ix)> = idx.iter().map(|x| sh(self, x)).collect();
            let c_ty = self.bind(true, &ib, inner);
            let (cn, ct) = fresh(self);
            motives.push((cn, c_ty, ct));
            majors.push((major, mn, mt));
        }
        let mut minors = vec![];
        for (i, cs) in ctors.iter().enumerate() {
            for c in cs {
                let cd = self.decls[&c.key];
                let k = self.constant(cd, &block_lv);
                let fvars: Vec<Ix> = c.fields.iter().map(|x| x.term).collect();
                let a = self.apps(k, &pvars);
                let intro = self.apps(a, &fvars);
                let ci: Vec<Ix> = c.indices.iter().map(|e| self.lshift(*e, shift)).collect();
                let m = self.apps(motives[i].2, &ci);
                let c_app = self.app(m, intro);
                let mut ihs = vec![];
                for (k, it, idx, xs) in &c.recs {
                    let xv: Vec<Ix> = xs.iter().map(|x| x.term).collect();
                    let u = self.apps(c.fields[*k].term, &xv);
                    let ii: Vec<Ix> = idx.iter().map(|e| self.lshift(*e, shift)).collect();
                    let m = self.apps(motives[*it].2, &ii);
                    let body = self.app(m, u);
                    let xb: Vec<(u32, Ix)> = xs.iter().map(|x| sh(self, x)).collect();
                    let ih_ty = self.bind(true, &xb, body);
                    let (hn, _) = fresh(self);
                    ihs.push((hn, ih_ty));
                }
                let inner = self.bind(true, &ihs, c_app);
                let fb: Vec<(u32, Ix)> = c.fields.iter().map(|x| sh(self, x)).collect();
                let ty = self.bind(true, &fb, inner);
                let (mn, mt) = fresh(self);
                minors.push((mn, ty, mt));
            }
        }
        let mbind: Vec<(u32, Ix)> = motives.iter().map(|(n, t, _)| (*n, *t)).collect();
        let nbind: Vec<(u32, Ix)> = minors.iter().map(|(n, t, _)| (*n, *t)).collect();
        let mut recs = vec![];
        for (d, idx) in indices.iter().enumerate() {
            let (major, mn, mt) = majors[d];
            let ivars: Vec<Ix> = idx.iter().map(|x| x.term).collect();
            let m = self.apps(motives[d].2, &ivars);
            let c_app = self.app(m, mt);
            let tail = self.bind(true, &[(mn, major)], c_app);
            let ib: Vec<(u32, Ix)> = idx.iter().map(|x| sh(self, x)).collect();
            let tail = self.bind(true, &ib, tail);
            let tail = self.bind(true, &nbind, tail);
            let tail = self.bind(true, &mbind, tail);
            let ty = self.bind(true, &params, tail);
            let info = Info::Rec {
                params: st.params.len() as u32,
                motives: motives.len() as u32,
                minors: minors.len() as u32,
                indices: idx.len() as u32,
                rules: vec![],
            };
            let r = self.decl(DeclRow {
                key: rec_keys[d],
                kind: DeclKind::Recursor,
                levels: nlv + shift,
                ty,
                value: None,
                info,
            });
            recs.push(r);
        }
        let mut minor_idx = 0;
        for (d, cs) in ctors.iter().enumerate() {
            let mut rules = vec![];
            for c in cs {
                let mut args: Vec<Ix> = c.fields.iter().map(|x| x.term).collect();
                for (k, it, idx, xs) in &c.recs {
                    let rc = self.constant(recs[*it], &rec_lv);
                    let mut a = pvars.clone();
                    a.extend(motives.iter().map(|m| m.2));
                    a.extend(minors.iter().map(|m| m.2));
                    a.extend(idx.iter().map(|e| self.lshift(*e, shift)));
                    let xv: Vec<Ix> = xs.iter().map(|x| x.term).collect();
                    let u = self.apps(c.fields[*k].term, &xv);
                    a.push(u);
                    let rec_app = self.apps(rc, &a);
                    let xb: Vec<(u32, Ix)> = xs.iter().map(|x| sh(self, x)).collect();
                    args.push(self.bind(false, &xb, rec_app));
                }
                let e_app = self.apps(minors[minor_idx].2, &args);
                let fb: Vec<(u32, Ix)> = c.fields.iter().map(|x| sh(self, x)).collect();
                let r = self.bind(false, &fb, e_app);
                let r = self.bind(false, &nbind, r);
                let r = self.bind(false, &mbind, r);
                let rhs = self.bind(false, &params, r);
                rules.push(RecRule { ctor: c.key, fields: c.fields.len() as u32, rhs });
                minor_idx += 1;
            }
            if let Info::Rec { rules: r, .. } = &mut self.t.decls[recs[d] as usize].info {
                *r = rules;
            }
        }
        Ok(recs)
    }
}
