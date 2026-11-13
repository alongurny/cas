//! The table checker (spec/lean/tables.md): every row is checked against
//! its own fields and the rows it names, and nothing else. Terms are
//! compared by fingerprint.

use crate::fp::{Field, Id, Point, limbs};
use crate::level::{Level, equivalent};
use crate::tables::*;

/// Numbers each term row carries, computed from its children.
#[derive(Clone, Copy, Debug, Default)]
struct Meta {
    /// One more than the largest loose bound index; 0 when closed.
    lbr: u32,
    /// One more than the largest free variable level; 0 when there is none.
    fv: u32,
    /// Whether a level parameter occurs.
    lp: bool,
}

pub struct Checker<'a> {
    t: &'a Tables,
    goal: &'a Goal,
    lid: Vec<Id>,
    lparam: Vec<bool>,
    listid: Vec<Id>,
    tid: Vec<Id>,
    meta: Vec<Meta>,
    cid: Vec<Id>,
    clen: Vec<u32>,
}

type R = Result<(), String>;

fn need(ok: bool, what: &str) -> R {
    if ok { Ok(()) } else { Err(what.to_string()) }
}

impl<'a> Checker<'a> {
    /// Checks `goal` against `tables`.
    pub fn check(tables: &'a Tables, goal: &'a Goal) -> R {
        let mut c = Checker {
            t: tables,
            goal,
            lid: vec![],
            lparam: vec![],
            listid: vec![],
            tid: vec![],
            meta: vec![],
            cid: vec![],
            clen: vec![],
        };
        let mut commit = blake3::Hasher::new();
        commit.update(&tables.commitment());
        commit.update(format!("{goal:?}").as_bytes());
        let point = Point::from_commitment(commit.finalize().as_bytes());
        c.ids(&point)?;
        for i in 0..tables.rows.len() {
            c.row(i).map_err(|e| format!("row {i} ({:?} by {:?}): {e}", tables.rows[i].fact, tables.rows[i].rule))?;
        }
        c.goal().map_err(|e| format!("goal: {e}"))
    }

    fn ids(&mut self, p: &Point) -> R {
        use Field::*;
        let t = self.t;
        let before = |i: Ix, n: usize, what: &str| need((i as usize) < n, &format!("{what} {i} is not an earlier row"));
        for (i, l) in t.levels.iter().enumerate() {
            let (f, lp) = match *l {
                LevelRow::Zero => (vec![Num(0)], false),
                LevelRow::Succ(a) => {
                    before(a, i, "level")?;
                    (vec![Num(1), Id(self.lid[a as usize])], self.lparam[a as usize])
                }
                LevelRow::Max(a, b) | LevelRow::IMax(a, b) => {
                    before(a, i, "level")?;
                    before(b, i, "level")?;
                    let k = if matches!(l, LevelRow::Max(..)) { 2 } else { 3 };
                    (
                        vec![Num(k), Id(self.lid[a as usize]), Id(self.lid[b as usize])],
                        self.lparam[a as usize] || self.lparam[b as usize],
                    )
                }
                LevelRow::Param(n) => (vec![Num(4), Num(n as u64)], true),
            };
            self.lid.push(p.id(&f));
            self.lparam.push(lp);
        }
        for (i, l) in t.lists.iter().enumerate() {
            let f = match *l {
                ListRow::Nil => vec![Num(0)],
                ListRow::Cons(h, r) => {
                    before(h, t.levels.len(), "level")?;
                    before(r, i, "list")?;
                    vec![Num(1), Id(self.lid[h as usize]), Id(self.listid[r as usize])]
                }
            };
            self.listid.push(p.id(&f));
        }
        for d in &t.decls {
            before(d.ty, t.terms.len(), "term")?;
            if let Some(v) = d.value {
                before(v, t.terms.len(), "term")?;
            }
        }
        for (i, e) in t.terms.iter().enumerate() {
            let m = |j: Ix| -> Result<Meta, String> {
                before(j, i, "term")?;
                Ok(self.meta[j as usize])
            };
            let (f, meta) = match e {
                TermRow::BVar(n) => (vec![Num(0), Num(*n as u64)], Meta { lbr: n + 1, fv: 0, lp: false }),
                TermRow::FVar(n) => (vec![Num(1), Num(*n as u64)], Meta { lbr: 0, fv: n + 1, lp: false }),
                TermRow::Sort(l) => {
                    before(*l, t.levels.len(), "level")?;
                    (vec![Num(2), Id(self.lid[*l as usize])], Meta { lp: self.lparam[*l as usize], ..Meta::default() })
                }
                TermRow::Const(d, ls) => {
                    before(*d, t.decls.len(), "declaration")?;
                    before(*ls, t.lists.len(), "list")?;
                    let mut f = vec![Num(3)];
                    limbs(&t.decls[*d as usize].key, &mut f);
                    f.push(Id(self.listid[*ls as usize]));
                    let lp = self.list(*ls)?.iter().any(|l| self.lparam[*l as usize]);
                    (f, Meta { lp, ..Meta::default() })
                }
                TermRow::App(a, b) => {
                    let (x, y) = (m(*a)?, m(*b)?);
                    let f = vec![Num(4), Id(self.tid[*a as usize]), Id(self.tid[*b as usize])];
                    (f, Meta { lbr: x.lbr.max(y.lbr), fv: x.fv.max(y.fv), lp: x.lp || y.lp })
                }
                TermRow::Lam(a, b) | TermRow::Pi(a, b) => {
                    let (x, y) = (m(*a)?, m(*b)?);
                    let k = if matches!(e, TermRow::Lam(..)) { 5 } else { 6 };
                    let f = vec![Num(k), Id(self.tid[*a as usize]), Id(self.tid[*b as usize])];
                    (f, Meta { lbr: x.lbr.max(y.lbr.saturating_sub(1)), fv: x.fv.max(y.fv), lp: x.lp || y.lp })
                }
                TermRow::Let(a, v, b) => {
                    let (x, y, z) = (m(*a)?, m(*v)?, m(*b)?);
                    let f =
                        vec![Num(7), Id(self.tid[*a as usize]), Id(self.tid[*v as usize]), Id(self.tid[*b as usize])];
                    let meta = Meta {
                        lbr: x.lbr.max(y.lbr).max(z.lbr.saturating_sub(1)),
                        fv: x.fv.max(y.fv).max(z.fv),
                        lp: x.lp || y.lp || z.lp,
                    };
                    (f, meta)
                }
                TermRow::Lit(n) => {
                    need(n.last() != Some(&0), "a literal has trailing zero bytes")?;
                    let mut f = vec![Num(8)];
                    limbs(n, &mut f);
                    (f, Meta::default())
                }
                TermRow::Str(s) => {
                    let mut f = vec![Num(9)];
                    limbs(s, &mut f);
                    (f, Meta::default())
                }
                TermRow::Proj(d, n, x) => {
                    before(*d, t.decls.len(), "declaration")?;
                    let meta = m(*x)?;
                    let mut f = vec![Num(10)];
                    limbs(&t.decls[*d as usize].key, &mut f);
                    f.extend([Num(*n as u64), Id(self.tid[*x as usize])]);
                    (f, meta)
                }
            };
            self.tid.push(p.id(&f));
            self.meta.push(meta);
        }
        for (i, g) in t.ctxs.iter().enumerate() {
            let (f, len) = match *g {
                CtxRow::Empty => (vec![Num(0)], 0),
                CtxRow::Push(q, a) => {
                    before(q, i, "context")?;
                    before(a, t.terms.len(), "term")?;
                    let n = self.clen[q as usize];
                    // An entry may mention only the entries before it.
                    need(self.meta[a as usize].fv <= n, "a context entry mentions a later variable")?;
                    need(self.meta[a as usize].lbr == 0, "a context entry has a loose bound variable")?;
                    (vec![Num(1), Id(self.cid[q as usize]), Id(self.tid[a as usize])], n + 1)
                }
            };
            self.cid.push(p.id(&f));
            self.clen.push(len);
        }
        Ok(())
    }

