//! Writes the derivation of Lean declarations as plain rows, for circuit
//! experiments outside this crate. Terms, levels, lists, contexts and
//! declarations are first made canonical (equal structure, one row), so that
//! equal ids mean equal terms, as the checker's fingerprints do.
//!
//! Usage: `lean_dump <export> <out dir> NAME...`. One file per declaration,
//! `<out dir>/<NAME>.rows`, lines:
//!
//! ```text
//! L <i> zero | succ <a> | max <a> <b> | imax <a> <b> | param <n>
//! S <i> nil | cons <level> <list>
//! C <i> empty | push <ctx> <term>
//! T <i> bvar <j>
//! T <i> leaf <sub> <a> <b>      sub: sort l | const d ls | fvar n | lit h | str h
//! T <i> app|lam|pi <a> <b>
//! T <i> let <a> <v> <b>
//! T <i> proj <d> <n> <x>
//! D <i> <unfoldable 0|1> <levels> <value or -1> <key0> <key1> <kind> <type> <order>
//! F <i> <Rule> <fact> <x1> <x2> <x3> <x4> : <premises>
//! G <key0> <key1> <kind> <levels> <type> <value> <ty infer> <ty eq> <value infer> <value eq> <is prop> <order> <empty context>
//! ```
//!
//! A key is the first 16 bytes of the declaration's key as two
//! little-endian u64; kinds number `DeclKind` from 1 (axiom) in order. The
//! goal row `G` is the declaration checked, its own key, and the facts that
//! establish it (-1 where it has none). A declaration's order is its
//! position in a dependency order (see `topo`); a citation must name a
//! declaration of smaller order. Usage: `lean_dump <export> <out dir>
//! [--order FILE] (--names FILE | NAME...)`.
//!
//! Facts: has (g n a), infer (g e t), eq (g a b), inst (e k a r), linst
//! (e ls r), linstl (l ls r), leq (l m); missing fields are 0. Children come
//! before their parents, and premises before the facts that use them.

use cas_lean::derivation::{CtxRow, DeclKind, Derivation, Fact, Goal, Info, Key, LevelRow, ListRow, TermRow};
use cas_lean::derive::{Builder, Cited, Source};
use cas_lean::import::{Env, Kind};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

struct Shared(Arc<Env>);
impl Source for Shared {
    fn cite(&self, key: &Key) -> Option<Cited> {
        self.0.cite(key)
    }
    fn name(&self, key: &Key) -> Option<String> {
        self.0.by_key(key).map(|d| self.0.name(d.name))
    }
}

/// Canonical numbering of one table: each row's id is the first row with
/// the same (already canonical) content.
struct Canon<K> {
    seen: HashMap<K, u32>,
    map: Vec<u32>,
}
impl<K: std::hash::Hash + Eq> Canon<K> {
    fn new() -> Self {
        Canon { seen: HashMap::new(), map: Vec::new() }
    }
    /// Gives row `map.len()` content `k`; true when it is new.
    fn push(&mut self, k: K) -> bool {
        let n = self.seen.len() as u32;
        let id = *self.seen.entry(k).or_insert(n);
        self.map.push(id);
        id == n
    }
}

fn h64(b: &[u8]) -> u64 {
    u64::from_le_bytes(blake3::hash(b).as_bytes()[..8].try_into().unwrap())
}

fn key_words(k: &Key) -> (u64, u64) {
    (u64::from_le_bytes(k[..8].try_into().unwrap()), u64::from_le_bytes(k[8..16].try_into().unwrap()))
}

fn kind_no(k: DeclKind) -> u8 {
    match k {
        DeclKind::Axiom => 1,
        DeclKind::Definition => 2,
        DeclKind::Theorem => 3,
        DeclKind::Opaque => 4,
        DeclKind::Inductive => 5,
        DeclKind::Constructor => 6,
        DeclKind::Recursor => 7,
        DeclKind::Quot => 8,
    }
}

