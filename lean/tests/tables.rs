//! Declarations of the core calculus: the small kernel writes their tables,
//! the checker accepts them, and changed tables are refused.

use cas_lean::tables::{CtxRow, Fact, LevelRow, Row, Rule, TermRow};
use cas_lean::{Builder, Checker, DeclKind, DeclRow, Goal, Tables};

fn key(n: u8) -> [u8; 32] {
    [n; 32]
}

fn axiom(b: &mut Builder, n: u8, levels: u32, ty: u32) -> u32 {
    b.decl(DeclRow { key: key(n), kind: DeclKind::Axiom, levels, ty, value: None, info: Default::default() })
}

fn accepted(b: &mut Builder, kind: DeclKind, levels: u32, ty: u32, value: Option<u32>) -> (Tables, Goal) {
    let goal = b.check_decl(kind, levels, ty, value).expect("the kernel accepts it");
    let t = b.t.clone();
    Checker::check(&t, &goal).expect("the checker accepts the tables");
    (t, goal)
}

/// `id.{u} : (a : Sort u) -> a -> a := fun a x => x`
fn identity(b: &mut Builder) -> (u32, u32) {
    let u = b.param(0);
    let s = b.sort(u);
    let (v0, v1) = (b.bvar(0), b.bvar(1));
    let inner = b.pi(v0, v1);
    let ty = b.pi(s, inner);
    let body = b.lam(v0, v0);
    let value = b.lam(s, body);
    (ty, value)
}

#[test]
fn the_polymorphic_identity_is_accepted() {
    let mut b = Builder::new();
    let (ty, value) = identity(&mut b);
    let (t, _) = accepted(&mut b, DeclKind::Definition, 1, ty, Some(value));
    assert!(t.rows.len() < 40, "{} rows", t.rows.len());
}

#[test]
fn a_definition_is_unfolded_and_a_redex_reduced() {
    let mut b = Builder::new();
    // axiom A : Type, axiom a : A
    let z = b.zero();
    let one = b.succ(z);
    let type0 = b.sort(one);
    let da = axiom(&mut b, 1, 0, type0);
    let a_ty = b.constant(da, &[]);
    let dx = axiom(&mut b, 2, 0, a_ty);
    let x = b.constant(dx, &[]);
    // def T : Type := A, and a value of type T: unfolding T is needed.
    let dt = b.decl(DeclRow {
        key: key(3),
        kind: DeclKind::Definition,
        levels: 0,
        ty: type0,
        value: Some(a_ty),
        info: Default::default(),
    });
    let t_ty = b.constant(dt, &[]);
    accepted(&mut b, DeclKind::Definition, 0, t_ty, Some(x));
    // (fun (t : Type) => t) A, as a type of x: a beta step is needed.
    let v0 = b.bvar(0);
    let f = b.lam(type0, v0);
    let redex = b.app(f, a_ty);
    accepted(&mut b, DeclKind::Definition, 0, redex, Some(x));
    // let y : A := x; y
    let body = b.bvar(0);
    let let_term = b.term(TermRow::Let(a_ty, x, body));
    accepted(&mut b, DeclKind::Definition, 0, a_ty, Some(let_term));
}

#[test]
fn levels_are_compared_by_normal_form() {
    // def s.{u,v} : Sort (succ (max u v)) := Sort (max v u)
    let mut b = Builder::new();
    let (u, v) = (b.param(0), b.param(1));
    let uv = b.level(LevelRow::Max(u, v));
    let vu = b.level(LevelRow::Max(v, u));
    let s_uv = b.succ(uv);
    let ty = b.sort(s_uv);
    let value = b.sort(vu);
    accepted(&mut b, DeclKind::Definition, 2, ty, Some(value));
}