    /// The levels of a list, in order.
    fn list(&self, mut ls: Ix) -> Result<Vec<Ix>, String> {
        let mut out = vec![];
        loop {
            match self.t.lists.get(ls as usize).ok_or("no such list")? {
                ListRow::Nil => return Ok(out),
                ListRow::Cons(h, r) => {
                    out.push(*h);
                    ls = *r;
                }
            }
        }
    }

    fn level(&self, l: Ix) -> Result<Level, String> {
        Ok(match *self.t.levels.get(l as usize).ok_or("no such level")? {
            LevelRow::Zero => Level::Zero,
            LevelRow::Succ(a) => self.level(a)?.succ(),
            LevelRow::Max(a, b) => Level::max(self.level(a)?, self.level(b)?),
            LevelRow::IMax(a, b) => Level::imax(self.level(a)?, self.level(b)?),
            LevelRow::Param(n) => Level::Param(n),
        })
    }

    /// The parameters in level `l` are the goal's.
    fn params_in_range(&self, l: Ix) -> R {
        match self.t.levels[l as usize] {
            LevelRow::Zero => Ok(()),
            LevelRow::Succ(a) => self.params_in_range(a),
            LevelRow::Max(a, b) | LevelRow::IMax(a, b) => {
                self.params_in_range(a)?;
                self.params_in_range(b)
            }
            LevelRow::Param(n) => need(n < self.goal.levels, "a level parameter is out of range"),
        }
    }

    fn term(&self, e: Ix) -> Result<&TermRow, String> {
        self.t.terms.get(e as usize).ok_or_else(|| format!("no term {e}"))
    }
    fn same(&self, a: Ix, b: Ix) -> bool {
        self.tid[a as usize] == self.tid[b as usize]
    }
    fn same_level(&self, a: Ix, b: Ix) -> bool {
        self.lid[a as usize] == self.lid[b as usize]
    }
    fn same_list(&self, a: Ix, b: Ix) -> bool {
        self.listid[a as usize] == self.listid[b as usize]
    }
    fn same_ctx(&self, a: Ix, b: Ix) -> bool {
        self.cid[a as usize] == self.cid[b as usize]
    }

    fn check_refs(&self, f: &Fact) -> R {
        let (nt, nc, nl, nls) = (self.t.terms.len(), self.t.ctxs.len(), self.t.levels.len(), self.t.lists.len());
        let ok = match *f {
            Fact::Has { g, a, .. } => (g as usize) < nc && (a as usize) < nt,
            Fact::Infer { g, e, t } => (g as usize) < nc && (e as usize) < nt && (t as usize) < nt,
            Fact::Eq { g, a, b } => (g as usize) < nc && (a as usize) < nt && (b as usize) < nt,
            Fact::Inst { e, a, r, .. } => (e as usize) < nt && (a as usize) < nt && (r as usize) < nt,
            Fact::LInst { e, ls, r } => (e as usize) < nt && (ls as usize) < nls && (r as usize) < nt,
            Fact::LInstL { l, ls, r } => (l as usize) < nl && (ls as usize) < nls && (r as usize) < nl,
            Fact::LEq { l, m } => (l as usize) < nl && (m as usize) < nl,
        };
        need(ok, "a fact names a row that does not exist")
    }

