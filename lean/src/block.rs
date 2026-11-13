//! The check of an inductive block (spec/lean/tables.md, "inductive
//! block"): Lean's `add_inductive` (kernel/inductive.cpp) run with every
//! `whnf`, `infer`, `instantiate` and `is_def_eq` it makes answered by the
//! next row of the goal's tape, whose own check is that of any row. The
//! recursors' types and rules are then built as Lean builds them and
//! compared by id with the rows given. Nested inductive types are not
//! covered yet: their occurrences fail the positivity check.

use crate::check::{Checker, R, need};
use crate::fp::{Field, Id, limbs};
use crate::level::{Level, equivalent, geq};
use crate::tables::*;
use std::collections::HashMap;
use std::rc::Rc;

/// A variable of the terms the check builds: a free variable of the
/// tables' contexts, or one of the recursor's motives, minor premises or
/// its major premise, which have no context of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Var {
    Table(u32),
    Motive(u32),
    Minor(u32),
    Ih(u32),
    Major,
}

/// A level of a built term: zero or a parameter of the recursor.
#[derive(Clone, Copy, Debug)]
enum Lv {
    Zero,
    Param(u32),
}

/// A term the check builds, from table rows and variables.
#[derive(Clone, Debug)]
enum V {
    /// A term row; its free variables are `Var::Table`.
    Row(Ix),
    Var(Var),
    Const(Key, Vec<Lv>),
    Sort(Lv),
    App(Rc<V>, Rc<V>),
    /// Binds the variable in the body.
    Pi(Var, Rc<V>, Rc<V>),
    Lam(Var, Rc<V>, Rc<V>),
}

fn app(f: V, args: &[V]) -> V {
    args.iter().fold(f, |f, a| V::App(Rc::new(f), Rc::new(a.clone())))
}

/// Binds `vars` (each with its type) around `body`, outermost first.
fn binds(pi: bool, vars: &[(Var, V)], body: V) -> V {
    vars.iter().rev().fold(body, |b, (x, t)| {
        if pi { V::Pi(*x, Rc::new(t.clone()), Rc::new(b)) } else { V::Lam(*x, Rc::new(t.clone()), Rc::new(b)) }
    })
}

/// A free variable of the tables, with the term row that is it and its type.
#[derive(Clone, Copy, Debug)]
struct Fv {
    n: u32,
    term: Ix,
    ty: Ix,
}

impl Fv {
    fn var(&self) -> V {
        V::Var(Var::Table(self.n))
    }
    fn bound(&self) -> (Var, V) {
        (Var::Table(self.n), V::Row(self.ty))
    }
}

/// A recursive argument: the type it is a function to (its block
/// position and indices), and the function's arguments.
type RecArg = (usize, Vec<Ix>, Vec<Fv>);

struct Ctor {
    key: Key,
    fields: Vec<Fv>,
    /// The recursive fields: position in `fields`, then as `RecArg`.
    recs: Vec<(usize, usize, Vec<Ix>, Vec<Fv>)>,
    /// The type it builds: its block position and indices.
    indices: Vec<Ix>,
}

struct Block<'c, 'a> {
    c: &'c Checker<'a>,
    goal: &'c BlockGoal,
    tape: usize,
    keys: Vec<Key>,
    nindices: Vec<usize>,
    params: Vec<Fv>,
    /// The context holding the parameters, and the empty one.
    pctx: Ix,
    empty: Ix,
    by_tid: HashMap<Id, Ix>,
    ctx_by: HashMap<(Id, Id), Ix>,
    occ: std::cell::RefCell<HashMap<Ix, bool>>,
}