#[test]
fn proofs_of_a_proposition_are_equal() {
    // axioms P : Prop, p q : P, F : P -> Prop, h : F p;  theorem : F q := h
    let mut b = Builder::new();
    let z = b.zero();
    let prop = b.sort(z);
    let dp = axiom(&mut b, 1, 0, prop);
    let p_ty = b.constant(dp, &[]);
    let dpp = axiom(&mut b, 2, 0, p_ty);
    let dq = axiom(&mut b, 3, 0, p_ty);
    let f_ty = b.pi(p_ty, prop);
    let df = axiom(&mut b, 4, 0, f_ty);
    let (pp, q, f) = (b.constant(dpp, &[]), b.constant(dq, &[]), b.constant(df, &[]));
    let fp_ = b.app(f, pp);
    let dh = axiom(&mut b, 5, 0, fp_);
    let h = b.constant(dh, &[]);
    let fq = b.app(f, q);
    let (t, _) = accepted(&mut b, DeclKind::Theorem, 0, fq, Some(h));
    assert!(t.rows.iter().any(|r| r.rule == Rule::ProofIrrel));
}

#[test]
fn a_function_equals_its_eta_expansion() {
    // axioms A : Type, g : A -> A, R : (A -> A) -> Prop, r : R g;
    // theorem : R (fun x => g x) := r
    let mut b = Builder::new();
    let z = b.zero();
    let one = b.succ(z);
    let (prop, type0) = (b.sort(z), b.sort(one));
    let da = axiom(&mut b, 1, 0, type0);
    let a = b.constant(da, &[]);
    let aa = b.pi(a, a);
    let dg = axiom(&mut b, 2, 0, aa);
    let g = b.constant(dg, &[]);
    let r_ty = b.pi(aa, prop);
    let dr = axiom(&mut b, 3, 0, r_ty);
    let rr = b.constant(dr, &[]);
    let rg = b.app(rr, g);
    let dh = axiom(&mut b, 4, 0, rg);
    let h = b.constant(dh, &[]);
    let x = b.bvar(0);
    let gx = b.app(g, x);
    let expanded = b.lam(a, gx);
    let r_exp = b.app(rr, expanded);
    let (t, _) = accepted(&mut b, DeclKind::Theorem, 0, r_exp, Some(h));
    assert!(t.rows.iter().any(|r| r.rule == Rule::Eta));
}

#[test]
fn ill_typed_declarations_are_refused_by_the_kernel() {
    let mut b = Builder::new();
    let z = b.zero();
    let one = b.succ(z);
    let type0 = b.sort(one);
    let da = axiom(&mut b, 1, 0, type0);
    let db = axiom(&mut b, 2, 0, type0);
    let (a, bb) = (b.constant(da, &[]), b.constant(db, &[]));
    let dx = axiom(&mut b, 3, 0, a);
    let x = b.constant(dx, &[]);
    assert!(b.check_decl(DeclKind::Definition, 0, bb, Some(x)).is_err());
    // A theorem whose type is not a proposition.
    assert!(b.check_decl(DeclKind::Theorem, 0, a, Some(x)).is_err());
}

/// Every single change to an accepted derivation is either refused or
/// still a derivation of the same goal (a change to an unused row).
#[test]
fn changed_tables_are_refused() {
    let mut b = Builder::new();
    let (ty, value) = identity(&mut b);
    let (t, goal) = accepted(&mut b, DeclKind::Definition, 1, ty, Some(value));

    // Claim the identity has a different type: the value's typing row says
    // it has type `Sort u`.
    let mut bad = t.clone();
    let su = bad.terms.iter().position(|e| matches!(e, TermRow::Sort(_))).unwrap() as u32;
    let (p, _) = goal.value_has_ty.unwrap();
    if let Fact::Infer { t: ref mut ty, .. } = bad.rows[p as usize].fact {
        *ty = su;
    }
    assert!(Checker::check(&bad, &goal).is_err());

    // Turn an equality into a bogus reflexivity between different terms.
    let mut bad = t.clone();
    let e = bad.terms.len() as u32 - 1;
    bad.rows.push(Row { fact: Fact::Eq { g: 0, a: 0, b: e }, rule: Rule::Refl, premises: vec![] });
    assert!(Checker::check(&bad, &goal).is_err());

    // A premise that comes later than its row.
    let mut bad = t.clone();
    let last = bad.rows.len() as u32 - 1;
    if let Some(r) = bad.rows.iter_mut().find(|r| !r.premises.is_empty()) {
        r.premises[0] = last;
    }
    assert!(Checker::check(&bad, &goal).is_err());

    // Every rule swapped for another on every row is refused.
    for i in 0..t.rows.len() {
        for rule in [Rule::Refl, Rule::Beta, Rule::Delta, Rule::InstClosed, Rule::LInstNone, Rule::ProofIrrel] {
            if t.rows[i].rule == rule {
                continue;
            }
            let mut bad = t.clone();
            bad.rows[i].rule = rule;
            assert!(Checker::check(&bad, &goal).is_err(), "row {i} by {rule:?} was accepted");
        }
    }

    // The empty context is not a context with an entry.
    let mut bad = t.clone();
    if let Some(c) = bad.ctxs.iter_mut().find(|c| **c == CtxRow::Empty) {
        let first = bad.terms.iter().position(|e| matches!(e, TermRow::Sort(_))).unwrap() as u32;
        *c = CtxRow::Push(0, first);
    }
    assert!(Checker::check(&bad, &goal).is_err());
}