    /// The fact of premise `j` of row `i`.
    fn p(&self, i: usize, j: usize) -> Result<Fact, String> {
        let r = &self.t.rows[i];
        let k = *r.premises.get(j).ok_or_else(|| format!("premise {j} is missing"))? as usize;
        need(k < i, "a premise is not an earlier row")?;
        Ok(self.t.rows[k].fact)
    }

    fn infer(&self, i: usize, j: usize, g: Ix) -> Result<(Ix, Ix), String> {
        match self.p(i, j)? {
            Fact::Infer { g: g2, e, t } if self.same_ctx(g, g2) => Ok((e, t)),
            f => Err(format!("premise {j} is {f:?}, not a typing in the same context")),
        }
    }
    fn eq(&self, i: usize, j: usize, g: Ix) -> Result<(Ix, Ix), String> {
        match self.p(i, j)? {
            Fact::Eq { g: g2, a, b } if self.same_ctx(g, g2) => Ok((a, b)),
            f => Err(format!("premise {j} is {f:?}, not an equality in the same context")),
        }
    }
    /// Premise `j` is `Inst(e, k, a, r)` for this e, k and a; returns r.
    fn inst(&self, i: usize, j: usize, e: Ix, k: u32, a: Ix) -> Result<Ix, String> {
        match self.p(i, j)? {
            Fact::Inst { e: e2, k: k2, a: a2, r } if self.same(e, e2) && k == k2 && self.same(a, a2) => Ok(r),
            f => Err(format!("premise {j} is {f:?}, not the substitution needed")),
        }
    }
    fn linst(&self, i: usize, j: usize, e: Ix, ls: Ix) -> Result<Ix, String> {
        match self.p(i, j)? {
            Fact::LInst { e: e2, ls: ls2, r } if self.same(e, e2) && self.same_list(ls, ls2) => Ok(r),
            f => Err(format!("premise {j} is {f:?}, not the level substitution needed")),
        }
    }
    fn linstl(&self, i: usize, j: usize, l: Ix, ls: Ix) -> Result<Ix, String> {
        match self.p(i, j)? {
            Fact::LInstL { l: l2, ls: ls2, r } if self.same_level(l, l2) && self.same_list(ls, ls2) => Ok(r),
            f => Err(format!("premise {j} is {f:?}, not the level substitution needed")),
        }
    }
    fn leq(&self, i: usize, j: usize, l: Ix, m: Ix) -> R {
        match self.p(i, j)? {
            Fact::LEq { l: l2, m: m2 } if self.same_level(l, l2) && self.same_level(m, m2) => Ok(()),
            f => Err(format!("premise {j} is {f:?}, not the level equivalence needed")),
        }
    }
    /// The context `g` with one more entry `a`, and the variable for it.
    fn pushed(&self, ext: Ix, g: Ix, a: Ix, fvar: Ix) -> R {
        match self.t.ctxs.get(ext as usize).ok_or("no such context")? {
            CtxRow::Push(q, ty) if self.same_ctx(*q, g) && self.same(*ty, a) => {}
            _ => return Err("the context is not the one with the binder's type added".into()),
        }
        need(*self.term(fvar)? == TermRow::FVar(self.clen[g as usize]), "the variable is not the next free one")
    }
    /// Premise `j` is a typing in a context extended by `a`; returns the
    /// extended context and the typing.
    fn infer_ext(&self, i: usize, j: usize, g: Ix, a: Ix, fvar: Ix) -> Result<(Ix, Ix, Ix), String> {
        match self.p(i, j)? {
            Fact::Infer { g: ext, e, t } => {
                self.pushed(ext, g, a, fvar)?;
                Ok((ext, e, t))
            }
            f => Err(format!("premise {j} is {f:?}, not a typing")),
        }
    }
    /// The head of an application and its arguments, in order.
    fn spine(&self, mut e: Ix) -> Result<(Ix, Vec<Ix>), String> {
        let mut args = vec![];
        while let TermRow::App(f, x) = *self.term(e)? {
            args.push(x);
            e = f;
        }
        args.reverse();
        Ok((e, args))
    }

    /// `e` as a head applied to exactly `n` arguments.
    fn peel(&self, mut e: Ix, n: usize) -> Result<(Ix, Vec<Ix>), String> {
        let mut args = vec![];
        for _ in 0..n {
            let TermRow::App(f, x) = *self.term(e)? else { return Err("too few arguments".into()) };
            args.push(x);
            e = f;
        }
        args.reverse();
        Ok((e, args))
    }

    fn sort_of(&self, e: Ix) -> Result<Ix, String> {
        match self.term(e)? {
            TermRow::Sort(l) => Ok(*l),
            _ => Err("not a sort".into()),
        }
    }
    fn n(&self, g: Ix) -> u32 {
        self.clen[g as usize]
    }

    fn row(&self, i: usize) -> R {
        use Rule::*;
        let row = &self.t.rows[i];
        self.check_refs(&row.fact)?;
        match (row.fact, row.rule) {
            (Fact::Has { g, n, a }, HasLast) => match self.t.ctxs[g as usize] {
                CtxRow::Push(q, ty) => need(n == self.n(q) && self.same(ty, a), "not the last entry"),
                CtxRow::Empty => Err("the context is empty".into()),
            },
            (Fact::Has { g, n, a }, HasUp) => match (self.t.ctxs[g as usize].clone(), self.p(i, 0)?) {
                (CtxRow::Push(q, _), Fact::Has { g: g2, n: n2, a: a2 }) => {
                    need(self.same_ctx(q, g2) && n == n2 && self.same(a, a2), "the premise is another entry")
                }
                _ => Err("not an entry of the context below".into()),
            },
            (Fact::Infer { g, e, t }, rule) => self.typing(i, g, e, t, rule),
            (Fact::Eq { g, a, b }, rule) => self.equality(i, g, a, b, rule),
            (Fact::Inst { e, k, a, r }, rule) => {
                need(self.meta[a as usize].lbr == 0, "the substituted term has a loose bound variable")?;
                self.substitution(i, e, k, a, r, rule)
            }
            (Fact::LInst { e, ls, r }, rule) => self.level_substitution(i, e, ls, r, rule),
            (Fact::LInstL { l, ls, r }, rule) => self.level_substitution_level(i, l, ls, r, rule),
            (Fact::LEq { l, m }, LEqNorm) => need(equivalent(&self.level(l)?, &self.level(m)?), "the levels differ"),
            (f, r) => Err(format!("{r:?} does not conclude {f:?}")),
        }
    }