impl<'a> Checker<'a> {
    /// Checks that `goal`'s rows are the inductive block Lean's kernel adds.
    pub fn check_block(tables: &'a Tables, goal: &'a BlockGoal) -> R {
        let c = Checker::new(tables, goal.levels, &format!("{goal:?}"))?;
        let mut by_tid = HashMap::new();
        for (i, id) in c.tid.iter().enumerate() {
            by_tid.entry(*id).or_insert(i as Ix);
        }
        let mut ctx_by = HashMap::new();
        let mut empty = None;
        for (i, g) in tables.ctxs.iter().enumerate() {
            match g {
                CtxRow::Empty => empty = Some(i as Ix),
                CtxRow::Push(q, a) => {
                    ctx_by.entry((c.cid[*q as usize], c.tid[*a as usize])).or_insert(i as Ix);
                }
            }
        }
        let empty = empty.ok_or("no empty context")?;
        let mut b = Block {
            c: &c,
            goal,
            tape: 0,
            keys: vec![],
            nindices: vec![],
            params: vec![],
            pctx: empty,
            empty,
            by_tid,
            ctx_by,
            occ: Default::default(),
        };
        b.run().map_err(|e| format!("block: {e}"))
    }
}

impl Block<'_, '_> {
    fn t(&self) -> &Tables {
        self.c.t
    }
    fn term(&self, e: Ix) -> &TermRow {
        &self.t().terms[e as usize]
    }
    fn same(&self, a: Ix, b: Ix) -> bool {
        self.c.tid[a as usize] == self.c.tid[b as usize]
    }
    fn decl(&self, d: Ix) -> Result<&DeclRow, String> {
        self.t().decls.get(d as usize).ok_or_else(|| "no such declaration".into())
    }
    fn decl_by_key(&self, k: &Key) -> Result<&DeclRow, String> {
        self.t().decls.iter().find(|d| &d.key == k).ok_or_else(|| "a constructor is not cited".into())
    }

    // ---- The tape ----

    fn next(&mut self) -> Result<Fact, String> {
        let i = *self.goal.tape.get(self.tape).ok_or("the tape ends early")?;
        self.tape += 1;
        Ok(self.t().rows.get(i as usize).ok_or("no such row")?.fact)
    }
    /// `whnf(t)` in context g.
    fn whnf(&mut self, g: Ix, t: Ix) -> Result<Ix, String> {
        match self.next()? {
            Fact::Eq { g: g2, a, b } if self.c.cid[g as usize] == self.c.cid[g2 as usize] && self.same(a, t) => Ok(b),
            f => Err(format!("tape {}: {f:?} is not a reduction of the term needed", self.tape - 1)),
        }
    }
    /// `infer(e)` in context g.
    fn infer(&mut self, g: Ix, e: Ix) -> Result<Ix, String> {
        match self.next()? {
            Fact::Infer { g: g2, e: e2, t }
                if self.c.cid[g as usize] == self.c.cid[g2 as usize] && self.same(e, e2) =>
            {
                Ok(t)
            }
            f => Err(format!("tape {}: {f:?} is not the typing needed", self.tape - 1)),
        }
    }
    fn defeq(&mut self, g: Ix, a: Ix, b: Ix) -> R {
        match self.next()? {
            Fact::Eq { g: g2, a: a2, b: b2 }
                if self.c.cid[g as usize] == self.c.cid[g2 as usize] && self.same(a, a2) && self.same(b, b2) =>
            {
                Ok(())
            }
            f => Err(format!("tape {}: {f:?} is not the equality needed", self.tape - 1)),
        }
    }
    /// `instantiate(body, x)`.
    fn inst(&mut self, body: Ix, x: Ix) -> Result<Ix, String> {
        match self.next()? {
            Fact::Inst { e, k: 0, a, r } if self.same(e, body) && self.same(a, x) => Ok(r),
            f => Err(format!("tape {}: {f:?} is not the substitution needed", self.tape - 1)),
        }
    }
    /// `ensure_type(e)`: the level of e's sort.
    fn sort_level(&mut self, g: Ix, e: Ix) -> Result<Ix, String> {
        let s = self.infer(g, e)?;
        let w = self.whnf(g, s)?;
        match *self.term(w) {
            TermRow::Sort(l) => Ok(l),
            _ => Err("not a type".into()),
        }
    }
    /// A new free variable of type `a` after context g: the context with
    /// it, and the variable.
    fn push(&self, g: Ix, a: Ix) -> Result<(Ix, Fv), String> {
        let ext = *self.ctx_by.get(&(self.c.cid[g as usize], self.c.tid[a as usize])).ok_or("a context is missing")?;
        let n = self.c.clen[g as usize];
        let id = self.c.point.id(&[Field::Num(1), Field::Num(n as u64)]);
        let term = *self.by_tid.get(&id).ok_or("a variable is missing")?;
        Ok((ext, Fv { n, term, ty: a }))
    }