/// `inductive N | zero | succ (n : N)` with its recursor, written by hand as
/// the inductive block's proof would provide them; a computation by the
/// recursor is accepted.
#[test]
fn a_recursor_computes_on_constructors() {
    use cas_lean::tables::{Info, RecRule};
    let mut b = Builder::new();
    let z = b.zero();
    let one = b.succ(z);
    let (prop, type0) = (b.sort(z), b.sort(one));
    let dn =
        b.decl(DeclRow { key: key(1), kind: DeclKind::Inductive, levels: 0, ty: type0, value: None, info: Info::None });
    let n = b.constant(dn, &[]);
    let ctor = |fields| Info::Ctor { induct: key(1), params: 0, fields };
    let dz = b.decl(DeclRow { key: key(2), kind: DeclKind::Constructor, levels: 0, ty: n, value: None, info: ctor(0) });
    let nn = b.pi(n, n);
    let ds =
        b.decl(DeclRow { key: key(3), kind: DeclKind::Constructor, levels: 0, ty: nn, value: None, info: ctor(1) });
    let (zero, succ) = (b.constant(dz, &[]), b.constant(ds, &[]));
    // rec.{u} : (motive : N -> Sort u) -> motive zero
    //   -> ((n : N) -> motive n -> motive (succ n)) -> (t : N) -> motive t
    let u = b.param(0);
    let su = b.sort(u);
    let motive_ty = b.pi(n, su);
    let v: Vec<u32> = (0..4).map(|i| b.bvar(i)).collect();
    let zero_minor = b.app(v[0], zero);
    let mn = b.app(v[2], v[0]);
    let sn = b.app(succ, v[1]);
    let msn = b.app(v[3], sn);
    let ih = b.pi(mn, msn);
    let succ_minor = b.pi(n, ih);
    let result = b.app(v[3], v[0]);
    let t = b.pi(n, result);
    let s = b.pi(succ_minor, t);
    let zz = b.pi(zero_minor, s);
    let rec_ty = b.pi(motive_ty, zz);
    let dr =
        b.decl(DeclRow { key: key(4), kind: DeclKind::Recursor, levels: 1, ty: rec_ty, value: None, info: Info::None });
    // zero: fun motive z s => z;  succ: fun motive z s n => s n (rec motive z s n)
    let l3 = b.lam(succ_minor, v[1]);
    let l2 = b.lam(zero_minor, l3);
    let rhs_zero = b.lam(motive_ty, l2);
    let rec_u = b.constant(dr, &[u]);
    let recur = b.apps(rec_u, &[v[3], v[2], v[1], v[0]]);
    let body = b.apps(v[1], &[v[0], recur]);
    let l4 = b.lam(n, body);
    let l3 = b.lam(succ_minor, l4);
    let l2 = b.lam(zero_minor, l3);
    let rhs_succ = b.lam(motive_ty, l2);
    b.t.decls[dr as usize].info = Info::Rec {
        params: 0,
        motives: 1,
        minors: 2,
        indices: 0,
        rules: vec![
            RecRule { ctor: key(2), fields: 0, rhs: rhs_zero },
            RecRule { ctor: key(3), fields: 1, rhs: rhs_succ },
        ],
    };
    // axioms P : N -> Prop, h : P (succ zero);
    // theorem : P (rec.{1} (fun _ => N) zero (fun n r => succ r) (succ zero)) := h
    let p_ty = b.pi(n, prop);
    let dp = axiom(&mut b, 5, 0, p_ty);
    let p = b.constant(dp, &[]);
    let one_n = b.app(succ, zero);
    let p1 = b.app(p, one_n);
    let dh = axiom(&mut b, 6, 0, p1);
    let h = b.constant(dh, &[]);
    let motive = b.lam(n, n);
    let succ_r = b.app(succ, v[0]);
    let inner = b.lam(n, succ_r);
    let step = b.lam(n, inner);
    let rec1 = b.constant(dr, &[one]);
    let computed = b.apps(rec1, &[motive, zero, step, one_n]);
    let goal_ty = b.app(p, computed);
    let (t, goal) = accepted(&mut b, DeclKind::Theorem, 0, goal_ty, Some(h));
    assert!(t.rows.iter().filter(|r| r.rule == Rule::Iota).count() >= 2);
    // The same with the rules swapped is refused.
    let mut bad = t.clone();
    if let Info::Rec { rules, .. } = &mut bad.decls[dr as usize].info {
        rules.swap(0, 1);
        let (a, c) = (rules[0].rhs, rules[1].rhs);
        rules[0].rhs = c;
        rules[1].rhs = a;
        rules[0].ctor = key(3);
        rules[1].ctor = key(2);
        rules[0].fields = 0;
    }
    assert!(Checker::check(&bad, &goal).is_err());
}