    fn typing(&self, i: usize, g: Ix, e: Ix, t: Ix, rule: Rule) -> R {
        use Rule::*;
        match (self.term(e)?.clone(), rule) {
            (TermRow::Sort(l), Sort) => {
                self.params_in_range(l)?;
                match self.term(t)? {
                    TermRow::Sort(s) => match self.t.levels[*s as usize] {
                        LevelRow::Succ(l2) if self.same_level(l, l2) => Ok(()),
                        _ => Err("the type is not the next sort".into()),
                    },
                    _ => Err("the type is not a sort".into()),
                }
            }
            (TermRow::FVar(n), FVar) => match self.p(i, 0)? {
                Fact::Has { g: g2, n: n2, a } => {
                    need(self.same_ctx(g, g2) && n == n2 && self.same(a, t), "the entry is another one")
                }
                f => Err(format!("premise {f:?} is not a context entry")),
            },
            (TermRow::Const(d, ls), Const) => {
                let decl = &self.t.decls[d as usize];
                let levels = self.list(ls)?;
                need(levels.len() == decl.levels as usize, "wrong number of levels")?;
                for l in &levels {
                    self.params_in_range(*l)?;
                }
                if levels.is_empty() && self.t.rows[i].premises.is_empty() {
                    need(self.same(decl.ty, t), "not the declared type")
                } else {
                    let r = self.linst(i, 0, decl.ty, ls)?;
                    need(self.same(r, t), "not the declared type at these levels")
                }
            }
            (TermRow::App(f, a), App) => {
                let (f2, ft) = self.infer(i, 0, g)?;
                let (ft2, pi) = self.eq(i, 1, g)?;
                let (a2, at) = self.infer(i, 2, g)?;
                let (at2, dom) = self.eq(i, 3, g)?;
                let TermRow::Pi(d, cod) = *self.term(pi)? else { return Err("the function's type is not a pi".into()) };
                need(self.same(f, f2) && self.same(ft, ft2) && self.same(a, a2) && self.same(at, at2), "premises")?;
                need(self.same(dom, d), "the argument's type is not the domain")?;
                let r = self.inst(i, 4, cod, 0, a)?;
                need(self.same(r, t), "the type is not the codomain at the argument")
            }
            (TermRow::Lam(a, b), Lam) => {
                let TermRow::Pi(a2, cod) = *self.term(t)? else { return Err("the type is not a pi".into()) };
                need(self.same(a, a2), "the domain differs")?;
                let (a3, s) = self.infer(i, 0, g)?;
                let (s2, sort) = self.eq(i, 1, g)?;
                need(self.same(a, a3) && self.same(s, s2), "premises")?;
                self.sort_of(sort)?;
                let n = self.n(g);
                let fvar = self.find_fvar(i, n)?;
                let bo = self.inst(i, 2, b, 0, fvar)?;
                let (_, bo2, codo) = self.infer_ext(i, 3, g, a, fvar)?;
                need(self.same(bo, bo2), "premises")?;
                let codo2 = self.inst(i, 4, cod, 0, fvar)?;
                need(self.same(codo, codo2), "the codomain does not close the body's type")?;
                need(self.meta[b as usize].fv <= n && self.meta[cod as usize].fv <= n, "the variable occurs already")
            }
            (TermRow::Pi(a, b), Pi) => {
                let u = self.sort_of(t)?;
                let LevelRow::IMax(l1, l2) = self.t.levels[u as usize] else { return Err("not an imax".into()) };
                let (a3, s) = self.infer(i, 0, g)?;
                let (s2, sort) = self.eq(i, 1, g)?;
                need(self.same(a, a3) && self.same(s, s2) && self.same_level(self.sort_of(sort)?, l1), "premises")?;
                let n = self.n(g);
                let fvar = self.find_fvar(i, n)?;
                let bo = self.inst(i, 2, b, 0, fvar)?;
                let (ext, bo2, s3) = self.infer_ext(i, 3, g, a, fvar)?;
                let (s4, sort2) = self.eq(i, 4, ext)?;
                need(self.same(bo, bo2) && self.same(s3, s4), "premises")?;
                need(self.same_level(self.sort_of(sort2)?, l2), "the codomain's sort")?;
                need(self.meta[b as usize].fv <= n, "the variable occurs already")
            }
            (TermRow::Let(a, v, b), Let) => {
                let (a2, s) = self.infer(i, 0, g)?;
                let (s2, sort) = self.eq(i, 1, g)?;
                self.sort_of(sort)?;
                let (v2, vt) = self.infer(i, 2, g)?;
                let (vt2, a3) = self.eq(i, 3, g)?;
                need(self.same(a, a2) && self.same(s, s2) && self.same(v, v2), "premises")?;
                need(self.same(vt, vt2) && self.same(a, a3), "the value's type is not the declared one")?;
                let b2 = self.inst(i, 4, b, 0, v)?;
                let (b3, t2) = self.infer(i, 5, g)?;
                need(self.same(b2, b3) && self.same(t, t2), "premises")
            }
            (TermRow::Proj(s, idx, x), Proj) => self.proj_type(i, g, s, idx, x, t),
            (TermRow::Lit(_), LitNat) => self.prelude_type(t, self.t.nat),
            (TermRow::Str(_), LitStr) => self.prelude_type(t, self.t.string),
            (e, r) => Err(format!("{r:?} does not type {e:?}")),
        }
    }

