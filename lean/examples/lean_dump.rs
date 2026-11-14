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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let text = std::fs::read_to_string(&args[1]).expect("an export");
    let env = Arc::new(Env::parse(&text).expect("a valid export"));
    drop(text);
    let dir = std::path::Path::new(&args[2]);
    std::fs::create_dir_all(dir).unwrap();
    for name in &args[3..] {
        let d = env.find(name).unwrap_or_else(|| panic!("no {name}"));
        let mut b = Builder::new();
        b.source = Some(std::rc::Rc::new(Shared(env.clone())));
        b.budget = 50_000_000;
        let mut memo = HashMap::new();
        let (kind, _) = env.kind_info(d);
        let ty = env.term(d.ty, &d.lparams, &mut memo).unwrap();
        let ty = b.row_of(&ty).unwrap();
        let value = match d.kind {
            Kind::Def { value, .. } | Kind::Thm { value } | Kind::Opaque { value } => {
                Some(b.row_of(&env.term(value, &d.lparams, &mut memo).unwrap()).unwrap())
            }
            _ => None,
        };
        let goal = b.check_decl(kind, d.lparams.len() as u32, ty, value).unwrap();
        cas_lean::Checker::check(&b.t, &goal).unwrap();
        let s = dump(&b.t);
        std::fs::write(dir.join(format!("{name}.rows")), s).unwrap();
        eprintln!("{name}: {} facts, {} terms", b.t.rows.len(), b.t.terms.len());
    }
}