/// Axioms `N : Type`, `n0 : N`, `P : N -> Prop`, `h : P n0`, and a
/// structure `mk : (v : N) -> P v -> S` living in `sort`, with a dependent
/// second field.
fn boxed(b: &mut Builder, sort: u32) -> (u32, u32, u32, u32, u32, u32) {
    use cas_lean::tables::Info;
    let z = b.zero();
    let one = b.succ(z);
    let (prop, type0) = (b.sort(z), b.sort(one));
    let dn = axiom(b, 1, 0, type0);
    let n = b.constant(dn, &[]);
    let dn0 = axiom(b, 2, 0, n);
    let n0 = b.constant(dn0, &[]);
    let p_ty = b.pi(n, prop);
    let dp = axiom(b, 3, 0, p_ty);
    let p = b.constant(dp, &[]);
    let pn0 = b.app(p, n0);
    let dh = axiom(b, 4, 0, pn0);
    let h = b.constant(dh, &[]);
    let induct = Info::Induct { params: 0, indices: 0, ctors: vec![key(6)], recursive: false };
    let ds = b.decl(DeclRow { key: key(5), kind: DeclKind::Inductive, levels: 0, ty: sort, value: None, info: induct });
    let s = b.constant(ds, &[]);
    let v0 = b.bvar(0);
    let pv = b.app(p, v0);
    let rest = b.pi(pv, s);
    let mk_ty = b.pi(n, rest);
    let info = Info::Ctor { induct: key(5), params: 0, fields: 2 };
    let dmk = b.decl(DeclRow { key: key(6), kind: DeclKind::Constructor, levels: 0, ty: mk_ty, value: None, info });
    let mk = b.constant(dmk, &[]);
    let built = b.apps(mk, &[n0, h]);
    (ds, s, p, n0, h, built)
}