fn dump(t: &Derivation, goal: Option<&Goal>, own: &Key, order: &dyn Fn(&Key) -> i64) -> String {
    let mut out = String::new();
    let mut lv = Canon::new();
    for l in &t.levels {
        let m = |i: &u32| lv.map[*i as usize];
        let k = match l {
            LevelRow::Zero => (0, 0, 0),
            LevelRow::Succ(a) => (1, m(a), 0),
            LevelRow::Max(a, b) => (2, m(a), m(b)),
            LevelRow::IMax(a, b) => (3, m(a), m(b)),
            LevelRow::Param(n) => (4, *n, 0),
        };
        if lv.push(k) {
            let line = match k {
                (0, _, _) => "zero".to_string(),
                (1, a, _) => format!("succ {a}"),
                (2, a, b) => format!("max {a} {b}"),
                (3, a, b) => format!("imax {a} {b}"),
                (_, n, _) => format!("param {n}"),
            };
            writeln!(out, "L {} {line}", lv.seen.len() - 1).unwrap();
        }
    }
    let mut ls = Canon::new();
    for l in &t.lists {
        let k = match l {
            ListRow::Nil => None,
            ListRow::Cons(h, r) => Some((lv.map[*h as usize], ls.map[*r as usize])),
        };
        if ls.push(k) {
            let line = match k {
                None => "nil".to_string(),
                Some((h, r)) => format!("cons {h} {r}"),
            };
            writeln!(out, "S {} {line}", ls.seen.len() - 1).unwrap();
        }
    }
    let mut dc = Canon::new();
    for d in &t.decls {
        dc.push(d.key);
    }
    let mut tm: Canon<(u8, u64, u64, u64)> = Canon::new();
    for e in &t.terms {
        let m = |i: &u32| tm.map[*i as usize] as u64;
        let (k, line) = match e {
            TermRow::BVar(j) => ((0, *j as u64, 0, 0), format!("bvar {j}")),
            TermRow::FVar(n) => ((1, *n as u64, 0, 0), format!("leaf fvar {n} 0")),
            TermRow::Sort(l) => {
                let l = lv.map[*l as usize];
                ((2, l as u64, 0, 0), format!("leaf sort {l} 0"))
            }
            TermRow::Const(d, l) => {
                let (d, l) = (dc.map[*d as usize], ls.map[*l as usize]);
                ((3, d as u64, l as u64, 0), format!("leaf const {d} {l}"))
            }
            TermRow::App(a, b) => ((4, m(a), m(b), 0), format!("app {} {}", m(a), m(b))),
            TermRow::Lam(a, b) => ((5, m(a), m(b), 0), format!("lam {} {}", m(a), m(b))),
            TermRow::Pi(a, b) => ((6, m(a), m(b), 0), format!("pi {} {}", m(a), m(b))),
            TermRow::Let(a, v, b) => ((7, m(a), m(v), m(b)), format!("let {} {} {}", m(a), m(v), m(b))),
            TermRow::Lit(n) => ((8, h64(n), 0, 0), format!("leaf lit {} 0", h64(n))),
            TermRow::Str(s) => ((9, h64(s), 0, 0), format!("leaf str {} 0", h64(s))),
            TermRow::Proj(d, n, x) => {
                let d = dc.map[*d as usize];
                ((10, d as u64, *n as u64, m(x)), format!("proj {d} {n} {}", m(x)))
            }
        };
        if tm.push(k) {
            writeln!(out, "T {} {line}", tm.seen.len() - 1).unwrap();
        }
    }
    let mut cx = Canon::new();
    for g in &t.ctxs {
        let k = match g {
            CtxRow::Empty => None,
            CtxRow::Push(q, a) => Some((cx.map[*q as usize], tm.map[*a as usize])),
        };
        if cx.push(k) {
            let line = match k {
                None => "empty".to_string(),
                Some((q, a)) => format!("push {q} {a}"),
            };
            writeln!(out, "C {} {line}", cx.seen.len() - 1).unwrap();
        }
    }
    let mut seen_decl = vec![false; dc.seen.len()];
    for (i, d) in t.decls.iter().enumerate() {
        let c = dc.map[i] as usize;
        if std::mem::replace(&mut seen_decl[c], true) {
            continue;
        }
        let unfold = matches!(d.kind, DeclKind::Definition | DeclKind::Theorem) as u8;
        let v = d.value.map_or(-1, |v| tm.map[v as usize] as i64);
        let (k0, k1) = key_words(&d.key);
        let ty = tm.map[d.ty as usize];
        writeln!(out, "D {c} {unfold} {} {v} {k0} {k1} {} {ty} {}", d.levels, kind_no(d.kind), order(&d.key)).unwrap();
        // What the reduction rules read of a declaration: `I c ctor` with
        // the inductive type's key, parameters and fields; `I c rec` with
        // the arguments before the indices and the indices; one `R c` per
        // recursor rule with the constructor's key, its fields and the
        // right-hand side; `I c induct` and `I c quot`.
        match &d.info {
            Info::None => {}
            Info::Induct { params, indices, ctors, recursive, .. } => {
                let one = ctors.first().map_or((0, 0), key_words);
                writeln!(out, "I {c} induct {params} {indices} {} {} {} {}", ctors.len(), *recursive as u8, one.0, one.1)
                    .unwrap();
            }
            Info::Ctor { induct, params, fields } => {
                let (i0, i1) = key_words(induct);
                writeln!(out, "I {c} ctor {i0} {i1} {params} {fields}").unwrap();
            }
            Info::Rec { params, motives, minors, indices, rules } => {
                writeln!(out, "I {c} rec {} {indices}", params + motives + minors).unwrap();
                for r in rules {
                    let (c0, c1) = key_words(&r.ctor);
                    writeln!(out, "R {c} {c0} {c1} {} {}", r.fields, tm.map[r.rhs as usize]).unwrap();
                }
            }
            Info::Quot(q) => writeln!(out, "I {c} quot {}", *q as u8).unwrap(),
        }
    }
    let (t_, g_, l_, ls_) = (
        |i: u32| tm.map[i as usize],
        |i: u32| cx.map[i as usize],
        |i: u32| lv.map[i as usize],
        |i: u32| ls.map[i as usize],
    );
    for (i, r) in t.rows.iter().enumerate() {
        let (f, x) = match r.fact {
            Fact::Has { g, n, a } => ("has", [g_(g), n, t_(a), 0]),
            Fact::Infer { g, e, t } => ("infer", [g_(g), t_(e), t_(t), 0]),
            Fact::Eq { g, a, b } => ("eq", [g_(g), t_(a), t_(b), 0]),
            Fact::Inst { e, k, a, r } => ("inst", [t_(e), k, t_(a), t_(r)]),
            Fact::LInst { e, ls, r } => ("linst", [t_(e), ls_(ls), t_(r), 0]),
            Fact::LInstL { l, ls, r } => ("linstl", [l_(l), ls_(ls), l_(r), 0]),
            Fact::LEq { l, m } => ("leq", [l_(l), l_(m), 0, 0]),
        };
        let p: Vec<String> = r.premises.iter().map(|p| p.to_string()).collect();
        writeln!(out, "F {i} {:?} {f} {} {} {} {} : {}", r.rule, x[0], x[1], x[2], x[3], p.join(" ")).unwrap();
    }
    let Some(goal) = goal else { return out };
    let (k0, k1) = key_words(own);
    let o = |x: Option<u32>| x.map_or(-1, |v| v as i64);
    let value = goal.value.map_or(-1, |v| tm.map[v as usize] as i64);
    writeln!(
        out,
        "G {k0} {k1} {} {} {} {value} {} {} {} {} {} {} {}",
        kind_no(goal.kind),
        goal.levels,
        tm.map[goal.ty as usize],
        goal.ty_is_type.0,
        goal.ty_is_type.1,
        o(goal.value_has_ty.map(|x| x.0)),
        o(goal.value_has_ty.map(|x| x.1)),
        o(goal.is_prop),
        order(own),
        cx.seen.get(&None).map_or(-1, |&i| i as i64),
    )
    .unwrap();
    out
}

