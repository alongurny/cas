//! Writes the derivation of Lean declarations as plain rows, for circuit
//! experiments outside this crate. Terms, levels, lists, contexts and
//! declarations are first made canonical (equal structure, one row), so that
//! equal ids mean equal terms, as the checker's fingerprints do.
//!
//! Usage: `lean_dump <export> <out dir> NAME...`. One file per declaration,
//! `<out dir>/<NAME>.rows`, lines:
//!
//! ```text
//! T <i> bvar <j>
//! T <i> leaf <sub> <a> <b>      sub: sort l | const d ls | fvar n | lit h | str h
//! T <i> app|lam|pi <a> <b>
//! T <i> let <a> <v> <b>
//! T <i> proj <d> <n> <x>
//! D <i> <unfoldable 0|1> <levels> <value or -1>
//! F <i> <Rule> <fact> <x1> <x2> <x3> <x4> : <premises>
//! ```
//!
//! Facts: has (g n a), infer (g e t), eq (g a b), inst (e k a r), linst
//! (e ls r), linstl (l ls r), leq (l m); missing fields are 0. Children come
//! before their parents, and premises before the facts that use them.

use cas_lean::derivation::{CtxRow, DeclKind, Derivation, Fact, Key, LevelRow, ListRow, TermRow};
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

fn dump(t: &Derivation) -> String {
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
        lv.push(k);
    }
    let mut ls = Canon::new();
    for l in &t.lists {
        let k = match l {
            ListRow::Nil => None,
            ListRow::Cons(h, r) => Some((lv.map[*h as usize], ls.map[*r as usize])),
        };
        ls.push(k);
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
        cx.push(k);
    }
    let mut seen_decl = vec![false; dc.seen.len()];
    for (i, d) in t.decls.iter().enumerate() {
        let c = dc.map[i] as usize;
        if std::mem::replace(&mut seen_decl[c], true) {
            continue;
        }
        let unfold = matches!(d.kind, DeclKind::Definition | DeclKind::Theorem) as u8;
        let v = d.value.map_or(-1, |v| tm.map[v as usize] as i64);
        writeln!(out, "D {c} {unfold} {} {v}", d.levels).unwrap();
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
    println!("declarations {decls}\nfacts {total}\ndistinct within each declaration {within}\ndistinct across the sample {}", global.len());
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
    let names: Vec<String> = if args[3] == "--names" {
        std::fs::read_to_string(&args[4]).unwrap().lines().map(str::to_string).collect()
    } else {
        args[3..].to_vec()
    };
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
            Some((b.t.rows.len(), b.t.terms.len(), dump(&b.t)))
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