#[test]
fn projections_are_typed_and_computed() {
    use cas_lean::tables::Rule;
    let mut b = Builder::new();
    let z = b.zero();
    let one = b.succ(z);
    let type0 = b.sort(one);
    let (ds, s, p, _, h, built) = boxed(&mut b, type0);
    // theorem : (x : S) -> P x.0 := fun x => x.1
    let v0 = b.bvar(0);
    let f0 = b.term(TermRow::Proj(ds, 0, v0));
    let f1 = b.term(TermRow::Proj(ds, 1, v0));
    let pf0 = b.app(p, f0);
    let ty = b.pi(s, pf0);
    let value = b.lam(s, f1);
    let (t, goal) = accepted(&mut b, DeclKind::Theorem, 0, ty, Some(value));
    assert!(t.rows.iter().any(|r| r.rule == Rule::Proj));
    // A wrong field is refused: claim the type of field 1 is N.
    let mut bad = t.clone();
    let i = bad.rows.iter().position(|r| r.rule == Rule::Proj).unwrap();
    if let Fact::Infer { t, .. } = &mut bad.rows[i].fact {
        *t = 0;
    }
    assert!(Checker::check(&bad, &goal).is_err());
    // theorem : P (mk n0 h).0 := h, by computing the projection.
    let mut b2 = Builder::new();
    let z = b2.zero();
    let one = b2.succ(z);
    let type0 = b2.sort(one);
    let (ds, _, p, _, h2, built2) = boxed(&mut b2, type0);
    let _ = (h, built);
    let proj = b2.term(TermRow::Proj(ds, 0, built2));
    let ty = b2.app(p, proj);
    let (t, _) = accepted(&mut b2, DeclKind::Theorem, 0, ty, Some(h2));
    assert!(t.rows.iter().any(|r| r.rule == Rule::ProjCtor));
}

#[test]
fn data_is_not_projected_out_of_a_proposition() {
    let mut b = Builder::new();
    let z = b.zero();
    let prop = b.sort(z);
    let (ds, s, _, n0, _, _) = boxed(&mut b, prop);
    let n_ty = match b.t.terms[n0 as usize] {
        TermRow::Const(d, _) => b.t.decls[d as usize].ty,
        _ => unreachable!(),
    };
    // def : S -> N := fun x => x.0, refused by the kernel ...
    let v0 = b.bvar(0);
    let f0 = b.term(TermRow::Proj(ds, 0, v0));
    let ty = b.pi(s, n_ty);
    let value = b.lam(s, f0);
    assert!(b.check_decl(DeclKind::Definition, 0, ty, Some(value)).is_err());
    // ... and, when the tables are written anyway, by the checker.
    b.lax = true;
    let goal = b.check_decl(DeclKind::Definition, 0, ty, Some(value)).expect("written");
    let err = Checker::check(&b.t, &goal).unwrap_err();
    assert!(err.contains("a field of a proposition is data"), "{err}");
}

fn set_recursive(t: &mut Tables, k: [u8; 32]) {
    use cas_lean::tables::Info;
    for d in &mut t.decls {
        if d.key == k
            && let Info::Induct { recursive, .. } = &mut d.info
        {
            *recursive = true;
        }
    }
}

#[test]
fn a_structure_equals_its_fields_rebuilt() {
    let mut b = Builder::new();
    let z = b.zero();
    let one = b.succ(z);
    let (prop, type0) = (b.sort(z), b.sort(one));
    let (ds, s, _, _, _, _) = boxed(&mut b, type0);
    let mk = match b
        .t
        .terms
        .iter()
        .position(|t| matches!(t, TermRow::Const(d, _) if b.t.decls[*d as usize].key == key(6)))
    {
        Some(i) => i as u32,
        None => unreachable!(),
    };
    // axiom Q : S -> Prop; theorem : (x : S) -> Q x -> Q (mk x.0 x.1) := fun x q => q
    let q_ty = b.pi(s, prop);
    let dq = axiom(&mut b, 7, 0, q_ty);
    let q = b.constant(dq, &[]);
    let (v0, v1) = (b.bvar(0), b.bvar(1));
    let qx0 = b.app(q, v0);
    let (f0, f1) = (b.term(TermRow::Proj(ds, 0, v1)), b.term(TermRow::Proj(ds, 1, v1)));
    let rebuilt = b.apps(mk, &[f0, f1]);
    let q_rebuilt = b.app(q, rebuilt);
    let inner = b.pi(qx0, q_rebuilt);
    let ty = b.pi(s, inner);
    let qx = b.app(q, v0);
    let body = b.lam(qx, v0);
    let value = b.lam(s, body);
    let (mut t, goal) = accepted(&mut b, DeclKind::Theorem, 0, ty, Some(value));
    assert!(t.rows.iter().any(|r| r.rule == Rule::EtaStruct));
    set_recursive(&mut t, key(5));
    assert!(Checker::check(&t, &goal).is_err());
}

