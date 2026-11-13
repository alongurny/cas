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