    /// The declaration row cited with this key.
    fn decl_by_key(&self, key: &Key) -> Result<&DeclRow, String> {
        self.t.decls.iter().find(|d| &d.key == key).ok_or_else(|| "a constructor is not cited".into())
    }

    /// `Infer(g, proj S idx x, t)`. Premises, in order: `Infer(g, x, E)`,
    /// `Eq(g, E, I ps)`, `Infer(g, E, SE)`, `Eq(g, SE, sort l)`, the
    /// constructor's type at the levels `LInst(C, ls, r)`; then for each
    /// parameter `Eq(g, r, pi)` and `Inst(body, 0, p, r')`; for each field
    /// before idx `Eq(g, r, pi)`, the proposition check of its type when l
    /// is zero and the rest depends on it, and `Inst(body, 0, proj S j x, r')`;
    /// last `Eq(g, r, pi)` whose domain is t, and its proposition check.
    /// A proposition check is `Infer(g, A, S)` and `Eq(g, S, sort zero)`.
    fn proj_type(&self, i: usize, g: Ix, s: Ix, idx: u32, x: Ix, t: Ix) -> R {
        let (x2, e) = self.infer(i, 0, g)?;
        let (e2, iapp) = self.eq(i, 1, g)?;
        let (e3, se) = self.infer(i, 2, g)?;
        let (se2, sort) = self.eq(i, 3, g)?;
        need(self.same(x, x2) && self.same(e, e2) && self.same(e, e3) && self.same(se, se2), "premises")?;
        let is_prop = equivalent(&self.level(self.sort_of(sort)?)?, &Level::Zero);
        let (head, ps) = self.spine(iapp)?;
        let TermRow::Const(d, ls) = *self.term(head)? else { return Err("the type is not an inductive type".into()) };
        let induct = &self.t.decls[d as usize];
        need(induct.key == self.t.decls[s as usize].key, "the type is not the structure projected")?;
        let Info::Induct { params, indices, ctors } = &induct.info else { return Err("not an inductive type".into()) };
        need(induct.kind == DeclKind::Inductive, "not an inductive type")?;
        need(ctors.len() == 1 && ps.len() == (params + indices) as usize, "not a structure applied to its arguments")?;
        let ctor = self.decl_by_key(&ctors[0])?;
        need(ctor.kind == DeclKind::Constructor && self.list(ls)?.len() == ctor.levels as usize, "the constructor")?;
        let mut r = self.linst(i, 4, ctor.ty, ls)?;
        let mut j = 5;
        let pi = |j: &mut usize, r: Ix| -> Result<(Ix, Ix), String> {
            let (r2, w) = self.eq(i, *j, g)?;
            need(self.same(r, r2), "premises")?;
            *j += 1;
            match *self.term(w)? {
                TermRow::Pi(a, b) => Ok((a, b)),
                _ => Err("the constructor's type has too few binders".into()),
            }
        };
        let prop = |j: &mut usize, a: Ix| -> R {
            let (a2, s) = self.infer(i, *j, g)?;
            let (s2, z) = self.eq(i, *j + 1, g)?;
            *j += 2;
            need(self.same(a, a2) && self.same(s, s2), "premises")?;
            need(self.t.levels[self.sort_of(z)? as usize] == LevelRow::Zero, "a field of a proposition is data")
        };
        for p in &ps[..*params as usize] {
            let (_, body) = pi(&mut j, r)?;
            r = self.inst(i, j, body, 0, *p)?;
            j += 1;
        }
        for k in 0..idx {
            let (a, body) = pi(&mut j, r)?;
            if is_prop && self.meta[body as usize].lbr > 0 {
                prop(&mut j, a)?;
            }
            r = match self.p(i, j)? {
                Fact::Inst { e, k: 0, a: pj, r }
                    if self.same(e, body)
                        && matches!(*self.term(pj)?, TermRow::Proj(s2, k2, x2)
                            if k2 == k && self.same(x2, x) && self.t.decls[s2 as usize].key == induct.key) =>
                {
                    r
                }
                f => return Err(format!("premise {j} is {f:?}, not the field replaced by its projection")),
            };
            j += 1;
        }
        let (a, _) = pi(&mut j, r)?;
        need(self.same(a, t), "not the field's type")?;
        if is_prop {
            prop(&mut j, a)?;
        }
        need(j == self.t.rows[i].premises.len(), "extra premises")
    }

    fn prelude_type(&self, t: Ix, decl: Option<Ix>) -> R {
        let d = decl.ok_or("no declaration given for literals")?;
        match self.term(t)? {
            TermRow::Const(d2, ls) => need(
                self.t.decls[*d2 as usize].key == self.t.decls[d as usize].key && self.list(*ls)?.is_empty(),
                "not the literal's type",
            ),
            _ => Err("not the literal's type".into()),
        }
    }

    /// The `fvar n` named by the row: a term row, found among the terms the
    /// row's facts mention (the substitution premise at position 2).
    fn find_fvar(&self, i: usize, n: u32) -> Result<Ix, String> {
        match self.p(i, 2)? {
            Fact::Inst { a, .. } => {
                need(*self.term(a)? == TermRow::FVar(n), "the binder is not opened with the next variable")?;
                Ok(a)
            }
            f => Err(format!("premise {f:?} does not open the binder")),
        }
    }