#[test]
fn values_of_a_unit_type_are_equal() {
    use cas_lean::tables::Info;
    let mut b = Builder::new();
    let z = b.zero();
    let one = b.succ(z);
    let (prop, type0) = (b.sort(z), b.sort(one));
    let induct = Info::Induct { params: 0, indices: 0, ctors: vec![key(2)], recursive: false };
    let du =
        b.decl(DeclRow { key: key(1), kind: DeclKind::Inductive, levels: 0, ty: type0, value: None, info: induct });
    let u = b.constant(du, &[]);
    let info = Info::Ctor { induct: key(1), params: 0, fields: 0 };
    b.decl(DeclRow { key: key(2), kind: DeclKind::Constructor, levels: 0, ty: u, value: None, info });
    // axiom R : U -> Prop; theorem : (x y : U) -> R x -> R y := fun x y r => r
    let r_ty = b.pi(u, prop);
    let dr = axiom(&mut b, 3, 0, r_ty);
    let r = b.constant(dr, &[]);
    let (v0, v1) = (b.bvar(0), b.bvar(1));
    // Under x and y, x is bvar 1; under the arrow's binder too, y is bvar 1.
    let r1 = b.app(r, v1);
    let arrow = b.pi(r1, r1);
    let inner = b.pi(u, arrow);
    let ty = b.pi(u, inner);
    let body = b.lam(r1, v0);
    let inner = b.lam(u, body);
    let value = b.lam(u, inner);
    let (mut t, goal) = accepted(&mut b, DeclKind::Theorem, 0, ty, Some(value));
    assert!(t.rows.iter().any(|r| r.rule == Rule::UnitLike));
    set_recursive(&mut t, key(1));
    assert!(Checker::check(&t, &goal).is_err());
}

/// `T.rec.{u} : (motive : T -> Sort u) -> motive intro -> (t : T) -> motive t`
/// reduces on any proof `t` of the proposition `T` (K-like reduction).
#[test]
fn a_recursor_on_a_proposition_reduces_on_any_proof() {
    use cas_lean::tables::{Info, RecRule};
    let mut b = Builder::new();
    let z = b.zero();
    let one = b.succ(z);
    let prop = b.sort(z);
    let type0 = b.sort(one);
    let (_, _, p, n0, h, _) = boxed(&mut b, type0);
    let n = match b.t.terms[n0 as usize] {
        TermRow::Const(d, _) => b.t.decls[d as usize].ty,
        _ => unreachable!(),
    };
    let induct = Info::Induct { params: 0, indices: 0, ctors: vec![key(11)], recursive: false };
    let dt =
        b.decl(DeclRow { key: key(10), kind: DeclKind::Inductive, levels: 0, ty: prop, value: None, info: induct });
    let t_ty = b.constant(dt, &[]);
    let info = Info::Ctor { induct: key(10), params: 0, fields: 0 };
    let di = b.decl(DeclRow { key: key(11), kind: DeclKind::Constructor, levels: 0, ty: t_ty, value: None, info });
    let intro = b.constant(di, &[]);
    let u = b.param(0);
    let su = b.sort(u);
    let motive_ty = b.pi(t_ty, su);
    let (v0, v2) = (b.bvar(0), b.bvar(2));
    let minor_ty = b.app(v0, intro);
    let result = b.app(v2, v0);
    let last = b.pi(t_ty, result);
    let rest = b.pi(minor_ty, last);
    let rec_ty = b.pi(motive_ty, rest);
    let inner = b.lam(minor_ty, v0);
    let rhs = b.lam(motive_ty, inner);
    let rules = vec![RecRule { ctor: key(11), fields: 0, rhs }];
    let info = Info::Rec { params: 0, motives: 1, minors: 1, indices: 0, rules };
    let dr = b.decl(DeclRow { key: key(12), kind: DeclKind::Recursor, levels: 1, ty: rec_ty, value: None, info });
    let dax = axiom(&mut b, 13, 0, t_ty);
    let t = b.constant(dax, &[]);
    // theorem : P (T.rec.{1} (fun _ => N) n0 t) := h
    let motive = b.lam(t_ty, n);
    let rec1 = b.constant(dr, &[one]);
    let app = b.apps(rec1, &[motive, n0, t]);
    let ty = b.app(p, app);
    let (t, _) = accepted(&mut b, DeclKind::Theorem, 0, ty, Some(h));
    assert!(t.rows.iter().any(|r| r.rule == Rule::Iota));
    assert!(t.rows.iter().any(|r| r.rule == Rule::ProofIrrel));
}