    // ---- Occurrences ----

    /// Whether e mentions one of the block's types.
    fn has_occ(&self, e: Ix) -> bool {
        if let Some(r) = self.occ.borrow().get(&e) {
            return *r;
        }
        let r = match self.term(e).clone() {
            TermRow::Const(d, _) => self.keys.contains(&self.t().decls[d as usize].key),
            TermRow::App(a, b) | TermRow::Lam(a, b) | TermRow::Pi(a, b) => self.has_occ(a) || self.has_occ(b),
            TermRow::Let(a, v, b) => self.has_occ(a) || self.has_occ(v) || self.has_occ(b),
            TermRow::Proj(_, _, x) => self.has_occ(x),
            _ => false,
        };
        self.occ.borrow_mut().insert(e, r);
        r
    }

    fn spine(&self, mut e: Ix) -> (Ix, Vec<Ix>) {
        let mut args = vec![];
        while let TermRow::App(f, x) = *self.term(e) {
            args.push(x);
            e = f;
        }
        args.reverse();
        (e, args)
    }

    /// `is_valid_ind_app`: t is a type of the block at its own levels,
    /// applied to the parameters and to indices that do not mention the
    /// block. Returns its position and indices.
    fn ind_app(&self, t: Ix) -> Option<(usize, Vec<Ix>)> {
        let (h, args) = self.spine(t);
        let TermRow::Const(d, ls) = *self.term(h) else { return None };
        let i = self.keys.iter().position(|k| *k == self.t().decls[d as usize].key)?;
        let ls = self.c.list(ls).ok()?;
        let own = ls.len() == self.goal.levels as usize
            && ls.iter().enumerate().all(|(j, l)| self.t().levels[*l as usize] == LevelRow::Param(j as u32));
        let np = self.params.len();
        if !own || args.len() != np + self.nindices[i] {
            return None;
        }
        if !args[..np].iter().zip(&self.params).all(|(a, p)| self.same(*a, p.term)) {
            return None;
        }
        if args[np..].iter().any(|a| self.has_occ(*a)) {
            return None;
        }
        Some((i, args[np..].to_vec()))
    }

    /// `check_positivity` of the argument type t in context g.
    fn positive(&mut self, g: Ix, t: Ix) -> R {
        let w = self.whnf(g, t)?;
        if !self.has_occ(w) {
            return Ok(());
        }
        if let TermRow::Pi(a, b) = *self.term(w) {
            need(!self.has_occ(a), "a non positive occurrence of the block")?;
            let (ext, x) = self.push(g, a)?;
            let body = self.inst(b, x.term)?;
            return self.positive(ext, body);
        }
        need(self.ind_app(w).is_some(), "an invalid occurrence of the block")
    }

    /// `is_rec_argument` of the argument type t in context g: the type it
    /// is a function to, its indices and the function's arguments.
    fn rec_arg(&mut self, g: Ix, t: Ix) -> Result<Option<RecArg>, String> {
        let mut g = g;
        let mut w = self.whnf(g, t)?;
        let mut xs = vec![];
        while let TermRow::Pi(a, b) = *self.term(w) {
            let (ext, x) = self.push(g, a)?;
            let body = self.inst(b, x.term)?;
            g = ext;
            xs.push(x);
            w = self.whnf(g, body)?;
        }
        Ok(self.ind_app(w).map(|(i, idx)| (i, idx, xs)))
    }

