//! Terms as Lean's kernel holds them, for the purely syntactic steps of a
//! nested inductive block (kernel/inductive.cpp, `elim_nested_inductive`
//! and `restore_nested`). Both sides use them: the prover turns the results
//! into rows, the checker computes their ids and compares them with the
//! rows it was given.

use crate::level::Level;
use crate::tables::*;
use std::collections::HashMap;
use std::rc::Rc;

pub type X = Rc<E>;

#[derive(Clone, Debug, PartialEq)]
pub enum E {
    BVar(u32),
    /// A local constant of the procedure (never left in a result).
    FVar(u32),
    Sort(Level),
    Const(Key, Vec<Level>),
    App(X, X),
    Lam(X, X),
    Pi(X, X),
    Let(X, X, X),
    /// A literal: its row, which is closed.
    Lit(Ix),
    Proj(Key, u32, X),
}

use E::*;

pub fn level_of(t: &Tables, l: Ix) -> Level {
    match t.levels[l as usize] {
        LevelRow::Zero => Level::Zero,
        LevelRow::Succ(a) => level_of(t, a).succ(),
        LevelRow::Max(a, b) => Level::max(level_of(t, a), level_of(t, b)),
        LevelRow::IMax(a, b) => Level::imax(level_of(t, a), level_of(t, b)),
        LevelRow::Param(n) => Level::Param(n),
    }
}

fn levels_of(t: &Tables, mut ls: Ix) -> Vec<Level> {
    let mut out = vec![];
    while let ListRow::Cons(h, r) = t.lists[ls as usize] {
        out.push(level_of(t, h));
        ls = r;
    }
    out
}

/// The term of row e.
pub fn from_row(t: &Tables, e: Ix, memo: &mut HashMap<Ix, X>) -> X {
    if let Some(x) = memo.get(&e) {
        return x.clone();
    }
    let mut r = |x: Ix| from_row(t, x, memo);
    let x = Rc::new(match t.terms[e as usize].clone() {
        TermRow::BVar(n) => BVar(n),
        TermRow::FVar(n) => FVar(n),
        TermRow::Sort(l) => Sort(level_of(t, l)),
        TermRow::Const(d, ls) => Const(t.decls[d as usize].key, levels_of(t, ls)),
        TermRow::App(a, b) => App(r(a), r(b)),
        TermRow::Lam(a, b) => Lam(r(a), r(b)),
        TermRow::Pi(a, b) => Pi(r(a), r(b)),
        TermRow::Let(a, v, b) => Let(r(a), r(v), r(b)),
        TermRow::Lit(_) | TermRow::Str(_) => Lit(e),
        TermRow::Proj(d, n, x) => Proj(t.decls[d as usize].key, n, r(x)),
    });
    memo.insert(e, x.clone());
    x
}

/// Lean's `replace`: f sees each subterm, outermost first, with the number
/// of binders above it; where it answers, the subterm is replaced and not
/// entered.
pub fn replace(e: &X, d: u32, f: &mut dyn FnMut(&X, u32) -> Option<X>) -> X {
    if let Some(r) = f(e, d) {
        return r;
    }
    match &**e {
        App(a, b) => Rc::new(App(replace(a, d, f), replace(b, d, f))),
        Lam(a, b) => Rc::new(Lam(replace(a, d, f), replace(b, d + 1, f))),
        Pi(a, b) => Rc::new(Pi(replace(a, d, f), replace(b, d + 1, f))),
        Let(a, v, b) => Rc::new(Let(replace(a, d, f), replace(v, d, f), replace(b, d + 1, f))),
        Proj(k, n, x) => Rc::new(Proj(*k, *n, replace(x, d, f))),
        _ => e.clone(),
    }
}

/// Lean's `find`: whether some subterm satisfies p.
pub fn find(e: &X, p: &dyn Fn(&E) -> bool) -> bool {
    p(e) || match &**e {
        App(a, b) | Lam(a, b) | Pi(a, b) => find(a, p) || find(b, p),
        Let(a, v, b) => find(a, p) || find(v, p) || find(b, p),
        Proj(_, _, x) => find(x, p),
        _ => false,
    }
}