    fn equality(&self, i: usize, g: Ix, a: Ix, b: Ix, rule: Rule) -> R {
        use Rule::*;
        let (ta, tb) = (self.term(a)?.clone(), self.term(b)?.clone());
        match rule {
            Refl => need(self.same(a, b), "not the same term"),
            Symm => {
                let (b2, a2) = self.eq(i, 0, g)?;
                need(self.same(a, a2) && self.same(b, b2), "premises")
            }
            Trans => {
                let (a2, m) = self.eq(i, 0, g)?;
                let (m2, b2) = self.eq(i, 1, g)?;
                need(self.same(a, a2) && self.same(m, m2) && self.same(b, b2), "premises")
            }
            Beta => {
                let TermRow::App(f, x) = ta else { return Err("not an application".into()) };
                let TermRow::Lam(_, body) = *self.term(f)? else { return Err("not a lambda applied".into()) };
                let r = self.inst(i, 0, body, 0, x)?;
                need(self.same(r, b), "not the body at the argument")
            }
            Delta => {
                let TermRow::Const(d, ls) = ta else { return Err("not a constant".into()) };
                let decl = &self.t.decls[d as usize];
                need(matches!(decl.kind, DeclKind::Definition | DeclKind::Theorem), "not unfoldable")?;
                need(self.list(ls)?.len() == decl.levels as usize, "wrong number of levels")?;
                let v = decl.value.ok_or("no value given")?;
                if self.t.rows[i].premises.is_empty() && decl.levels == 0 {
                    need(self.same(v, b), "not the value")
                } else {
                    let r = self.linst(i, 0, v, ls)?;
                    need(self.same(r, b), "not the value at these levels")
                }
            }
            Iota => {
                let (head, args) = self.spine(a)?;
                let TermRow::Const(r, ls) = *self.term(head)? else { return Err("not a recursor applied".into()) };
                let decl = &self.t.decls[r as usize];
                let Info::Rec { params, motives, minors, indices, rules } = &decl.info else {
                    return Err("not a recursor".into());
                };
                need(decl.kind == DeclKind::Recursor, "not a recursor")?;
                need(self.list(ls)?.len() == decl.levels as usize, "wrong number of levels")?;
                let before = (params + motives + minors) as usize;
                let at = before + *indices as usize;
                let major = *args.get(at).ok_or("too few arguments for the major premise")?;
                let (mh, margs) = self.spine(major)?;
                let TermRow::Const(c, _) = *self.term(mh)? else { return Err("the major premise is not built".into()) };
                let cdecl = &self.t.decls[c as usize];
                let Info::Ctor { params: cp, fields, .. } = cdecl.info else {
                    return Err("the major premise is not a constructor applied".into());
                };
                need(cp == *params && margs.len() == (cp + fields) as usize, "the constructor's arguments")?;
                let rule = rules.iter().find(|x| x.ctor == cdecl.key).ok_or("no rule for the constructor")?;
                need(rule.fields == fields, "the rule's fields")?;
                let rhs = if decl.levels == 0 && self.t.rows[i].premises.is_empty() {
                    rule.rhs
                } else {
                    self.linst(i, 0, rule.rhs, ls)?
                };
                let mut want: Vec<Ix> = args[..before].to_vec();
                want.extend_from_slice(&margs[cp as usize..]);
                want.extend_from_slice(&args[at + 1..]);
                let (bh, bargs) = self.peel(b, want.len())?;
                need(self.same(bh, rhs), "not the rule's right-hand side")?;
                need(bargs.iter().zip(&want).all(|(x, y)| self.same(*x, *y)), "not the arguments the rule takes")
            }
            ProjCtor => {
                let TermRow::Proj(s, idx, x) = ta else { return Err("not a projection".into()) };
                let (h, args) = self.spine(x)?;
                let TermRow::Const(c, _) = *self.term(h)? else { return Err("not a constructor applied".into()) };
                let Info::Ctor { induct, params, fields } = self.t.decls[c as usize].info else {
                    return Err("not a constructor applied".into());
                };
                need(induct == self.t.decls[s as usize].key, "a constructor of another type")?;
                need(args.len() == (params + fields) as usize && idx < fields, "the constructor's arguments")?;
                need(self.same(args[(params + idx) as usize], b), "not the field")
            }
            CongProj => {
                let (TermRow::Proj(s, n, x), TermRow::Proj(s2, n2, y)) = (ta, tb) else {
                    return Err("not projections".into());
                };
                need(self.t.decls[s as usize].key == self.t.decls[s2 as usize].key && n == n2, "different fields")?;
                let (p, q) = self.eq(i, 0, g)?;
                need(self.same(x, p) && self.same(y, q), "premises")
            }
            Zeta => {
                let TermRow::Let(_, v, body) = ta else { return Err("not a let".into()) };
                let r = self.inst(i, 0, body, 0, v)?;
                need(self.same(r, b), "not the body at the value")
            }
            CongApp => {
                let (TermRow::App(f, x), TermRow::App(f2, x2)) = (ta, tb) else {
                    return Err("not applications".into());
                };
                let (p, q) = self.eq(i, 0, g)?;
                let (r, s) = self.eq(i, 1, g)?;
                need(self.same(f, p) && self.same(f2, q) && self.same(x, r) && self.same(x2, s), "premises")
            }
            CongLam | CongPi => {
                let ((a1, b1), (a2, b2)) = match (ta, tb, rule) {
                    (TermRow::Lam(a1, b1), TermRow::Lam(a2, b2), CongLam) => ((a1, b1), (a2, b2)),
                    (TermRow::Pi(a1, b1), TermRow::Pi(a2, b2), CongPi) => ((a1, b1), (a2, b2)),
                    _ => return Err("not two binders of the kind".into()),
                };
                let (p, q) = self.eq(i, 0, g)?;
                need(self.same(a1, p) && self.same(a2, q), "the domains")?;
                let n = self.n(g);
                let fvar = self.find_fvar(i, n)?;
                let o1 = self.inst(i, 2, b1, 0, fvar)?;
                // premise 1 opens the other body with the same variable.
                let o2 = match self.p(i, 1)? {
                    Fact::Inst { e, k: 0, a: v, r } if self.same(e, b2) && self.same(v, fvar) => r,
                    f => return Err(format!("premise {f:?} does not open the other body")),
                };
                match self.p(i, 3)? {
                    Fact::Eq { g: ext, a: x, b: y } => {
                        self.pushed(ext, g, a1, fvar)?;
                        need(self.same(x, o1) && self.same(y, o2), "the bodies")?;
                    }
                    f => return Err(format!("premise {f:?} is not the bodies' equality")),
                }
                need(self.meta[b1 as usize].fv <= n && self.meta[b2 as usize].fv <= n, "the variable occurs already")
            }
            CongSort => {
                let (TermRow::Sort(l), TermRow::Sort(m)) = (ta, tb) else { return Err("not sorts".into()) };
                self.leq(i, 0, l, m)
            }
            CongConst => {
                let (TermRow::Const(d, ls), TermRow::Const(d2, ms)) = (ta, tb) else {
                    return Err("not constants".into());
                };
                need(self.t.decls[d as usize].key == self.t.decls[d2 as usize].key, "different constants")?;
                let (ls, ms) = (self.list(ls)?, self.list(ms)?);
                need(ls.len() == ms.len(), "different numbers of levels")?;
                for (j, (l, m)) in ls.iter().zip(&ms).enumerate() {
                    self.leq(i, j, *l, *m)?;
                }
                Ok(())
            }
            Eta => {
                let TermRow::Lam(dom, body) = ta else { return Err("not a lambda".into()) };
                let TermRow::App(f, x) = *self.term(body)? else { return Err("the body is not an application".into()) };
                need(*self.term(x)? == TermRow::BVar(0), "the argument is not the bound variable")?;
                need(self.meta[f as usize].lbr == 0 && self.same(f, b), "not the function itself")?;
                let (f2, ft) = self.infer(i, 0, g)?;
                let (ft2, pi) = self.eq(i, 1, g)?;
                let TermRow::Pi(d, _) = *self.term(pi)? else { return Err("the function's type is not a pi".into()) };
                let (x1, x2) = self.eq(i, 2, g)?;
                need(self.same(f, f2) && self.same(ft, ft2) && self.same(x1, dom) && self.same(x2, d), "premises")
            }
            ProofIrrel => {
                let (a2, pa) = self.infer(i, 0, g)?;
                let (b2, pb) = self.infer(i, 1, g)?;
                let (pa2, pb2) = self.eq(i, 2, g)?;
                let (pa3, s) = self.infer(i, 3, g)?;
                let (s2, z) = self.eq(i, 4, g)?;
                need(self.same(a, a2) && self.same(b, b2) && self.same(pa, pa2) && self.same(pb, pb2), "premises")?;
                need(self.same(pa, pa3) && self.same(s, s2), "premises")?;
                need(self.t.levels[self.sort_of(z)? as usize] == LevelRow::Zero, "the type is not a proposition")
            }
            r => Err(format!("{r:?} does not conclude an equality")),
        }
    }