    fn run(&mut self) -> R {
        let goal = self.goal;
        let np = goal.params as usize;
        need(!goal.types.is_empty() && goal.recursors.len() == goal.types.len(), "the types and recursors")?;
        for d in &goal.types {
            let row = self.decl(*d)?;
            need(row.kind == DeclKind::Inductive && row.levels == goal.levels, "not an inductive type")?;
            self.keys.push(row.key);
        }
        // check_inductive_types
        let mut result: Option<Level> = None;
        let mut type_indices: Vec<Vec<Fv>> = vec![];
        for (idx, d) in goal.types.iter().enumerate() {
            let ty = self.decl(*d)?.ty;
            self.infer(self.empty, ty)?;
            let mut g = self.empty;
            let mut t = self.whnf(g, ty)?;
            let mut i = 0;
            let mut indices = vec![];
            while let TermRow::Pi(a, b) = *self.term(t) {
                let x = if i < np {
                    if idx == 0 {
                        let (ext, x) = self.push(g, a)?;
                        self.params.push(x);
                        self.pctx = ext;
                        g = ext;
                        x
                    } else {
                        self.defeq(g, a, self.params[i].ty)?;
                        g = *self
                            .ctx_by
                            .get(&(self.c.cid[g as usize], self.c.tid[self.params[i].ty as usize]))
                            .unwrap();
                        self.params[i]
                    }
                } else {
                    let (ext, x) = self.push(g, a)?;
                    indices.push(x);
                    g = ext;
                    x
                };
                let body = self.inst(b, x.term)?;
                t = self.whnf(g, body)?;
                i += 1;
            }
            need(i >= np && (idx > 0 || self.params.len() == np), "the number of parameters")?;
            if np == 0 {
                self.pctx = self.empty;
            }
            let TermRow::Sort(l) = *self.term(t) else { return Err("a type that is not a sort".into()) };
            let l = self.c.level(l)?;
            match &result {
                None => result = Some(l),
                Some(r) => need(equivalent(r, &l), "the types of a block live in different universes")?,
            }
            self.nindices.push(indices.len());
            type_indices.push(indices);
        }
        let result = result.unwrap();
        // check_constructors, with the constructors' fields kept.
        let mut ctors: Vec<Vec<Ctor>> = vec![];
        let mut recursive = false;
        for (idx, d) in goal.types.iter().enumerate() {
            let Info::Induct { params, indices, ctors: keys, recursive: rec } = self.decl(*d)?.info.clone() else {
                return Err("an inductive type without its information".into());
            };
            need(params == goal.params && indices as usize == self.nindices[idx], "the parameters and indices")?;
            let _ = rec;
            let mut cs = vec![];
            for (j, k) in keys.iter().enumerate() {
                need(!keys[..j].contains(k), "a constructor twice")?;
                let row = self.decl_by_key(k)?.clone();
                need(row.kind == DeclKind::Constructor && row.levels == goal.levels, "not a constructor")?;
                self.infer(self.empty, row.ty)?;
                let mut t = row.ty;
                let mut g = self.empty;
                let mut i = 0;
                let mut fields = vec![];
                while let TermRow::Pi(a, b) = *self.term(t) {
                    if self.has_occ(a) {
                        recursive = true;
                    }
                    if i < np {
                        self.defeq(g, a, self.params[i].ty)?;
                        g = *self
                            .ctx_by
                            .get(&(self.c.cid[g as usize], self.c.tid[self.params[i].ty as usize]))
                            .unwrap();
                        t = self.inst(b, self.params[i].term)?;
                    } else {
                        let s = self.sort_level(g, a)?;
                        let s = self.c.level(s)?;
                        need(geq(&result, &s) || result == Level::Zero, "a field's universe is too big")?;
                        self.positive(g, a)?;
                        let (ext, x) = self.push(g, a)?;
                        fields.push(x);
                        g = ext;
                        t = self.inst(b, x.term)?;
                    }
                    i += 1;
                }
                need(i >= np, "too few parameters")?;
                let (it, indices) = self.ind_app(t).ok_or("a constructor that does not build its type")?;
                need(it == idx, "a constructor of another type")?;
                need(
                    row.info == Info::Ctor { induct: self.keys[idx], params: goal.params, fields: fields.len() as u32 },
                    "the constructor's information",
                )?;
                cs.push(Ctor { key: *k, fields, recs: vec![], indices });
            }
            ctors.push(cs);
        }
        for d in &goal.types {
            let Info::Induct { recursive: r, .. } = self.decl(*d)?.info else { unreachable!() };
            need(r == recursive, "whether the block is recursive")?;
        }
        // elim_only_at_universe_zero
        let elim_zero = if result.is_not_zero() {
            false
        } else if goal.types.len() > 1 || ctors[0].len() > 1 {
            true
        } else if ctors[0].is_empty() {
            false
        } else {
            let row = self.decl_by_key(&ctors[0][0].key)?.clone();
            let (mut t, mut g, mut i) = (row.ty, self.empty, 0);
            let mut to_check = vec![];
            while let TermRow::Pi(a, b) = *self.term(t) {
                let zero = if i >= np {
                    let s = self.sort_level(g, a)?;
                    self.t().levels[s as usize] == LevelRow::Zero
                } else {
                    true
                };
                let (ext, x) = self.push(g, a)?;
                if !zero {
                    to_check.push(x);
                }
                g = ext;
                t = self.inst(b, x.term)?;
                i += 1;
            }
            let (_, args) = self.spine(t);
            to_check.iter().any(|x| !args.iter().any(|a| self.same(*a, x.term)))
        };
        let shift = if elim_zero { 0 } else { 1 };
        // The recursive fields (is_rec_argument, in each field's context)
        // and the arguments of each (the whnf of its type, in the context
        // of all fields), as mk_rec_infos finds them.
        for cs in ctors.iter_mut() {
            for ctor in cs.iter_mut() {
                let mut g = self.pctx;
                let mut flags = vec![];
                for x in &ctor.fields {
                    flags.push(self.rec_arg(g, x.ty)?.is_some());
                    g = self.push(g, x.ty)?.0;
                }
                for (k, rec) in flags.into_iter().enumerate() {
                    if rec {
                        let x = ctor.fields[k];
                        let t = self.infer(g, x.term)?;
                        let (it, idx, xs) = self.rec_arg(g, t)?.ok_or("a recursive field is not")?;
                        ctor.recs.push((k, it, idx, xs));
                    }
                }
            }
        }
        // K-like reduction needs no flag: it is derived (see iota).
        self.recursors(&ctors, &type_indices, shift)?;
        need(self.tape == goal.tape.len(), "rows left on the tape")
    }