/// `S.rec` reduces on a variable of the structure S, through eta.
#[test]
fn a_recursor_on_a_structure_reduces_on_a_variable() {
    use cas_lean::tables::{Info, RecRule};
    let mut b = Builder::new();
    let z = b.zero();
    let one = b.succ(z);
    let type0 = b.sort(one);
    let (ds, s, p, n0, _, _) = boxed(&mut b, type0);
    let n = match b.t.terms[n0 as usize] {
        TermRow::Const(d, _) => b.t.decls[d as usize].ty,
        _ => unreachable!(),
    };
    let mk = b.term(TermRow::Const(ds + 1, 0));
    let mk = match b.t.terms[mk as usize] {
        TermRow::Const(d, _) if b.t.decls[d as usize].key == key(6) => mk,
        _ => unreachable!(),
    };
    let u = b.param(0);
    let su = b.sort(u);
    let motive_ty = b.pi(s, su);
    let (v0, v1, v2, v3) = (b.bvar(0), b.bvar(1), b.bvar(2), b.bvar(3));
    let pv0 = b.app(p, v0);
    let built = b.apps(mk, &[v1, v0]);
    let m_built = b.app(v2, built);
    let minor_ty = {
        let inner = b.pi(pv0, m_built);
        b.pi(n, inner)
    };
    let result = b.app(v2, v0);
    let last = b.pi(s, result);
    let rest = b.pi(minor_ty, last);
    let rec_ty = b.pi(motive_ty, rest);
    let body = b.apps(v2, &[v1, v0]);
    let rhs = {
        let l4 = b.lam(pv0, body);
        let l3 = b.lam(n, l4);
        let l2 = b.lam(minor_ty, l3);
        b.lam(motive_ty, l2)
    };
    let _ = v3;
    let rules = vec![RecRule { ctor: key(6), fields: 2, rhs }];
    let info = Info::Rec { params: 0, motives: 1, minors: 1, indices: 0, rules };
    let dr = b.decl(DeclRow { key: key(8), kind: DeclKind::Recursor, levels: 1, ty: rec_ty, value: None, info });
    // theorem : (x : S) -> P (S.rec.{1} (fun _ => N) (fun v _ => v) x) -> P x.0 := fun x h => h
    let motive = b.lam(s, n);
    let first = {
        let l = b.lam(pv0, v1);
        b.lam(n, l)
    };
    let rec1 = b.constant(dr, &[one]);
    let app = b.apps(rec1, &[motive, first, v0]);
    let p_app = b.app(p, app);
    let x0 = b.term(TermRow::Proj(ds, 0, v1));
    let p_x0 = b.app(p, x0);
    let inner = b.pi(p_app, p_x0);
    let ty = b.pi(s, inner);
    let value = {
        let l = b.lam(p_app, v0);
        b.lam(s, l)
    };
    let (t, _) = accepted(&mut b, DeclKind::Theorem, 0, ty, Some(value));
    assert!(t.rows.iter().any(|r| r.rule == Rule::Iota));
    assert!(t.rows.iter().any(|r| r.rule == Rule::EtaStruct));
}