    fn substitution(&self, i: usize, e: Ix, k: u32, a: Ix, r: Ix, rule: Rule) -> R {
        use Rule::*;
        let (te, tr) = (self.term(e)?.clone(), self.term(r)?.clone());
        match (rule, te, tr) {
            (InstClosed, _, _) => need(self.meta[e as usize].lbr <= k && self.same(e, r), "not unchanged"),
            (InstHit, TermRow::BVar(j), _) => need(j == k && self.same(a, r), "not the variable replaced"),
            (InstLower, TermRow::BVar(j), TermRow::BVar(j2)) => need(j > k && j2 + 1 == j, "not lowered by one"),
            (InstApp, TermRow::App(f, x), TermRow::App(f2, x2)) => {
                let (r1, r2) = (self.inst(i, 0, f, k, a)?, self.inst(i, 1, x, k, a)?);
                need(self.same(r1, f2) && self.same(r2, x2), "the parts")
            }
            (InstBind, TermRow::Lam(d, b), TermRow::Lam(d2, b2))
            | (InstBind, TermRow::Pi(d, b), TermRow::Pi(d2, b2)) => {
                let (r1, r2) = (self.inst(i, 0, d, k, a)?, self.inst(i, 1, b, k + 1, a)?);
                need(self.same(r1, d2) && self.same(r2, b2), "the parts")
            }
            (InstProj, TermRow::Proj(s, n, x), TermRow::Proj(s2, n2, x2)) => {
                need(self.t.decls[s as usize].key == self.t.decls[s2 as usize].key && n == n2, "different fields")?;
                need(self.same(self.inst(i, 0, x, k, a)?, x2), "the part")
            }
            (InstLet, TermRow::Let(t, v, b), TermRow::Let(t2, v2, b2)) => {
                let (r1, r2, r3) =
                    (self.inst(i, 0, t, k, a)?, self.inst(i, 1, v, k, a)?, self.inst(i, 2, b, k + 1, a)?);
                need(self.same(r1, t2) && self.same(r2, v2) && self.same(r3, b2), "the parts")
            }
            (r, e, _) => Err(format!("{r:?} does not substitute in {e:?}")),
        }
    }