    /// The recursors' types and rules, built as `declare_recursors` builds
    /// them and compared with the rows given. The recursor has the elim
    /// level first when it is a parameter, so the block's level parameters
    /// move up by `shift`.
    fn recursors(&mut self, ctors: &[Vec<Ctor>], indices: &[Vec<Fv>], shift: u32) -> R {
        let goal = self.goal;
        let nlv = goal.levels;
        let block_lv: Vec<Lv> = (0..nlv).map(|i| Lv::Param(i + shift)).collect();
        let rec_lv: Vec<Lv> = (0..nlv + shift).map(Lv::Param).collect();
        let elim = if shift == 1 { Lv::Param(0) } else { Lv::Zero };
        let params: Vec<(Var, V)> = self.params.iter().map(Fv::bound).collect();
        let pvars: Vec<V> = self.params.iter().map(Fv::var).collect();
        let rows = |xs: &[Ix]| xs.iter().map(|x| V::Row(*x)).collect::<Vec<V>>();
        let vars = |xs: &[Fv]| xs.iter().map(Fv::var).collect::<Vec<V>>();
        let bound = |xs: &[Fv]| xs.iter().map(Fv::bound).collect::<Vec<(Var, V)>>();
        let ind = |i: usize| V::Const(self.keys[i], block_lv.clone());
        let mut rec_keys = vec![];
        for r in &goal.recursors {
            rec_keys.push(self.decl(*r)?.key);
        }
        // The motives: C : (indices) -> I params indices -> Sort elim.
        let mut motives: Vec<(Var, V)> = vec![];
        let mut majors = vec![];
        for (i, idx) in indices.iter().enumerate() {
            let major = app(app(ind(i), &pvars), &vars(idx));
            let c_ty = binds(true, &bound(idx), V::Pi(Var::Major, Rc::new(major.clone()), Rc::new(V::Sort(elim))));
            motives.push((Var::Motive(i as u32), c_ty));
            majors.push(major);
        }
        let mvars: Vec<V> = motives.iter().map(|(x, _)| V::Var(*x)).collect();
        // The minor premises, one per constructor in order.
        let mut minors: Vec<(Var, V)> = vec![];
        for (i, cs) in ctors.iter().enumerate() {
            for c in cs {
                let intro = app(app(V::Const(c.key, block_lv.clone()), &pvars), &vars(&c.fields));
                let c_app = app(app(V::Var(Var::Motive(i as u32)), &rows(&c.indices)), &[intro]);
                let ihs: Vec<(Var, V)> = c
                    .recs
                    .iter()
                    .enumerate()
                    .map(|(j, (k, it, idx, xs))| {
                        let u = app(c.fields[*k].var(), &vars(xs));
                        let c_app = app(app(V::Var(Var::Motive(*it as u32)), &rows(idx)), &[u]);
                        (Var::Ih(j as u32), binds(true, &bound(xs), c_app))
                    })
                    .collect();
                let ty = binds(true, &bound(&c.fields), binds(true, &ihs, c_app));
                minors.push((Var::Minor(minors.len() as u32), ty));
            }
        }
        let minor_vars: Vec<V> = minors.iter().map(|(x, _)| V::Var(*x)).collect();
        let mut minor_idx = 0;
        for (d, r) in goal.recursors.iter().enumerate() {
            let row = self.decl(*r)?.clone();
            need(row.kind == DeclKind::Recursor && row.levels == nlv + shift, "not the recursor's kind or levels")?;
            let c_app = app(app(V::Var(Var::Motive(d as u32)), &vars(&indices[d])), &[V::Var(Var::Major)]);
            let tail = binds(true, &bound(&indices[d]), V::Pi(Var::Major, Rc::new(majors[d].clone()), Rc::new(c_app)));
            let ty = binds(true, &params, binds(true, &motives, binds(true, &minors, tail)));
            need(self.vid(&ty, &mut vec![], shift)? == self.c.tid[row.ty as usize], "not the recursor's type")?;
            let Info::Rec { params: p, motives: m, minors: n, indices: k, rules } = &row.info else {
                return Err("a recursor without its information".into());
            };
            need(
                *p == goal.params
                    && *m as usize == motives.len()
                    && *n as usize == minors.len()
                    && *k as usize == indices[d].len()
                    && rules.len() == ctors[d].len(),
                "the recursor's information",
            )?;
            for (c, rule) in ctors[d].iter().zip(rules) {
                need(rule.ctor == c.key && rule.fields as usize == c.fields.len(), "a rule's constructor")?;
                let mut args = vars(&c.fields);
                for (k, it, idx, xs) in &c.recs {
                    let mut a = pvars.clone();
                    a.extend(mvars.iter().cloned());
                    a.extend(minor_vars.iter().cloned());
                    a.extend(rows(idx));
                    a.push(app(c.fields[*k].var(), &vars(xs)));
                    let rec_app = app(V::Const(rec_keys[*it], rec_lv.clone()), &a);
                    args.push(binds(false, &bound(xs), rec_app));
                }
                let e_app = app(V::Var(Var::Minor(minor_idx)), &args);
                let rhs = binds(
                    false,
                    &params,
                    binds(false, &motives, binds(false, &minors, binds(false, &bound(&c.fields), e_app))),
                );
                need(
                    self.vid(&rhs, &mut vec![], shift)? == self.c.tid[rule.rhs as usize],
                    "not the rule's right-hand side",
                )?;
                minor_idx += 1;
            }
        }
        Ok(())
    }
}

