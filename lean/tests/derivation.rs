//! Declarations of the core calculus: the small kernel writes their derivations,
//! the checker accepts them, and changed derivations are refused.

use cas_lean::derivation::{CtxRow, Fact, LevelRow, Row, Rule, TermRow};
use cas_lean::{Builder, Checker, DeclKind, DeclRow, Derivation, Goal};

fn key(n: u8) -> [u8; 32] {
    [n; 32]
}

fn axiom(b: &mut Builder, n: u8, levels: u32, ty: u32) -> u32 {
    b.decl(DeclRow { key: key(n), kind: DeclKind::Axiom, levels, ty, value: None, info: Default::default() })
}

fn accepted(b: &mut Builder, kind: DeclKind, levels: u32, ty: u32, value: Option<u32>) -> (Derivation, Goal) {
    let goal = b.check_decl(kind, levels, ty, value).expect("the kernel accepts it");
    let t = b.t.clone();
    Checker::check(&t, &goal).expect("the checker accepts the derivation");
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
fn changed_derivations_are_refused() {
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
    use cas_lean::derivation::{Info, RecRule};
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
    use cas_lean::derivation::Info;
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
    let induct = Info::Induct { params: 0, indices: 0, ctors: vec![key(6)], recursive: false, all: vec![key(5)] };
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
    use cas_lean::derivation::Rule;
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
    // ... and, when the derivation is written anyway, by the checker.
    b.lax = true;
    let goal = b.check_decl(DeclKind::Definition, 0, ty, Some(value)).expect("written");
    let err = Checker::check(&b.t, &goal).unwrap_err();
    assert!(err.contains("a field of a proposition is data"), "{err}");
}

fn set_recursive(t: &mut Derivation, k: [u8; 32]) {
    use cas_lean::derivation::Info;
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
    use cas_lean::derivation::Info;
    let mut b = Builder::new();
    let z = b.zero();
    let one = b.succ(z);
    let (prop, type0) = (b.sort(z), b.sort(one));
    let induct = Info::Induct { params: 0, indices: 0, ctors: vec![key(2)], recursive: false, all: vec![key(1)] };
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
    use cas_lean::derivation::{Info, RecRule};
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
    let induct = Info::Induct { params: 0, indices: 0, ctors: vec![key(11)], recursive: false, all: vec![key(10)] };
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
    use cas_lean::derivation::{Info, RecRule};
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

/// `Quot.lift f h (Quot.mk r a)` computes to `f a`. The type of `h` is
/// simplified (`(a b : α) -> r a b -> r a b`) to keep the test free of `Eq`.
#[test]
fn a_function_lifted_to_a_quotient_computes() {
    use cas_lean::derivation::{Info, QuotKind};
    let mut b = Builder::new();
    let z = b.zero();
    let one = b.succ(z);
    let (prop, type0) = (b.sort(z), b.sort(one));
    let (_, _, p, n0, h0, _) = boxed(&mut b, type0);
    let n = match b.t.terms[n0 as usize] {
        TermRow::Const(d, _) => b.t.decls[d as usize].ty,
        _ => unreachable!(),
    };
    let (u, v) = (b.param(0), b.param(1));
    let (su, sv) = (b.sort(u), b.sort(v));
    let vs: Vec<u32> = (0..6).map(|i| b.bvar(i)).collect();
    let quot = |b: &mut Builder, k: u8, levels, ty, kind| {
        b.decl(DeclRow { key: key(k), kind: DeclKind::Quot, levels, ty, value: None, info: Info::Quot(kind) })
    };
    // Quot.{u} : {α : Sort u} -> (α -> α -> Prop) -> Sort u
    let r_ty = {
        let inner = b.pi(vs[1], prop);
        b.pi(vs[0], inner)
    };
    let q_ty = {
        let inner = b.pi(r_ty, su);
        b.pi(su, inner)
    };
    let dq = quot(&mut b, 20, 1, q_ty, QuotKind::Type);
    let quot_u = b.constant(dq, &[u]);
    // Quot.mk.{u} : {α : Sort u} -> (r : α -> α -> Prop) -> α -> Quot r
    let mk_ty = {
        let qr = b.apps(quot_u, &[vs[2], vs[1]]);
        let last = b.pi(vs[1], qr);
        let inner = b.pi(r_ty, last);
        b.pi(su, inner)
    };
    let dmk = quot(&mut b, 21, 1, mk_ty, QuotKind::Mk);
    // Quot.lift.{u,v} : {α} -> {r} -> {β : Sort v} -> (f : α -> β) -> h -> Quot r -> β
    let h_ty = {
        let rab = b.apps(vs[4], &[vs[1], vs[0]]);
        let rab2 = b.apps(vs[5], &[vs[2], vs[1]]);
        let i = b.pi(rab, rab2);
        let i = b.pi(vs[4], i);
        b.pi(vs[3], i)
    };
    let lift_ty = {
        let qr = b.apps(quot_u, &[vs[4], vs[3]]);
        let last = b.pi(qr, vs[3]);
        let i = b.pi(h_ty, last);
        let f_ty = b.pi(vs[2], vs[1]);
        let i = b.pi(f_ty, i);
        let i = b.pi(sv, i);
        let i = b.pi(r_ty, i);
        b.pi(su, i)
    };
    let dlift = quot(&mut b, 22, 2, lift_ty, QuotKind::Lift);
    // axiom R : N -> N -> Prop
    let rel_ty = {
        let i = b.pi(n, prop);
        b.pi(n, i)
    };
    let dr = axiom(&mut b, 23, 0, rel_ty);
    let rel = b.constant(dr, &[]);
    // theorem : P (Quot.lift.{1,1} N R N (fun x => x) (fun a b p => p) (Quot.mk.{1} N R n0)) := h0
    let id = b.lam(n, vs[0]);
    let hstub = {
        let rab = b.apps(rel, &[vs[1], vs[0]]);
        let i = b.lam(rab, vs[0]);
        let i = b.lam(n, i);
        b.lam(n, i)
    };
    let mk1 = b.constant(dmk, &[one]);
    let built = b.apps(mk1, &[n, rel, n0]);
    let lift11 = b.constant(dlift, &[one, one]);
    let lifted = b.apps(lift11, &[n, rel, n, id, hstub, built]);
    let ty = b.app(p, lifted);
    let (t, goal) = accepted(&mut b, DeclKind::Theorem, 0, ty, Some(h0));
    assert!(t.rows.iter().any(|r| r.rule == Rule::QuotLift));
    // Quot.mk marked as Quot.ind no longer computes.
    let mut bad = t.clone();
    bad.decls[dmk as usize].info = Info::Quot(QuotKind::Ind);
    assert!(Checker::check(&bad, &goal).is_err());
}

/// The constants literals need, cited and named as primitives: `Nat` with
/// its constructors and recursor, `Nat.add`, `Bool`, `Nat.beq`, `Char`,
/// `Char.ofNat`, `List`, `String` and `String.mk`. Returns the term `Nat`.
fn literal_prelude(b: &mut Builder) -> u32 {
    use cas_lean::derivation::{Info, Prim, RecRule};
    let z = b.zero();
    let one = b.succ(z);
    let type0 = b.sort(one);
    let cite = |b: &mut Builder, k: u8, kind, levels, ty, info, prim: Option<Prim>| {
        let d = b.decl(DeclRow { key: key(k), kind, levels, ty, value: None, info });
        if let Some(p) = prim {
            b.prim(p, d);
        }
        d
    };
    let induct = |ctors: Vec<[u8; 32]>, recursive, me: u8| Info::Induct {
        params: 0,
        indices: 0,
        ctors,
        recursive,
        all: vec![key(me)],
    };
    let ctor = |induct: u8, fields| Info::Ctor { induct: key(induct), params: 0, fields };
    let dn = cite(b, 30, DeclKind::Inductive, 0, type0, induct(vec![key(31), key(32)], true, 30), Some(Prim::Nat));
    let nat = b.constant(dn, &[]);
    let dz = cite(b, 31, DeclKind::Constructor, 0, nat, ctor(30, 0), Some(Prim::NatZero));
    let nn = b.pi(nat, nat);
    let ds = cite(b, 32, DeclKind::Constructor, 0, nn, ctor(30, 1), Some(Prim::NatSucc));
    let nnn = b.pi(nat, nn);
    cite(b, 33, DeclKind::Axiom, 0, nnn, Info::None, Some(Prim::NatAdd));
    let db = cite(b, 34, DeclKind::Inductive, 0, type0, induct(vec![key(35), key(36)], false, 34), None);
    let bool_ = b.constant(db, &[]);
    cite(b, 35, DeclKind::Constructor, 0, bool_, ctor(34, 0), Some(Prim::BoolFalse));
    cite(b, 36, DeclKind::Constructor, 0, bool_, ctor(34, 0), Some(Prim::BoolTrue));
    let nnb = {
        let i = b.pi(nat, bool_);
        b.pi(nat, i)
    };
    cite(b, 37, DeclKind::Axiom, 0, nnb, Info::None, Some(Prim::NatBeq));
    let dc = cite(b, 38, DeclKind::Axiom, 0, type0, Info::None, Some(Prim::Char));
    let char_ = b.constant(dc, &[]);
    let nc = b.pi(nat, char_);
    cite(b, 39, DeclKind::Axiom, 0, nc, Info::None, Some(Prim::CharOfNat));
    let u = b.param(0);
    let su1 = {
        let s = b.succ(u);
        b.sort(s)
    };
    let list_ty = b.pi(su1, su1);
    let dl = cite(b, 40, DeclKind::Axiom, 1, list_ty, Info::None, None);
    let list_u = b.constant(dl, &[u]);
    let (v0, v1, v2) = (b.bvar(0), b.bvar(1), b.bvar(2));
    let nil_ty = {
        let l = b.app(list_u, v0);
        b.pi(su1, l)
    };
    cite(b, 41, DeclKind::Axiom, 1, nil_ty, Info::None, Some(Prim::ListNil));
    let cons_ty = {
        let l1 = b.app(list_u, v1);
        let l2 = b.app(list_u, v2);
        let i = b.pi(l1, l2);
        let i = b.pi(v0, i);
        b.pi(su1, i)
    };
    cite(b, 42, DeclKind::Axiom, 1, cons_ty, Info::None, Some(Prim::ListCons));
    let dstr = cite(b, 43, DeclKind::Axiom, 0, type0, Info::None, Some(Prim::String));
    let string = b.constant(dstr, &[]);
    let list_char = {
        let l0 = b.constant(dl, &[z]);
        b.app(l0, char_)
    };
    let mk_ty = b.pi(list_char, string);
    cite(b, 44, DeclKind::Axiom, 0, mk_ty, Info::None, Some(Prim::StringMk));
    // Nat.rec.{u} : (motive : Nat -> Sort u) -> motive zero
    //   -> ((n : Nat) -> motive n -> motive (succ n)) -> (t : Nat) -> motive t
    let (zero, succ) = (b.constant(dz, &[]), b.constant(ds, &[]));
    let su = b.sort(u);
    let motive_ty = b.pi(nat, su);
    let v3 = b.bvar(3);
    let zero_minor = b.app(v0, zero);
    let mn = b.app(v2, v0);
    let sn = b.app(succ, v1);
    let msn = b.app(v3, sn);
    let ih = b.pi(mn, msn);
    let succ_minor = b.pi(nat, ih);
    let result = b.app(v3, v0);
    let rec_ty = {
        let t = b.pi(nat, result);
        let s = b.pi(succ_minor, t);
        let z = b.pi(zero_minor, s);
        b.pi(motive_ty, z)
    };
    let l3 = b.lam(succ_minor, v1);
    let l2 = b.lam(zero_minor, l3);
    let rhs_zero = b.lam(motive_ty, l2);
    // The rules mention the recursor itself, so they are set once it is cited.
    let info = Info::Rec { params: 0, motives: 1, minors: 2, indices: 0, rules: vec![] };
    let dr = cite(b, 45, DeclKind::Recursor, 1, rec_ty, info, None);
    let rec_u = b.constant(dr, &[u]);
    let recur = b.apps(rec_u, &[v3, v2, v1, v0]);
    let body = b.apps(v1, &[v0, recur]);
    let l4 = b.lam(nat, body);
    let l3 = b.lam(succ_minor, l4);
    let l2 = b.lam(zero_minor, l3);
    let rhs_succ = b.lam(motive_ty, l2);
    b.t.decls[dr as usize].info = Info::Rec {
        params: 0,
        motives: 1,
        minors: 2,
        indices: 0,
        rules: vec![
            RecRule { ctor: key(31), fields: 0, rhs: rhs_zero },
            RecRule { ctor: key(32), fields: 1, rhs: rhs_succ },
        ],
    };
    nat
}

/// A predicate on `ty` and a proof that it holds of `at`: `(P, h)`.
fn holds(b: &mut Builder, k: u8, ty: u32, at: u32) -> (u32, u32) {
    let z = b.zero();
    let prop = b.sort(z);
    let p_ty = b.pi(ty, prop);
    let dp = axiom(b, k, 0, p_ty);
    let p = b.constant(dp, &[]);
    let pa = b.app(p, at);
    let dh = axiom(b, k + 1, 0, pa);
    (p, b.constant(dh, &[]))
}

fn constant_named(b: &mut Builder, k: u8, levels: &[u32]) -> u32 {
    let d = b.t.decls.iter().position(|d| d.key == key(k)).expect("cited") as u32;
    b.constant(d, levels)
}

#[test]
fn the_kernel_arithmetic_on_literals_is_checked() {
    use num_bigint::BigUint;
    let mut b = Builder::new();
    let nat = literal_prelude(&mut b);
    let lit = |b: &mut Builder, n: u32| b.nat_lit(&BigUint::from(n));
    // P (Nat.add 2 3) from P 5, and P (Nat.succ 4) from P 5.
    let five = lit(&mut b, 5);
    let (p, h) = holds(&mut b, 50, nat, five);
    let add = constant_named(&mut b, 33, &[]);
    let (two, three, four) = (lit(&mut b, 2), lit(&mut b, 3), lit(&mut b, 4));
    let sum = b.apps(add, &[two, three]);
    let ty = b.app(p, sum);
    let (t, goal) = accepted(&mut b, DeclKind::Theorem, 0, ty, Some(h));
    assert!(t.rows.iter().any(|r| r.rule == Rule::NatBin));
    let mut bad = t.clone();
    let i = bad.terms.iter().position(|e| *e == TermRow::Lit(vec![5])).unwrap();
    bad.terms[i] = TermRow::Lit(vec![6]);
    assert!(Checker::check(&bad, &goal).is_err());
    let succ = constant_named(&mut b, 32, &[]);
    let s4 = b.app(succ, four);
    let ty = b.app(p, s4);
    let (t, _) = accepted(&mut b, DeclKind::Theorem, 0, ty, Some(h));
    assert!(t.rows.iter().any(|r| r.rule == Rule::NatSucc));
    // R (Nat.beq 3 3) gives R Bool.true.
    let bool_true = constant_named(&mut b, 36, &[]);
    let bool_ = match b.t.terms[bool_true as usize] {
        TermRow::Const(d, _) => b.t.decls[d as usize].ty,
        _ => unreachable!(),
    };
    let beq = constant_named(&mut b, 37, &[]);
    let eq33 = b.apps(beq, &[three, three]);
    let (r, hr) = holds(&mut b, 52, bool_, eq33);
    let ty = b.app(r, bool_true);
    accepted(&mut b, DeclKind::Theorem, 0, ty, Some(hr));
}

#[test]
fn a_recursor_runs_on_a_literal() {
    use num_bigint::BigUint;
    let mut b = Builder::new();
    let nat = literal_prelude(&mut b);
    let lit = |b: &mut Builder, n: u32| b.nat_lit(&BigUint::from(n));
    let nine = lit(&mut b, 9);
    let (p, h) = holds(&mut b, 50, nat, nine);
    // P (Nat.rec.{1} (fun _ => Nat) 7 (fun n r => Nat.succ r) 2) from P 9
    let z = b.zero();
    let one = b.succ(z);
    let rec1 = constant_named(&mut b, 45, &[one]);
    let succ = constant_named(&mut b, 32, &[]);
    let motive = b.lam(nat, nat);
    let v0 = b.bvar(0);
    let sr = b.app(succ, v0);
    let step = {
        let i = b.lam(nat, sr);
        b.lam(nat, i)
    };
    let (seven, two) = (lit(&mut b, 7), lit(&mut b, 2));
    let run = b.apps(rec1, &[motive, seven, step, two]);
    let ty = b.app(p, run);
    let (t, _) = accepted(&mut b, DeclKind::Theorem, 0, ty, Some(h));
    assert!(t.rows.iter().filter(|r| r.rule == Rule::Iota).count() >= 3);
}

#[test]
fn a_string_literal_is_its_characters() {
    let mut b = Builder::new();
    literal_prelude(&mut b);
    let z = b.zero();
    let string = constant_named(&mut b, 43, &[]);
    let ch = constant_named(&mut b, 38, &[]);
    let of_nat = constant_named(&mut b, 39, &[]);
    let nil = constant_named(&mut b, 41, &[z]);
    let cons = constant_named(&mut b, 42, &[z]);
    let mk = constant_named(&mut b, 44, &[]);
    let mut list = b.app(nil, ch);
    for c in "hé".chars().rev() {
        let l = b.nat_lit(&(c as u32).into());
        let cv = b.app(of_nat, l);
        list = b.apps(cons, &[ch, cv, list]);
    }
    let built = b.app(mk, list);
    let (q, h) = holds(&mut b, 50, string, built);
    let s = b.term(TermRow::Str("hé".as_bytes().to_vec()));
    let ty = b.app(q, s);
    let (t, goal) = accepted(&mut b, DeclKind::Theorem, 0, ty, Some(h));
    assert!(t.rows.iter().any(|r| r.rule == Rule::StrLit));
    let mut bad = t.clone();
    let i = bad.terms.iter().position(|e| *e == TermRow::Str("hé".as_bytes().to_vec())).unwrap();
    bad.terms[i] = TermRow::Str(b"he".to_vec());
    assert!(Checker::check(&bad, &goal).is_err());
}

/// `inductive N | zero | succ (n : N)`: the block's own check, with the
/// recursor built by the kernel and compared by the checker.
#[test]
fn an_inductive_block_is_checked_and_its_recursor_built() {
    use cas_lean::derivation::{BlockGoal, Info};
    let mut b = Builder::new();
    let z = b.zero();
    let one = b.succ(z);
    let type0 = b.sort(one);
    let cite = |b: &mut Builder, k: u8, kind, ty| {
        b.decl(DeclRow { key: key(k), kind, levels: 0, ty, value: None, info: Info::None })
    };
    let dn = cite(&mut b, 1, DeclKind::Inductive, type0);
    let n = b.constant(dn, &[]);
    let dz = cite(&mut b, 2, DeclKind::Constructor, n);
    let nn = b.pi(n, n);
    let ds = cite(&mut b, 3, DeclKind::Constructor, nn);
    let goal: BlockGoal = b.inductive_block(0, 0, &[dn], &[vec![dz, ds]], &[key(4)]).expect("the kernel accepts it");
    let t = b.t.clone();
    Checker::check_block(&t, &goal).expect("the checker accepts the block");
    let rec = &t.decls[goal.recursors[0] as usize];
    assert_eq!(rec.levels, 1);
    // A recursor with its rules swapped is refused.
    let mut bad = t.clone();
    if let Info::Rec { rules, .. } = &mut bad.decls[goal.recursors[0] as usize].info {
        let (a, c) = (rules[0].rhs, rules[1].rhs);
        rules[0].rhs = c;
        rules[1].rhs = a;
    }
    assert!(Checker::check_block(&bad, &goal).is_err());
    // A negative occurrence is refused: `bad : (N' -> N') -> N'`.
    let mut b = Builder::new();
    let z = b.zero();
    let one = b.succ(z);
    let type0 = b.sort(one);
    let dn = cite(&mut b, 1, DeclKind::Inductive, type0);
    let n = b.constant(dn, &[]);
    let nn = b.pi(n, n);
    let neg = b.pi(nn, n);
    let dc = cite(&mut b, 2, DeclKind::Constructor, neg);
    assert!(b.inductive_block(0, 0, &[dn], &[vec![dc]], &[key(4)]).is_err());
}

fn cite_as(b: &mut Builder, k: u8, kind: DeclKind, levels: u32, ty: u32) -> u32 {
    b.decl(DeclRow { key: key(k), kind, levels, ty, value: None, info: Default::default() })
}

/// `List.{u} (α : Type u)`, mutual `Even`/`Odd` on `N` with indices, and
/// a reflexive `T | sup (f : N -> T)`: each block checked, and the
/// recursor's universe as Lean decides it.
#[test]
fn parameters_indices_mutual_and_reflexive_blocks() {
    use cas_lean::derivation::BlockGoal;
    // Each block has its own derivation, as each declaration does.
    let nat = |b: &mut Builder| {
        let z = b.zero();
        let one = b.succ(z);
        let type0 = b.sort(one);
        let dn = cite_as(b, 1, DeclKind::Inductive, 0, type0);
        let n = b.constant(dn, &[]);
        let dz = cite_as(b, 2, DeclKind::Constructor, 0, n);
        let nn = b.pi(n, n);
        let ds = cite_as(b, 3, DeclKind::Constructor, 0, nn);
        b.inductive_block(0, 0, &[dn], &[vec![dz, ds]], &[key(4)]).unwrap();
        let (zero, succ) = (b.constant(dz, &[]), b.constant(ds, &[]));
        (n, zero, succ)
    };
    let checked = |b: &Builder, g: &BlockGoal| {
        let t = b.t.clone();
        Checker::check_block(&t, g).expect("the checker accepts the block");
        t
    };
    // List.{u} (α : Type u) : Type u | nil | cons (a : α) (t : List α)
    let mut b = Builder::new();
    let u = b.param(0);
    let su1 = {
        let s = b.succ(u);
        b.sort(s)
    };
    let list_ty = b.pi(su1, su1);
    let dl = cite_as(&mut b, 10, DeclKind::Inductive, 1, list_ty);
    let list_u = b.constant(dl, &[u]);
    let (v0, v1, v2) = (b.bvar(0), b.bvar(1), b.bvar(2));
    let nil_ty = {
        let l = b.app(list_u, v0);
        b.pi(su1, l)
    };
    let dnil = cite_as(&mut b, 11, DeclKind::Constructor, 1, nil_ty);
    let cons_ty = {
        let l1 = b.app(list_u, v1);
        let l2 = b.app(list_u, v2);
        let i = b.pi(l1, l2);
        let i = b.pi(v0, i);
        b.pi(su1, i)
    };
    let dcons = cite_as(&mut b, 12, DeclKind::Constructor, 1, cons_ty);
    let g = b.inductive_block(1, 1, &[dl], &[vec![dnil, dcons]], &[key(13)]).unwrap();
    let t = checked(&b, &g);
    assert_eq!(t.decls[g.recursors[0] as usize].levels, 2, "List.rec has the elimination level and u");
    // mutual Even Odd : N -> Prop
    let mut b = Builder::new();
    let (n, zero, succ) = nat(&mut b);
    let z = b.zero();
    let prop = b.sort(z);
    let (v0, v1) = (b.bvar(0), b.bvar(1));
    let pred = b.pi(n, prop);
    let de = cite_as(&mut b, 20, DeclKind::Inductive, 0, pred);
    let dodd = cite_as(&mut b, 21, DeclKind::Inductive, 0, pred);
    let (even, odd) = (b.constant(de, &[]), b.constant(dodd, &[]));
    let ez = b.app(even, zero);
    let dez = cite_as(&mut b, 22, DeclKind::Constructor, 0, ez);
    let sn = b.app(succ, v1);
    let es = {
        let on = b.app(odd, v0);
        let e1 = b.app(even, sn);
        let i = b.pi(on, e1);
        b.pi(n, i)
    };
    let des = cite_as(&mut b, 23, DeclKind::Constructor, 0, es);
    let os = {
        let en = b.app(even, v0);
        let o1 = b.app(odd, sn);
        let i = b.pi(en, o1);
        b.pi(n, i)
    };
    let dos = cite_as(&mut b, 24, DeclKind::Constructor, 0, os);
    let g = b.inductive_block(0, 0, &[de, dodd], &[vec![dez, des], vec![dos]], &[key(25), key(26)]).unwrap();
    let t = checked(&b, &g);
    let lv = |i: usize| t.decls[g.recursors[i] as usize].levels;
    assert_eq!((lv(0), lv(1)), (0, 0), "Even.rec eliminates only into Prop");
    let mut bad = t.clone();
    bad.decls[g.recursors[0] as usize].levels = 1;
    assert!(Checker::check_block(&bad, &g).is_err(), "a recursor at the wrong universe");
    // T | sup (f : N -> T)
    let mut b = Builder::new();
    let (n, _, _) = nat(&mut b);
    let z = b.zero();
    let one = b.succ(z);
    let type0 = b.sort(one);
    let dt = cite_as(&mut b, 30, DeclKind::Inductive, 0, type0);
    let t_ = b.constant(dt, &[]);
    let nt = b.pi(n, t_);
    let sup = b.pi(nt, t_);
    let dsup = cite_as(&mut b, 31, DeclKind::Constructor, 0, sup);
    let g = b.inductive_block(0, 0, &[dt], &[vec![dsup]], &[key(32)]).unwrap();
    let t = checked(&b, &g);
    assert_eq!(t.decls[g.recursors[0] as usize].levels, 1);
}

/// The recursor the block check builds for `N` is the one Lean prints:
/// `N.rec.{u} : {motive : N -> Sort u} -> motive N.zero ->
/// ((n : N) -> motive n -> motive n.succ) -> (t : N) -> motive t`, with the
/// rules `fun motive zero succ => zero` and
/// `fun motive zero succ n => succ n (N.rec zero succ n)`.
#[test]
fn the_built_recursor_is_leans() {
    let mut b = Builder::new();
    let z = b.zero();
    let one = b.succ(z);
    let type0 = b.sort(one);
    let dn = cite_as(&mut b, 1, DeclKind::Inductive, 0, type0);
    let n = b.constant(dn, &[]);
    let dz = cite_as(&mut b, 2, DeclKind::Constructor, 0, n);
    let nn = b.pi(n, n);
    let ds = cite_as(&mut b, 3, DeclKind::Constructor, 0, nn);
    let g = b.inductive_block(0, 0, &[dn], &[vec![dz, ds]], &[key(4)]).unwrap();
    let dr = g.recursors[0];
    let (zero, succ) = (b.constant(dz, &[]), b.constant(ds, &[]));
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
    assert_eq!(b.t.decls[dr as usize].ty, rec_ty, "the recursor's type");
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
    let cas_lean::derivation::Info::Rec { rules, .. } = &b.t.decls[dr as usize].info else { panic!() };
    assert_eq!((rules[0].rhs, rules[1].rhs), (rhs_zero, rhs_succ), "the rules");
}

/// `List.{u} (α : Type u)` cited with its constructors and information,
/// as an earlier block left it. Returns the row of `List` and of its
/// constructors.
fn cite_list(b: &mut Builder) -> (u32, u32, u32) {
    use cas_lean::derivation::Info;
    let u = b.param(0);
    let su1 = {
        let s = b.succ(u);
        b.sort(s)
    };
    let list_ty = b.pi(su1, su1);
    let info =
        Info::Induct { params: 1, indices: 0, ctors: vec![key(61), key(62)], recursive: true, all: vec![key(60)] };
    let dl = b.decl(DeclRow { key: key(60), kind: DeclKind::Inductive, levels: 1, ty: list_ty, value: None, info });
    let list_u = b.constant(dl, &[u]);
    let (v0, v1, v2) = (b.bvar(0), b.bvar(1), b.bvar(2));
    let nil_ty = {
        let l = b.app(list_u, v0);
        b.pi(su1, l)
    };
    let cons_ty = {
        let l1 = b.app(list_u, v1);
        let l2 = b.app(list_u, v2);
        let i = b.pi(l1, l2);
        let i = b.pi(v0, i);
        b.pi(su1, i)
    };
    let ctor = |fields| Info::Ctor { induct: key(60), params: 1, fields };
    let dnil = b.decl(DeclRow {
        key: key(61),
        kind: DeclKind::Constructor,
        levels: 1,
        ty: nil_ty,
        value: None,
        info: ctor(0),
    });
    let dcons = b.decl(DeclRow {
        key: key(62),
        kind: DeclKind::Constructor,
        levels: 1,
        ty: cons_ty,
        value: None,
        info: ctor(2),
    });
    (dl, dnil, dcons)
}

/// `inductive Tree | node : List Tree -> Tree`: Lean's kernel checks it
/// as the mutual block of `Tree` and an auxiliary copy of `List Tree`, and
/// the recursors it adds are `Tree.rec` and `Tree.rec_1`, with `List Tree`
/// in place of the copy. The checker accepts exactly those.
#[test]
fn a_nested_inductive_type_is_checked_through_its_mutual_block() {
    use cas_lean::derivation::Info;
    let mut b = Builder::new();
    let (dl, dnil, dcons) = cite_list(&mut b);
    let z = b.zero();
    let one = b.succ(z);
    let type0 = b.sort(one);
    let dt = cite_as(&mut b, 70, DeclKind::Inductive, 0, type0);
    let tree = b.constant(dt, &[]);
    let list0 = b.constant(dl, &[z]);
    let list_tree = b.app(list0, tree);
    let node_ty = b.pi(list_tree, tree);
    let dnode = cite_as(&mut b, 71, DeclKind::Constructor, 0, node_ty);
    let goal = b.nested_block(0, 0, &[dt], &[vec![dnode]], &[key(72), key(73)]).expect("the kernel accepts Tree");
    let t = b.t.clone();
    Checker::check_nested(&t, &goal).expect("the checker accepts Tree");
    assert_eq!(goal.aux.types.len(), 2, "Tree and the copy of List Tree");
    // Tree.rec.{u} : (motive_1 : Tree -> Sort u) -> (motive_2 : List Tree -> Sort u)
    //   -> ((a : List Tree) -> motive_2 a -> motive_1 (node a))
    //   -> motive_2 [] -> ((head : Tree) -> (tail : List Tree) -> motive_1 head
    //   -> motive_2 tail -> motive_2 (head :: tail)) -> (t : Tree) -> motive_1 t
    let u = b.param(0);
    let su = b.sort(u);
    let v: Vec<u32> = (0..8).map(|i| b.bvar(i)).collect();
    let node = b.constant(dnode, &[]);
    let nil = {
        let n = b.constant(dnil, &[z]);
        b.app(n, tree)
    };
    let cons = {
        let c = b.constant(dcons, &[z]);
        b.app(c, tree)
    };
    let m1 = b.pi(tree, su);
    let m2 = b.pi(list_tree, su);
    let node_minor = {
        let na = b.app(node, v[1]);
        let r = b.app(v[3], na);
        let ih = b.app(v[1], v[0]);
        let i = b.pi(ih, r);
        b.pi(list_tree, i)
    };
    let nil_minor = b.app(v[1], nil);
    let cons_minor = {
        let ht = b.apps(cons, &[v[3], v[2]]);
        let r = b.app(v[6], ht);
        let ih2 = b.app(v[5], v[1]);
        let ih1 = b.app(v[5], v[1]);
        let i = b.pi(ih2, r);
        let i = b.pi(ih1, i);
        let i = b.pi(list_tree, i);
        b.pi(tree, i)
    };
    let prefix = |b: &mut Builder, tail: u32| {
        let r = b.pi(cons_minor, tail);
        let r = b.pi(nil_minor, r);
        let r = b.pi(node_minor, r);
        let r = b.pi(m2, r);
        b.pi(m1, r)
    };
    let tail = {
        let r = b.app(v[5], v[0]);
        b.pi(tree, r)
    };
    let rec_ty = prefix(&mut b, tail);
    assert_eq!(b.t.decls[goal.recursors[0] as usize].ty, rec_ty, "Tree.rec is Lean's");
    let tail1 = {
        let r = b.app(v[4], v[0]);
        b.pi(list_tree, r)
    };
    let rec1_ty = prefix(&mut b, tail1);
    assert_eq!(b.t.decls[goal.recursors[1] as usize].ty, rec1_ty, "Tree.rec_1 is Lean's");
    let Info::Rec { rules, .. } = &t.decls[goal.recursors[1] as usize].info else { panic!() };
    assert_eq!((rules[0].ctor, rules[1].ctor), (key(61), key(62)), "rec_1 computes on List.nil and List.cons");
    let Info::Induct { recursive, all, .. } = &t.decls[dt as usize].info else { panic!() };
    assert!(*recursive && *all == vec![key(70)]);
    // Tampering is refused: rules swapped, a rule on the copy's
    // constructor, and the auxiliary type left in a recursor's type.
    let mut bad = t.clone();
    if let Info::Rec { rules, .. } = &mut bad.decls[goal.recursors[1] as usize].info {
        rules.swap(0, 1);
    }
    assert!(Checker::check_nested(&bad, &goal).is_err());
    let mut bad = t.clone();
    let Info::Induct { ctors: copy_ctors, .. } = t.decls[goal.aux.types[1] as usize].info.clone() else { panic!() };
    if let Info::Rec { rules, .. } = &mut bad.decls[goal.recursors[1] as usize].info {
        rules[0].ctor = copy_ctors[0];
    }
    assert!(Checker::check_nested(&bad, &goal).is_err());
    let mut bad = t.clone();
    bad.decls[goal.recursors[1] as usize].ty = bad.decls[goal.aux.recursors[1] as usize].ty;
    assert!(Checker::check_nested(&bad, &goal).is_err());
    let mut bad = t.clone();
    bad.decls[dt as usize].info =
        Info::Induct { params: 0, indices: 0, ctors: vec![key(71)], recursive: false, all: vec![key(70)] };
    assert!(Checker::check_nested(&bad, &goal).is_err(), "Tree is recursive");
}

/// `R | mk : List (List R) -> R` needs two auxiliary types, found one
/// inside the other; `S | mk : (T : Type) -> List (T -> S) -> S` is refused,
/// since a nested type's parameter may not mention a field.
#[test]
fn nested_occurrences_inside_nested_ones_and_local_parameters() {
    let mut b = Builder::new();
    let (dl, _, _) = cite_list(&mut b);
    let z = b.zero();
    let one = b.succ(z);
    let type0 = b.sort(one);
    let dr = cite_as(&mut b, 80, DeclKind::Inductive, 0, type0);
    let r = b.constant(dr, &[]);
    let list0 = b.constant(dl, &[z]);
    let lr = b.app(list0, r);
    let llr = b.app(list0, lr);
    let mk_ty = b.pi(llr, r);
    let dmk = cite_as(&mut b, 81, DeclKind::Constructor, 0, mk_ty);
    let keys = [key(82), key(83), key(84)];
    let goal = b.nested_block(0, 0, &[dr], &[vec![dmk]], &keys).expect("the kernel accepts R");
    assert_eq!(goal.aux.types.len(), 3);
    let t = b.t.clone();
    Checker::check_nested(&t, &goal).expect("the checker accepts R");
    // S, in a builder of its own.
    let mut b = Builder::new();
    let (dl, _, _) = cite_list(&mut b);
    let z = b.zero();
    let one = b.succ(z);
    let type0 = b.sort(one);
    let ds = cite_as(&mut b, 90, DeclKind::Inductive, 0, type0);
    let s = b.constant(ds, &[]);
    let list0 = b.constant(dl, &[z]);
    let v0 = b.bvar(0);
    let ts = b.pi(v0, s);
    let lts = b.app(list0, ts);
    let inner = b.pi(lts, s);
    let mk_ty = b.pi(type0, inner);
    let dmk = cite_as(&mut b, 91, DeclKind::Constructor, 0, mk_ty);
    let err = b.nested_block(0, 0, &[ds], &[vec![dmk]], &[key(92), key(93)]).unwrap_err();
    assert!(err.contains("local variables"), "{err}");
}