    fn level_substitution(&self, i: usize, e: Ix, ls: Ix, r: Ix, rule: Rule) -> R {
        use Rule::*;
        let (te, tr) = (self.term(e)?.clone(), self.term(r)?.clone());
        match (rule, te, tr) {
            (LInstNone, _, _) => need(!self.meta[e as usize].lp && self.same(e, r), "not unchanged"),
            (LInstSort, TermRow::Sort(l), TermRow::Sort(l2)) => {
                need(self.same_level(self.linstl(i, 0, l, ls)?, l2), "the level")
            }
            (LInstConst, TermRow::Const(d, ms), TermRow::Const(d2, ms2)) => {
                need(self.t.decls[d as usize].key == self.t.decls[d2 as usize].key, "different constants")?;
                let (ms, ms2) = (self.list(ms)?, self.list(ms2)?);
                need(ms.len() == ms2.len(), "different numbers of levels")?;
                for (j, (m, m2)) in ms.iter().zip(&ms2).enumerate() {
                    need(self.same_level(self.linstl(i, j, *m, ls)?, *m2), "a level")?;
                }
                Ok(())
            }
            (LInstApp, TermRow::App(f, x), TermRow::App(f2, x2))
            | (LInstBind, TermRow::Lam(f, x), TermRow::Lam(f2, x2))
            | (LInstBind, TermRow::Pi(f, x), TermRow::Pi(f2, x2)) => {
                let (r1, r2) = (self.linst(i, 0, f, ls)?, self.linst(i, 1, x, ls)?);
                need(self.same(r1, f2) && self.same(r2, x2), "the parts")
            }
            (LInstProj, TermRow::Proj(s, n, x), TermRow::Proj(s2, n2, x2)) => {
                need(self.t.decls[s as usize].key == self.t.decls[s2 as usize].key && n == n2, "different fields")?;
                need(self.same(self.linst(i, 0, x, ls)?, x2), "the part")
            }
            (LInstLet, TermRow::Let(t, v, b), TermRow::Let(t2, v2, b2)) => {
                let (r1, r2, r3) = (self.linst(i, 0, t, ls)?, self.linst(i, 1, v, ls)?, self.linst(i, 2, b, ls)?);
                need(self.same(r1, t2) && self.same(r2, v2) && self.same(r3, b2), "the parts")
            }
            (r, e, _) => Err(format!("{r:?} does not substitute levels in {e:?}")),
        }
    }

    fn level_substitution_level(&self, i: usize, l: Ix, ls: Ix, r: Ix, rule: Rule) -> R {
        use Rule::*;
        let (tl, tr) = (self.t.levels[l as usize].clone(), self.t.levels[r as usize].clone());
        match (rule, tl, tr) {
            (LInstLZero, LevelRow::Zero, LevelRow::Zero) => Ok(()),
            (LInstLSucc, LevelRow::Succ(a), LevelRow::Succ(a2)) => {
                need(self.same_level(self.linstl(i, 0, a, ls)?, a2), "the level")
            }
            (LInstLMax, LevelRow::Max(a, b), LevelRow::Max(a2, b2))
            | (LInstLIMax, LevelRow::IMax(a, b), LevelRow::IMax(a2, b2)) => {
                let (r1, r2) = (self.linstl(i, 0, a, ls)?, self.linstl(i, 1, b, ls)?);
                need(self.same_level(r1, a2) && self.same_level(r2, b2), "the parts")
            }
            (LInstLParam, LevelRow::Param(n), _) => {
                let list = self.list(ls)?;
                let x = *list.get(n as usize).ok_or("the parameter has no level given")?;
                need(self.same_level(x, r), "not the level given for the parameter")
            }
            (r, l, _) => Err(format!("{r:?} does not substitute in {l:?}")),
        }
    }

    fn goal(&self) -> R {
        let g = self.goal;
        let fact = |k: Ix| -> Result<Fact, String> { Ok(self.t.rows.get(k as usize).ok_or("no such row")?.fact) };
        let empty = |c: Ix| self.t.ctxs.get(c as usize) == Some(&CtxRow::Empty);
        let Fact::Infer { g: c, e, t: s } = fact(g.ty_is_type.0)? else { return Err("not a typing".into()) };
        need(empty(c) && self.same(e, g.ty), "the type is not typed in the empty context")?;
        let Fact::Eq { g: c, a: s2, b: sort } = fact(g.ty_is_type.1)? else { return Err("not an equality".into()) };
        need(empty(c) && self.same(s, s2), "premises")?;
        let u = self.sort_of(sort)?;
        need(self.meta[g.ty as usize].fv == 0, "the type has free variables")?;
        match (g.kind, g.value, g.value_has_ty) {
            (DeclKind::Axiom, None, None) => {}
            (DeclKind::Definition | DeclKind::Theorem | DeclKind::Opaque, Some(v), Some((p, q))) => {
                let Fact::Infer { g: c, e, t } = fact(p)? else { return Err("not a typing".into()) };
                need(empty(c) && self.same(e, v), "the value is not typed in the empty context")?;
                let Fact::Eq { g: c, a: t2, b: ty } = fact(q)? else { return Err("not an equality".into()) };
                need(empty(c) && self.same(t, t2) && self.same(ty, g.ty), "the value's type is not the declared type")?;
            }
            _ => return Err("the value does not fit the kind".into()),
        }
        if g.kind == DeclKind::Theorem {
            let Fact::LEq { l, m } = fact(g.is_prop.ok_or("a theorem must be a proposition")?)? else {
                return Err("not a level equivalence".into());
            };
            need(self.same_level(l, u) && self.t.levels[m as usize] == LevelRow::Zero, "not a proposition")?;
        }
        Ok(())
    }
}