// ---- Ids of built terms ----

impl Block<'_, '_> {
    fn level_id(&self, l: Ix, shift: u32) -> Id {
        use Field::*;
        let f = match self.t().levels[l as usize] {
            LevelRow::Zero => vec![Num(0)],
            LevelRow::Succ(a) => vec![Num(1), Id(self.level_id(a, shift))],
            LevelRow::Max(a, b) => vec![Num(2), Id(self.level_id(a, shift)), Id(self.level_id(b, shift))],
            LevelRow::IMax(a, b) => vec![Num(3), Id(self.level_id(a, shift)), Id(self.level_id(b, shift))],
            LevelRow::Param(n) => vec![Num(4), Num((n + shift) as u64)],
        };
        self.c.point.id(&f)
    }
    fn lv_id(&self, l: Lv) -> Id {
        use Field::*;
        match l {
            Lv::Zero => self.c.point.id(&[Num(0)]),
            Lv::Param(n) => self.c.point.id(&[Num(4), Num(n as u64)]),
        }
    }
    fn list_id(&self, ls: &[Id]) -> Id {
        use Field::*;
        ls.iter().rev().fold(self.c.point.id(&[Num(0)]), |r, h| self.c.point.id(&[Num(1), Id(*h), Id(r)]))
    }
    fn const_id(&self, key: &Key, ls: &[Id]) -> Id {
        let mut f = vec![Field::Num(3)];
        limbs(key, &mut f);
        f.push(Field::Id(self.list_id(ls)));
        self.c.point.id(&f)
    }