/// Structural 128-bit hashes of every fact of a derivation, independent of
/// row numbering, so facts can be compared across declarations.
fn fact_hashes(t: &Derivation) -> Vec<u128> {
    fn h(parts: &[u128]) -> u128 {
        let mut b = blake3::Hasher::new();
        for p in parts {
            b.update(&p.to_le_bytes());
        }
        u128::from_le_bytes(b.finalize().as_bytes()[..16].try_into().unwrap())
    }
    let mut lv: Vec<u128> = Vec::new();
    for l in &t.levels {
        let x = match l {
            LevelRow::Zero => h(&[0]),
            LevelRow::Succ(a) => h(&[1, lv[*a as usize]]),
            LevelRow::Max(a, b) => h(&[2, lv[*a as usize], lv[*b as usize]]),
            LevelRow::IMax(a, b) => h(&[3, lv[*a as usize], lv[*b as usize]]),
            LevelRow::Param(n) => h(&[4, *n as u128]),
        };
        lv.push(x);
    }
    let mut ls: Vec<u128> = Vec::new();
    for l in &t.lists {
        let x = match l {
            ListRow::Nil => h(&[5]),
            ListRow::Cons(a, r) => h(&[6, lv[*a as usize], ls[*r as usize]]),
        };
        ls.push(x);
    }
    let dc: Vec<u128> = t.decls.iter().map(|d| u128::from_le_bytes(d.key[..16].try_into().unwrap())).collect();
    let mut tm: Vec<u128> = Vec::new();
    for e in &t.terms {
        let m = |i: &u32| tm[*i as usize];
        let x = match e {
            TermRow::BVar(j) => h(&[10, *j as u128]),
            TermRow::FVar(n) => h(&[11, *n as u128]),
            TermRow::Sort(l) => h(&[12, lv[*l as usize]]),
            TermRow::Const(d, l) => h(&[13, dc[*d as usize], ls[*l as usize]]),
            TermRow::App(a, b) => h(&[14, m(a), m(b)]),
            TermRow::Lam(a, b) => h(&[15, m(a), m(b)]),
            TermRow::Pi(a, b) => h(&[16, m(a), m(b)]),
            TermRow::Let(a, v, b) => h(&[17, m(a), m(v), m(b)]),
            TermRow::Lit(n) => h(&[18, h64(n) as u128]),
            TermRow::Str(s) => h(&[19, h64(s) as u128]),
            TermRow::Proj(d, n, x) => h(&[20, dc[*d as usize], *n as u128, m(x)]),
        };
        tm.push(x);
    }
    let mut cx: Vec<u128> = Vec::new();
    for g in &t.ctxs {
        let x = match g {
            CtxRow::Empty => h(&[30]),
            CtxRow::Push(q, a) => h(&[31, cx[*q as usize], tm[*a as usize]]),
        };
        cx.push(x);
    }
    t.rows
        .iter()
        .map(|r| match r.fact {
            Fact::Has { g, n, a } => h(&[40, cx[g as usize], n as u128, tm[a as usize]]),
            Fact::Infer { g, e, t } => h(&[41, cx[g as usize], tm[e as usize], tm[t as usize]]),
            Fact::Eq { g, a, b } => h(&[42, cx[g as usize], tm[a as usize], tm[b as usize]]),
            Fact::Inst { e, k, a, r } => h(&[43, tm[e as usize], k as u128, tm[a as usize], tm[r as usize]]),
            Fact::LInst { e, ls: l, r } => h(&[44, tm[e as usize], ls[l as usize], tm[r as usize]]),
            Fact::LInstL { l, ls: m, r } => h(&[45, lv[l as usize], ls[m as usize], lv[r as usize]]),
            Fact::LEq { l, m } => h(&[46, lv[l as usize], lv[m as usize]]),
        })
        .collect()
}