pub fn has_loose_bvars(e: &X) -> bool {
    fn go(e: &X, d: u32) -> bool {
        match &**e {
            BVar(n) => *n >= d,
            App(a, b) => go(a, d) || go(b, d),
            Lam(a, b) | Pi(a, b) => go(a, d) || go(b, d + 1),
            Let(a, v, b) => go(a, d) || go(v, d) || go(b, d + 1),
            Proj(_, _, x) => go(x, d),
            _ => false,
        }
    }
    go(e, 0)
}

/// Lean's `instantiate_rev`: the loose bound variables 0..n of e become
/// `vals` (the last for 0); the values have no loose bound variables.
pub fn instantiate_rev(e: &X, vals: &[X]) -> X {
    let n = vals.len() as u32;
    replace(e, 0, &mut |x, d| match &**x {
        BVar(i) if *i >= d => {
            Some(if i - d < n { vals[(n - 1 - (i - d)) as usize].clone() } else { Rc::new(BVar(i - n)) })
        }
        _ => None,
    })
}

/// Lean's `abstract`: the local constants `xs` become loose bound
/// variables, the last one 0.
pub fn abstract_(e: &X, xs: &[u32]) -> X {
    let n = xs.len() as u32;
    replace(e, 0, &mut |x, d| match &**x {
        FVar(v) => xs.iter().rposition(|y| y == v).map(|p| Rc::new(BVar(d + n - 1 - p as u32))),
        _ => None,
    })
}

/// `lctx.mk_pi(xs, body)` or `mk_lambda`: each local constant of xs, with
/// its type, bound around body, the first outermost.
pub fn bind(pi: bool, xs: &[(u32, X)], body: &X) -> X {
    xs.iter().rev().fold(body.clone(), |b, (x, ty)| {
        let b = abstract_(&b, &[*x]);
        Rc::new(if pi { Pi(ty.clone(), b) } else { Lam(ty.clone(), b) })
    })
}

pub fn spine(e: &X) -> (X, Vec<X>) {
    let mut args = vec![];
    let mut f = e.clone();
    while let App(g, a) = &*f.clone() {
        args.push(a.clone());
        f = g.clone();
    }
    args.reverse();
    (f, args)
}

pub fn mk_app(f: X, args: &[X]) -> X {
    args.iter().fold(f, |f, a| Rc::new(App(f, a.clone())))
}

/// The level with its parameter n replaced by `ls[n]`.
pub fn inst_level(l: &Level, ls: &[Level]) -> Level {
    match l {
        Level::Zero => Level::Zero,
        Level::Succ(a) => inst_level(a, ls).succ(),
        Level::Max(a, b) => Level::max(inst_level(a, ls), inst_level(b, ls)),
        Level::IMax(a, b) => Level::imax(inst_level(a, ls), inst_level(b, ls)),
        Level::Param(n) => ls.get(*n as usize).cloned().unwrap_or(Level::Param(*n)),
    }
}

/// Lean's `instantiate_lparams`.
pub fn inst_levels(e: &X, ls: &[Level]) -> X {
    replace(e, 0, &mut |x, _| match &**x {
        Sort(l) => Some(Rc::new(Sort(inst_level(l, ls)))),
        Const(k, cl) => Some(Rc::new(Const(*k, cl.iter().map(|l| inst_level(l, ls)).collect()))),
        _ => None,
    })
}

/// e with level parameter n renamed n + shift.
pub fn shift_levels(e: &X, shift: u32) -> X {
    fn sh(l: &Level, s: u32) -> Level {
        match l {
            Level::Zero => Level::Zero,
            Level::Succ(a) => sh(a, s).succ(),
            Level::Max(a, b) => Level::max(sh(a, s), sh(b, s)),
            Level::IMax(a, b) => Level::imax(sh(a, s), sh(b, s)),
            Level::Param(n) => Level::Param(n + s),
        }
    }
    if shift == 0 {
        return e.clone();
    }
    replace(e, 0, &mut |x, _| match &**x {
        Sort(l) => Some(Rc::new(Sort(sh(l, shift)))),
        Const(k, cl) => Some(Rc::new(Const(*k, cl.iter().map(|l| sh(l, shift)).collect()))),
        _ => None,
    })
}