    /// The id of v with the variables in `env` bound (innermost last), the
    /// table's level parameters shifted by `shift`.
    fn vid(&self, v: &V, env: &mut Vec<Var>, shift: u32) -> Result<Id, String> {
        use Field::*;
        let p = &self.c.point;
        Ok(match v {
            V::Row(e) => self.row_id(*e, env, 0, shift)?,
            V::Var(x) => {
                let pos = env.iter().rposition(|y| y == x).ok_or("a variable is not bound")?;
                p.id(&[Num(0), Num((env.len() - 1 - pos) as u64)])
            }
            V::Const(k, ls) => {
                let ls: Vec<crate::fp::Id> = ls.iter().map(|l| self.lv_id(*l)).collect();
                self.const_id(k, &ls)
            }
            V::Sort(l) => p.id(&[Num(2), Id(self.lv_id(*l))]),
            V::App(f, a) => {
                let (f, a) = (self.vid(f, env, shift)?, self.vid(a, env, shift)?);
                p.id(&[Num(4), Id(f), Id(a)])
            }
            V::Pi(x, t, b) | V::Lam(x, t, b) => {
                let t = self.vid(t, env, shift)?;
                env.push(*x);
                let b = self.vid(b, env, shift);
                env.pop();
                let k = if matches!(v, V::Lam(..)) { 5 } else { 6 };
                p.id(&[Num(k), Id(t), Id(b?)])
            }
        })
    }

    /// The id of term row e under `depth` of its own binders, its free
    /// variables bound by `env`.
    fn row_id(&self, e: Ix, env: &mut Vec<Var>, depth: u32, shift: u32) -> Result<Id, String> {
        use Field::*;
        let p = &self.c.point;
        let sub = |x: Ix, env: &mut Vec<Var>, d: u32| self.row_id(x, env, d, shift);
        Ok(match self.term(e).clone() {
            TermRow::BVar(n) => {
                need(n < depth, "a loose bound variable")?;
                p.id(&[Num(0), Num(n as u64)])
            }
            TermRow::FVar(n) => {
                let pos = env.iter().rposition(|y| *y == Var::Table(n)).ok_or("a free variable is not bound")?;
                p.id(&[Num(0), Num((env.len() - 1 - pos) as u64 + depth as u64)])
            }
            TermRow::Sort(l) => p.id(&[Num(2), Id(self.level_id(l, shift))]),
            TermRow::Const(d, ls) => {
                let ls: Vec<crate::fp::Id> = self.c.list(ls)?.iter().map(|l| self.level_id(*l, shift)).collect();
                self.const_id(&self.t().decls[d as usize].key, &ls)
            }
            TermRow::App(a, b) => p.id(&[Num(4), Id(sub(a, env, depth)?), Id(sub(b, env, depth)?)]),
            TermRow::Lam(a, b) => p.id(&[Num(5), Id(sub(a, env, depth)?), Id(sub(b, env, depth + 1)?)]),
            TermRow::Pi(a, b) => p.id(&[Num(6), Id(sub(a, env, depth)?), Id(sub(b, env, depth + 1)?)]),
            TermRow::Let(a, v, b) => {
                p.id(&[Num(7), Id(sub(a, env, depth)?), Id(sub(v, env, depth)?), Id(sub(b, env, depth + 1)?)])
            }
            TermRow::Lit(_) | TermRow::Str(_) => self.c.tid[e as usize],
            TermRow::Proj(d, n, x) => {
                let mut f = vec![Num(10)];
                limbs(&self.t().decls[d as usize].key, &mut f);
                f.extend([Num(n as u64), Id(sub(x, env, depth)?)]);
                p.id(&f)
            }
        })
    }
}