/// The constants Lean's kernel cites by name: they come first.
const PRIMS: [&str; 25] = [
    "Nat",
    "Nat.zero",
    "Nat.succ",
    "Nat.add",
    "Nat.sub",
    "Nat.mul",
    "Nat.pow",
    "Nat.gcd",
    "Nat.mod",
    "Nat.div",
    "Nat.beq",
    "Nat.ble",
    "Nat.land",
    "Nat.lor",
    "Nat.xor",
    "Nat.shiftLeft",
    "Nat.shiftRight",
    "Bool.true",
    "Bool.false",
    "String",
    "String.mk",
    "Char",
    "Char.ofNat",
    "List.nil",
    "List.cons",
];

/// A dependency order of the declarations reachable from `roots` (after
/// the primitives): each comes after every constant in its type, value,
/// constructors and recursor rules, so a check only cites smaller orders.
/// The same roots give the same order in every process.
fn topo(env: &Env, roots: &[String]) -> HashMap<Key, i64> {
    let by_name: HashMap<String, usize> = env.decls.iter().enumerate().map(|(i, d)| (env.name(d.name), i)).collect();
    let deps = |i: usize| -> Vec<usize> {
        let d = &env.decls[i];
        let mut exprs = vec![d.ty];
        let mut names = std::collections::HashSet::new();
        match &d.kind {
            Kind::Def { value, .. } | Kind::Thm { value } | Kind::Opaque { value } => exprs.push(*value),
            Kind::Induct { ctors, .. } => names.extend(ctors.iter().copied()),
            Kind::Ctor { induct, .. } => {
                names.insert(*induct);
            }
            Kind::Rec { rules, .. } => {
                for (c, _, rhs) in rules {
                    names.insert(*c);
                    exprs.push(*rhs);
                }
            }
            _ => {}
        }
        for e in exprs {
            env.constants(e, &mut std::collections::HashSet::new(), &mut names);
        }
        let mut v: Vec<usize> = names.into_iter().filter_map(|n| env.index_of_name(n)).collect();
        v.sort();
        v
    };
    let mut state = vec![0u8; env.decls.len()];
    let mut order = HashMap::new();
    let mut next = 0i64;
    for r in PRIMS.iter().map(|s| s.to_string()).chain(roots.iter().cloned()) {
        let Some(&root) = by_name.get(&r) else { continue };
        if state[root] != 0 {
            continue;
        }
        let mut stack: Vec<(usize, Vec<usize>)> = vec![(root, deps(root))];
        state[root] = 1;
        while let Some((i, ds)) = stack.last_mut() {
            if let Some(j) = ds.pop() {
                if state[j] == 0 {
                    state[j] = 1;
                    let dj = deps(j);
                    stack.push((j, dj));
                }
            } else {
                let i = *i;
                state[i] = 2;
                order.insert(env.key(env.decls[i].name), next);
                next += 1;
                stack.pop();
            }
        }
    }
    order
}

/// `lean_dump --stats <export> EVERY`: how many facts repeat, within each
/// declaration and across the sample.
fn stats(env: &Arc<Env>, every: usize) {
    let todo: Vec<usize> = (0..env.decls.len())
        .filter(|&i| {
            let d = &env.decls[i];
            !d.unsafe_ && matches!(d.kind, Kind::Axiom | Kind::Def { .. } | Kind::Thm { .. } | Kind::Opaque { .. })
        })
        .step_by(every)
        .collect();
    let (mut total, mut within, mut decls) = (0usize, 0usize, 0usize);
    let mut global: std::collections::HashSet<u128> = std::collections::HashSet::new();
    let mut by_rule: HashMap<String, (usize, usize)> = HashMap::new();
    std::panic::set_hook(Box::new(|_| {}));
    for &i in &todo {
        let d = &env.decls[i];
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut b = Builder::new();
            b.source = Some(std::rc::Rc::new(Shared(env.clone())));
            b.budget = 3_000_000;
            let mut memo = HashMap::new();
            let (kind, _) = env.kind_info(d);
            let ty = b.row_of(&env.term(d.ty, &d.lparams, &mut memo).ok()?).ok()?;
            let value = match d.kind {
                Kind::Def { value, .. } | Kind::Thm { value } | Kind::Opaque { value } => {
                    Some(b.row_of(&env.term(value, &d.lparams, &mut memo).ok()?).ok()?)
                }
                _ => None,
            };
            b.check_decl(kind, d.lparams.len() as u32, ty, value).ok()?;
            Some(b.t)
        }));
        let Ok(Some(t)) = r else { continue };
        decls += 1;
        let hs = fact_hashes(&t);
        let mut local = std::collections::HashSet::new();
        for (row, x) in t.rows.iter().zip(&hs) {
            total += 1;
            let e = by_rule.entry(format!("{:?}", row.rule)).or_default();
            e.0 += 1;
            if local.insert(*x) {
                within += 1;
            }
            if global.insert(*x) {
                e.1 += 1;
            }
        }
    }
    println!(
        "declarations {decls}\nfacts {total}\ndistinct within each declaration {within}\ndistinct across the sample {}",
        global.len()
    );
    let mut v: Vec<_> = by_rule.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1.0));
    for (r, (n, g)) in v.iter().take(25) {
        println!("{r}\t{n}\t{g}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args[1] == "--stats" {
        let text = std::fs::read_to_string(&args[2]).expect("an export");
        let env = Arc::new(Env::parse(&text).expect("a valid export"));
        drop(text);
        return stats(&env, args[3].parse().unwrap());
    }
    let text = std::fs::read_to_string(&args[1]).expect("an export");
    let env = Arc::new(Env::parse(&text).expect("a valid export"));
    drop(text);
    let dir = std::path::Path::new(&args[2]);
    std::fs::create_dir_all(dir).unwrap();
    // Names on the command line, or `--names FILE`, one per line.
    // `--order FILE`: the roots of the dependency order (every process of
    // one cone must pass the same); by default the names dumped.
    let mut rest: Vec<String> = args[3..].to_vec();
    let order_roots: Option<Vec<String>> = (rest.first().is_some_and(|a| a == "--order")).then(|| {
        let f = rest[1].clone();
        rest.drain(..2);
        std::fs::read_to_string(f).unwrap().lines().map(str::to_string).collect()
    });
    let names: Vec<String> = if rest[0] == "--names" {
        std::fs::read_to_string(&rest[1]).unwrap().lines().map(str::to_string).collect()
    } else {
        rest.clone()
    };
    let order = topo(&env, order_roots.as_ref().unwrap_or(&names));
    // `--info FILE` after the names: one file of declaration rows only,
    // every named constant cited with what its reduction rules read.
    if let Some(at) = rest.iter().position(|a| a == "--info") {
        let mut b = Builder::new();
        b.source = Some(std::rc::Rc::new(Shared(env.clone())));
        let mut keys: Vec<(i64, Key)> = order.iter().map(|(k, o)| (*o, *k)).collect();
        keys.sort_unstable();
        for (_, k) in &keys {
            b.cite(k).unwrap_or_else(|e| panic!("a constant of the cone: {e}"));
        }
        let s = dump(&b.t, None, &[0; 32], &|k| order.get(k).copied().unwrap_or(-1));
        std::fs::write(&rest[at + 1], s).unwrap();
        return eprintln!("{} declarations, {} terms", b.t.decls.len(), b.t.terms.len());
    }
    std::panic::set_hook(Box::new(|_| {}));
    for name in &names {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Option<(usize, usize, String)> {
            let d = env.find(name)?;
            let mut b = Builder::new();
            b.source = Some(std::rc::Rc::new(Shared(env.clone())));
            b.budget = 25_000_000;
            let mut memo = HashMap::new();
            let (kind, _) = env.kind_info(d);
            let ty = b.row_of(&env.term(d.ty, &d.lparams, &mut memo).ok()?).ok()?;
            let value = match d.kind {
                Kind::Def { value, .. } | Kind::Thm { value } | Kind::Opaque { value } => {
                    Some(b.row_of(&env.term(value, &d.lparams, &mut memo).ok()?).ok()?)
                }
                _ => None,
            };
            let goal = b.check_decl(kind, d.lparams.len() as u32, ty, value).ok()?;
            cas_lean::Checker::check(&b.t, &goal).ok()?;
            Some((
                b.t.rows.len(),
                b.t.terms.len(),
                dump(&b.t, Some(&goal), &env.key(d.name), &|k| order.get(k).copied().unwrap_or(-1)),
            ))
        }));
        match r {
            Ok(Some((rows, terms, s))) => {
                std::fs::write(dir.join(format!("{name}.rows")), s).unwrap();
                eprintln!("{name}: {rows} facts, {terms} terms");
            }
            _ => eprintln!("{name}: skipped"),
        }
    }
}
